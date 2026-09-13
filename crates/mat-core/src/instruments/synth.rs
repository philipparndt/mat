//! Subtractive synth voice: unison PolyBLEP oscillators, noise, drive,
//! ZDF state variable filter with envelope and key tracking, vibrato.

use std::f32::consts::TAU;

use crate::arrange::{Sweep, TimedNote};
use crate::dsp::biquad::Biquad;
use crate::dsp::envelope::Envelope;
use crate::dsp::filter::Svf;
use crate::dsp::osc::Oscillator;
use crate::dsp::{Rng, StereoClip, midi_to_hz, pan_gains};
use crate::model::{FilterMode, LfoTarget, SynthDef, Waveform};

/// Headroom per voice so that chords do not clip before the mix.
const VOICE_GAIN: f32 = 0.22;

struct UnisonVoice {
    osc: Oscillator,
    wave: Waveform,
    ratio: f32,
    gain_l: f32,
    gain_r: f32,
    highpass: Option<Biquad>,
}

/// Detune offsets of the seven saws, relative to the note frequency.
const SUPERSAW_OFFSETS: [f32; 7] = [-0.110_023_13, -0.062_884_39, -0.019_523_56, 0.0, 0.019_912_21, 0.062_165_38, 0.107_452_42];
/// Stereo positions, alternating so neighbours in pitch sit on opposite sides.
const SUPERSAW_PAN: [f32; 7] = [-1.0, 0.67, -0.33, 0.0, 0.33, -0.67, 1.0];

/// Maps the detune control (0..1) to the detune depth, after A. Szabo's
/// analysis of the Roland JP-8000 ("How to Emulate the Super Saw", 2010).
fn supersaw_detune_curve(x: f32) -> f32 {
    let x = x as f64;
    let coefs = [
        10028.731_289_163_4,
        -50818.865_204_592_4,
        111_363.480_872_936_8,
        -138_150.676_108_054_8,
        106_649.667_915_829_2,
        -53046.964_275_187_5,
        17019.951_858_008_0,
        -3425.083_659_131_8,
        404.270_393_838_8,
        -24.187_882_439_1,
        0.671_741_763_4,
        0.003_011_559_6,
    ];
    coefs.iter().fold(0.0, |acc, c| acc * x + c) as f32
}

/// Value of the latest started sweep of `param` at absolute time `t`, if any.
fn sweep_value(sweeps: &[Sweep], param: &str, t: f64) -> Option<f32> {
    let mut value = None;
    for s in sweeps.iter().filter(|s| s.param == param && t >= s.start) {
        let x = if s.end > s.start { ((t - s.start) / (s.end - s.start)).clamp(0.0, 1.0) as f32 } else { 1.0 };
        // Frequencies interpolate logarithmically, everything else linearly.
        value = Some(if param == "cutoff" && s.from > 0.0 && s.to > 0.0 { s.from * (s.to / s.from).powf(x) } else { s.from + (s.to - s.from) * x });
    }
    value
}

pub fn render_note(def: &SynthDef, note: &TimedNote, sweeps: &[Sweep], sample_rate: f32, seed: u64) -> StereoClip {
    let mut rng = Rng::new(seed);
    let offset = (note.start * sample_rate as f64).round() as usize;
    let note_off = ((note.duration * sample_rate as f64).round() as usize).max(1);
    let max_len = note_off + ((def.amp.release * 3.0 + 0.05) * sample_rate) as usize;

    let mut voices = Vec::new();
    let drift = if def.drift_cents > 0.0 { rng.bipolar() * def.drift_cents } else { 0.0 };
    let base_hz = midi_to_hz(note.midi + drift / 100.0);
    let lfos: Vec<(crate::model::Lfo, f32)> = def.lfos.iter().map(|l| (*l, l.phase.unwrap_or_else(|| rng.unit()))).collect();
    for osc in &def.oscillators {
        if let Some(ss) = osc.supersaw {
            let depth = supersaw_detune_curve(ss.detune);
            let m = ss.mix;
            let side = -0.737_64 * m * m + 1.2841 * m + 0.044_372;
            let center = -0.553_66 * m + 0.997_85;
            let norm = osc.level / (center * center + 6.0 * side * side).sqrt();
            let semis = osc.octave as f32 * 12.0 + osc.semitones + osc.detune_cents / 100.0;
            let base_ratio = 2f32.powf(semis / 12.0);
            for (i, offset) in SUPERSAW_OFFSETS.iter().enumerate() {
                let gain = if i == 3 { center } else { side } * norm;
                let (gl, gr) = pan_gains(SUPERSAW_PAN[i] * osc.width);
                voices.push(UnisonVoice {
                    osc: Oscillator::new(rng.unit()),
                    wave: Waveform::Saw,
                    ratio: base_ratio * (1.0 + offset * depth),
                    gain_l: gl * gain,
                    gain_r: gr * gain,
                    // Removes the aliasing-prone energy below the fundamental.
                    highpass: Some(Biquad::highpass(base_hz * base_ratio, std::f32::consts::FRAC_1_SQRT_2, sample_rate)),
                });
            }
            continue;
        }
        let n = osc.voices.max(1);
        let level = osc.level / (n as f32).sqrt();
        for v in 0..n {
            let pos = if n > 1 { v as f32 / (n - 1) as f32 * 2.0 - 1.0 } else { 0.0 };
            let cents = osc.detune_cents + pos * osc.spread_cents;
            let semis = osc.octave as f32 * 12.0 + osc.semitones + cents / 100.0;
            let (gl, gr) = pan_gains(pos * osc.width);
            voices.push(UnisonVoice {
                osc: Oscillator::new(rng.unit()),
                wave: osc.wave,
                ratio: 2f32.powf(semis / 12.0),
                gain_l: gl * level,
                gain_r: gr * level,
                highpass: None,
            });
        }
    }

    let mut amp_env = Envelope::new(&def.amp, sample_rate);
    let mut filter_env = Envelope::new(&def.filter_env, sample_rate);
    // One stage per filter: key-tracked base cutoff, envelope depth, drive, and a stereo pair of SVFs.
    struct Stage {
        mode: FilterMode,
        key_factor: f32,
        base_cutoff: f32,
        resonance: f32,
        env_depth: f32,
        drive: f32,
        drive_norm: f32,
        svf: [Svf; 2],
    }
    let mut stages: Vec<Stage> = def
        .filters
        .iter()
        .filter(|f| f.mode != FilterMode::Off)
        .map(|f| {
            let key_factor = 2f32.powf(f.keytrack * (note.midi - 60.0) / 12.0);
            let drive = 1.0 + f.drive * 6.0;
            Stage {
                mode: f.mode,
                key_factor,
                base_cutoff: f.cutoff_hz * key_factor,
                resonance: f.resonance,
                env_depth: f.env_octaves * (0.5 + 0.5 * note.velocity),
                drive: if f.drive > 0.0 { drive } else { 0.0 },
                drive_norm: 1.0 / drive.tanh(),
                svf: [Svf::default(), Svf::default()],
            }
        })
        .collect();
    let sweeps_cutoff = sweeps.iter().any(|s| s.param == "cutoff");
    let sweeps_res = sweeps.iter().any(|s| s.param == "res");
    let velocity_gain = note.velocity.powf(1.5) * VOICE_GAIN;

    let vib = def.vibrato;
    let vib_active = vib.rate_hz > 0.0 && vib.depth_cents > 0.0;
    let nyquist_dt = 0.49;

    let mut left = Vec::with_capacity(max_len);
    let mut right = Vec::with_capacity(max_len);
    for i in 0..max_len {
        if i == note_off {
            amp_env.release();
            filter_env.release();
        }
        let t = i as f32 / sample_rate;

        let mut hz = base_hz;
        let (mut lfo_filter, mut lfo_pitch, mut lfo_pan, mut lfo_amp, mut lfo_width) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for (l, phase) in &lfos {
            let fade = if l.fade_in > 0.0 { (t / l.fade_in).min(1.0) } else { 1.0 };
            let v = (TAU * (l.rate_hz * t + phase)).sin() * l.depth * fade;
            match l.target {
                LfoTarget::Filter => lfo_filter += v,
                LfoTarget::Pitch => lfo_pitch += v,
                LfoTarget::Pan => lfo_pan += v,
                LfoTarget::Amp => lfo_amp += v,
                LfoTarget::Width => lfo_width += v,
            }
        }
        if lfo_pitch != 0.0 {
            hz *= 2f32.powf(lfo_pitch / 1200.0);
        }
        if vib_active && t > vib.delay {
            let fade = ((t - vib.delay) / 0.25).min(1.0);
            let cents = vib.depth_cents * fade * (TAU * vib.rate_hz * (t - vib.delay)).sin();
            hz *= 2f32.powf(cents / 1200.0);
        }

        let (mut l, mut r) = (0.0f32, 0.0f32);
        for v in &mut voices {
            let dt = (hz * v.ratio / sample_rate).min(nyquist_dt);
            let mut s = v.osc.next(v.wave, dt);
            if let Some(hp) = &mut v.highpass {
                s = hp.process(s);
            }
            l += s * v.gain_l;
            r += s * v.gain_r;
        }
        if def.noise > 0.0 {
            l += rng.bipolar() * def.noise;
            r += rng.bipolar() * def.noise;
        }

        let fe = filter_env.next();
        let abs_t = note.start + t as f64;
        // Sweeps move the first stage; the others keep their own settings.
        for (si, st) in stages.iter_mut().enumerate() {
            if st.drive > 0.0 {
                l = (l * st.drive).tanh() * st.drive_norm;
                r = (r * st.drive).tanh() * st.drive_norm;
            }
            let base = if si == 0 && sweeps_cutoff { sweep_value(sweeps, "cutoff", abs_t).map_or(st.base_cutoff, |v| v * st.key_factor) } else { st.base_cutoff };
            let res = if si == 0 && sweeps_res { sweep_value(sweeps, "res", abs_t).unwrap_or(st.resonance) } else { st.resonance };
            let cutoff = base * 2f32.powf(st.env_depth * fe + lfo_filter);
            for f in &mut st.svf {
                f.set(cutoff, res, sample_rate);
            }
            l = st.svf[0].process(l, st.mode);
            r = st.svf[1].process(r, st.mode);
        }

        let a = amp_env.next() * velocity_gain * (1.0 - lfo_amp.clamp(-1.0, 1.0) * 0.5 - lfo_amp.abs() * 0.0);
        if lfo_pan != 0.0 || lfo_width != 0.0 {
            let (pl, pr) = pan_gains(lfo_pan.clamp(-1.0, 1.0));
            let mid = (l + r) * 0.5;
            let side = (l - r) * 0.5 * (1.0 + lfo_width);
            l = (mid + side) * pl;
            r = (mid - side) * pr;
        }
        left.push(l * a);
        right.push(r * a);
        if amp_env.is_done() {
            break;
        }
    }
    StereoClip { offset, left, right }
}
