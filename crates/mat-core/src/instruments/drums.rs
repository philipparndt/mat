//! Synthesized drum kit (analog drum machine style).

use std::f32::consts::TAU;

use crate::arrange::TimedNote;
use crate::dsp::filter::{OnePole, Svf};
use crate::dsp::osc::Oscillator;
use crate::dsp::{Rng, StereoClip, db_to_gain};
use crate::model::{DrumKind, DrumVoice, FilterMode, Pitch, Waveform};

/// Calibrates the kit so a hit sits at a similar level to a synth voice.
const KIT_GAIN: f32 = 0.35;

/// Renders a drum hit. `choke_at` (seconds after the hit) cuts open hats.
pub fn render_hit(note: &TimedNote, voice: &DrumVoice, choke_at: Option<f64>, sample_rate: f32, seed: u64) -> StereoClip {
    let Pitch::Drum(kind) = note.pitch else { unreachable!("drum kit received a pitched note") };
    let sr = sample_rate;
    let tune = 2f32.powf(voice.tune / 12.0);
    let mut rng = Rng::new(seed);
    let gain = KIT_GAIN * db_to_gain(voice.gain_db) * (0.25 + 0.75 * note.velocity);

    let mono = match kind {
        DrumKind::Kick => kick(sr, tune, voice.decay, &mut rng),
        DrumKind::Snare => snare(sr, tune, voice.decay, &mut rng),
        DrumKind::Clap => clap(sr, tune, voice.decay, &mut rng),
        DrumKind::Hat => hat(sr, tune, 0.07 * voice.decay, &mut rng),
        DrumKind::OpenHat => hat(sr, tune, 0.5 * voice.decay, &mut rng),
        DrumKind::Tom => tom(sr, tune, voice.decay, &mut rng),
        DrumKind::Rim => rim(sr, tune, voice.decay, &mut rng),
        DrumKind::Crash => cymbal(sr, tune * 0.85, 1.8 * voice.decay, 5_500.0, &mut rng),
        DrumKind::Ride => cymbal(sr, tune * 1.1, 1.2 * voice.decay, 8_000.0, &mut rng),
        // Scratch moves need a scratch instrument; a drum kit stays silent on them.
        _ => Vec::new(),
    };

    let choke = choke_at.map(|c| (c * sr as f64) as usize);
    let fade = (0.004 * sr) as usize;
    let mut left = Vec::with_capacity(mono.len());
    for (i, s) in mono.into_iter().enumerate() {
        let mut g = gain;
        if let Some(c) = choke {
            if i >= c + fade {
                break;
            }
            if i >= c {
                g *= 1.0 - (i - c) as f32 / fade as f32;
            }
        }
        left.push(s * g);
    }
    StereoClip { offset: (note.start * sr as f64).round() as usize, right: left.clone(), left }
}

fn exp_decay(t: f32, seconds: f32) -> f32 {
    (-t * 6.9 / seconds).exp()
}

fn kick(sr: f32, tune: f32, decay: f32, rng: &mut Rng) -> Vec<f32> {
    let length = 0.55 * decay;
    let n = ((length + 0.02) * sr) as usize;
    let mut phase = 0.0f32;
    let mut click_hp = OnePole::new(1500.0, sr);
    let norm = 1.0 / 1.8f32.tanh();
    (0..n)
        .map(|i| {
            let t = i as f32 / sr;
            let hz = tune * (46.0 + 110.0 * (-t / 0.035).exp() + 280.0 * (-t / 0.004).exp());
            phase = (phase + hz / sr) % 1.0;
            let shape = (1.0 - t / length).max(0.0);
            let body = (phase * TAU).sin() * (-t * 2.5 / length).exp() * shape;
            let click = click_hp.highpass(rng.bipolar()) * (-t / 0.002).exp() * 0.35;
            let attack = (t / 0.0008).min(1.0);
            ((body + click) * 1.8 * attack).tanh() * norm * 0.9
        })
        .collect()
}

fn snare(sr: f32, tune: f32, decay: f32, rng: &mut Rng) -> Vec<f32> {
    let body_decay = 0.16 * decay;
    let noise_decay = 0.24 * decay;
    let n = ((noise_decay + 0.02) * sr) as usize;
    let (mut p1, mut p2) = (0.0f32, 0.0f32);
    let mut hp = Svf::new(1400.0, 0.1, sr);
    let mut lp = Svf::new(9000.0, 0.0, sr);
    (0..n)
        .map(|i| {
            let t = i as f32 / sr;
            let bend = 1.0 + 0.25 * (-t / 0.012).exp();
            p1 = (p1 + 180.0 * tune * bend / sr) % 1.0;
            p2 = (p2 + 330.0 * tune * bend / sr) % 1.0;
            let body = ((p1 * TAU).sin() * 0.65 + (p2 * TAU).sin() * 0.35) * exp_decay(t, body_decay);
            let noise = lp.process(hp.process(rng.bipolar(), FilterMode::Highpass), FilterMode::Lowpass);
            let attack = (t / 0.0005).min(1.0);
            (body * 0.55 + noise * exp_decay(t, noise_decay) * 0.75) * attack
        })
        .collect()
}

fn clap(sr: f32, tune: f32, decay: f32, rng: &mut Rng) -> Vec<f32> {
    let tail = 0.28 * decay;
    let n = ((0.03 + tail) * sr) as usize;
    let mut bp = Svf::new(1150.0 * tune, 0.45, sr);
    let mut hp = OnePole::new(400.0, sr);
    (0..n)
        .map(|i| {
            let t = i as f32 / sr;
            let bursts = [0.0f32, 0.009, 0.019]
                .iter()
                .filter(|&&t0| t >= t0)
                .map(|&t0| (-(t - t0) / 0.0035).exp())
                .fold(0.0, f32::max);
            let env = if t >= 0.028 { bursts.max(0.7 * exp_decay(t - 0.028, tail)) } else { bursts };
            hp.highpass(bp.process(rng.bipolar(), FilterMode::Bandpass)) * env * 2.4
        })
        .collect()
}

fn hat(sr: f32, tune: f32, decay: f32, rng: &mut Rng) -> Vec<f32> {
    // The six detuned square oscillators of a classic analog cymbal circuit.
    const FREQS: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];
    let n = ((decay * 1.1 + 0.01) * sr) as usize;
    let mut oscs: Vec<Oscillator> = FREQS.iter().map(|_| Oscillator::new(rng.unit())).collect();
    let mut bp = Svf::new(10_000.0 * tune, 0.25, sr);
    let mut hp = Svf::new(7_500.0 * tune, 0.05, sr);
    (0..n)
        .map(|i| {
            let t = i as f32 / sr;
            let metal: f32 = oscs
                .iter_mut()
                .zip(FREQS)
                .map(|(o, f)| o.next(Waveform::Square, (f * tune * 1.6 / sr).min(0.49)))
                .sum::<f32>()
                / 6.0;
            let s = metal + rng.bipolar() * 0.25;
            let s = hp.process(bp.process(s, FilterMode::Bandpass), FilterMode::Highpass);
            let attack = (t / 0.0004).min(1.0);
            s * exp_decay(t, decay) * attack * 3.2
        })
        .collect()
}

/// Crash and ride: the metallic oscillator bank with more noise, a lower
/// band and a long decay.
fn cymbal(sr: f32, tune: f32, decay: f32, band: f32, rng: &mut Rng) -> Vec<f32> {
    const FREQS: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];
    let n = ((decay * 1.1 + 0.01) * sr) as usize;
    let mut oscs: Vec<Oscillator> = FREQS.iter().map(|_| Oscillator::new(rng.unit())).collect();
    let mut bp = Svf::new(band * tune, 0.15, sr);
    let mut hp = Svf::new(3_000.0, 0.05, sr);
    (0..n)
        .map(|i| {
            let t = i as f32 / sr;
            let metal: f32 = oscs.iter_mut().zip(FREQS).map(|(o, f)| o.next(Waveform::Square, (f * tune * 2.3 / sr).min(0.49))).sum::<f32>() / 6.0;
            let s = metal * 0.7 + rng.bipolar() * 0.6;
            let s = hp.process(bp.process(s, FilterMode::Bandpass), FilterMode::Highpass);
            let body = exp_decay(t, decay);
            let splash = 0.8 * exp_decay(t, 0.08);
            s * (body + splash) * (t / 0.0015).min(1.0) * 2.2
        })
        .collect()
}

fn tom(sr: f32, tune: f32, decay: f32, rng: &mut Rng) -> Vec<f32> {
    let length = 0.45 * decay;
    let n = ((length + 0.02) * sr) as usize;
    let mut phase = 0.0f32;
    let mut bp = Svf::new(700.0, 0.2, sr);
    (0..n)
        .map(|i| {
            let t = i as f32 / sr;
            let hz = 105.0 * tune * (1.0 + 0.45 * (-t / 0.06).exp());
            phase = (phase + hz / sr) % 1.0;
            let body = (phase * TAU).sin() * exp_decay(t, length);
            let noise = bp.process(rng.bipolar(), FilterMode::Bandpass) * exp_decay(t, 0.03);
            (body * 0.85 + noise * 0.3) * (t / 0.0008).min(1.0)
        })
        .collect()
}

fn rim(sr: f32, tune: f32, decay: f32, rng: &mut Rng) -> Vec<f32> {
    let length = 0.05 * decay;
    let n = ((length + 0.01) * sr) as usize;
    let (mut p1, mut p2) = (0.0f32, 0.0f32);
    let mut hp = OnePole::new(2000.0, sr);
    (0..n)
        .map(|i| {
            let t = i as f32 / sr;
            p1 = (p1 + 1700.0 * tune / sr) % 1.0;
            p2 = (p2 + 520.0 * tune / sr) % 1.0;
            let tone = ((p1 * TAU).sin() * 0.6 + (p2 * TAU).sin() * 0.4) * exp_decay(t, length);
            let click = hp.highpass(rng.bipolar()) * exp_decay(t, 0.008);
            (tone + click * 0.5) * 0.8
        })
        .collect()
}
