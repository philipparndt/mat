//! Subtractive synth voice: unison PolyBLEP oscillators that wander like
//! analog ones, noise, drive, a ZDF state variable filter or a saturating
//! 24 dB ladder with envelope and key tracking, vibrato, legato phrases.
//!
//! A voice runs at twice the sample rate and is brought down through a
//! linear-phase halfband filter, so what the oscillators, the FM, the drive and
//! the ladder put above the band is removed instead of folding back into it.

use std::f32::consts::TAU;

use crate::arrange::{Sweep, TimedNote};
use crate::dsp::biquad::Biquad;
use crate::dsp::envelope::Envelope;
use crate::dsp::filter::{Ladder, Svf};
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
    pw: f32,
    highpass: Option<Biquad>,
    /// Phase modulation: (index in radians, ratio, envelope amount, modulator phase).
    fm: Option<(f32, f32, f32, f32)>,
    /// Slow pitch wander: two sines, (rate in Hz, phase) each, and the
    /// frequency factor they add up to now.
    wander: [(f32, f32); 2],
    wander_now: f32,
}

/// A voice is rendered at this many times the sample rate.
const OVERSAMPLE: usize = 2;
/// LFOs, pitch, sweeps and filter coefficients move once per this many
/// oversampled samples (a sixth of a millisecond); envelopes and audio every one.
const CONTROL: usize = 16;
/// How long one note of a legato phrase takes to hand over to the next.
const LEGATO_FADE: f32 = 0.012;
/// Notes closer together than this are one legato phrase, so that `humanize`
/// does not break it.
const LEGATO_GAP: f64 = 0.03;

/// What a note needs to know about its neighbours.
#[derive(Debug, Clone, Copy)]
pub struct NoteContext {
    /// The previous note's pitch, for portamento.
    pub glide_from: Option<f32>,
    /// When and with which seed the legato phrase this note belongs to began;
    /// the note's own start and seed if it begins one.
    pub phrase_start: f64,
    pub phrase_seed: u64,
    /// When the next note of the phrase takes over, if one does.
    pub tied_to: Option<f64>,
}

impl NoteContext {
    pub fn alone(note: &TimedNote) -> Self {
        NoteContext { glide_from: None, phrase_start: note.start, phrase_seed: note.seed, tied_to: None }
    }

    /// The context of `notes[i]` on a track of `def`. Notes are in order of time.
    pub fn of(def: &SynthDef, notes: &[TimedNote], i: usize) -> Self {
        let note = &notes[i];
        // The last note that started before this one.
        let previous = |i: usize| notes[..i].iter().rposition(|p| p.start < notes[i].start - 1e-6);
        let tied = |from: usize, to: usize| def.legato && notes[to].start - (notes[from].start + notes[from].duration) < LEGATO_GAP;
        let mut ctx = NoteContext::alone(note);
        if def.glide > 0.0 {
            ctx.glide_from = previous(i).map(|p| notes[p].midi);
        }
        if def.legato {
            let mut first = i;
            while let Some(p) = previous(first).filter(|&p| tied(p, first)) {
                first = p;
            }
            ctx.phrase_start = notes[first].start;
            ctx.phrase_seed = notes[first].seed;
            ctx.tied_to = notes[i + 1..].iter().position(|q| q.start > note.start + 1e-6).map(|k| i + 1 + k).filter(|&n| tied(i, n)).map(|n| notes[n].start);
        }
        ctx
    }
}

/// Halves the sample rate through a linear-phase halfband lowpass, without
/// delay: the whole note is there, so the filter can look both ways.
pub(crate) fn halve(x: &[f32]) -> Vec<f32> {
    const HALF: usize = 23;
    // Blackman-windowed sinc at a quarter of the rate; every even tap but the
    // middle one is zero.
    let mut taps = [0.0f32; HALF + 1];
    let mut sum = 0.0;
    for (k, tap) in taps.iter_mut().enumerate() {
        let w = 0.5 + 0.5 * k as f32 / HALF as f32;
        let window = 0.42 - 0.5 * (TAU * w).cos() + 0.08 * (2.0 * TAU * w).cos();
        let sinc = if k == 0 { 0.5 } else { (std::f32::consts::FRAC_PI_2 * k as f32).sin() / (std::f32::consts::PI * k as f32) };
        *tap = sinc * window;
        sum += if k == 0 { *tap } else { 2.0 * *tap };
    }
    taps.iter_mut().for_each(|t| *t /= sum);
    let at = |i: isize| if i >= 0 && (i as usize) < x.len() { x[i as usize] } else { 0.0 };
    (0..x.len().div_ceil(2))
        .map(|n| {
            let c = 2 * n as isize;
            let mut y = taps[0] * at(c);
            for k in (1..=HALF).step_by(2) {
                y += taps[k] * (at(c - k as isize) + at(c + k as isize));
            }
            y
        })
        .collect()
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
    let mut note = note.clone();
    note.seed = seed;
    render_note_in(def, &note, &NoteContext::alone(&note), sweeps, sample_rate)
}

/// `glide_from` is the previous note's pitch for portamento.
pub fn render_note_from(def: &SynthDef, note: &TimedNote, glide_from: Option<f32>, sweeps: &[Sweep], sample_rate: f32, seed: u64) -> StereoClip {
    let mut note = note.clone();
    note.seed = seed;
    render_note_in(def, &note, &NoteContext { glide_from, ..NoteContext::alone(&note) }, sweeps, sample_rate)
}

pub fn render_note_in(def: &SynthDef, note: &TimedNote, ctx: &NoteContext, sweeps: &[Sweep], output_rate: f32) -> StereoClip {
    let sample_rate = output_rate * OVERSAMPLE as f32;
    // What a phrase shares — the drift and the LFO phases — comes from the
    // note that began it; the oscillators' phases and the noise are the note's own.
    let mut phrase_rng = Rng::new(ctx.phrase_seed ^ 0x5eed_1e9a_70);
    let mut rng = Rng::new(note.seed);
    let offset = (note.start * output_rate as f64).round() as usize;
    let into_phrase = (note.start - ctx.phrase_start).max(0.0) as f32;
    let tied_from = into_phrase > 0.0;
    // A note tied to the next one is held until that takes over, then faded.
    let held = ctx.tied_to.map_or(note.duration, |next| next - note.start);
    let note_off = ((held * sample_rate as f64).round() as usize).max(1);
    let fade_len = ((LEGATO_FADE * sample_rate) as usize).max(1);
    // Amp decay and release sweeps take their value at the start of the note.
    let mut amp = def.amp;
    if let Some(v) = sweep_value(sweeps, "decay", note.start) {
        amp.decay = v.max(0.001);
    }
    if let Some(v) = sweep_value(sweeps, "release", note.start) {
        amp.release = v.max(0.005);
    }
    let max_len = if ctx.tied_to.is_some() { note_off + fade_len } else { note_off + ((amp.release * 3.0 + 0.05) * sample_rate) as usize };

    let mut voices = Vec::new();
    let drift = if def.drift_cents > 0.0 { phrase_rng.bipolar() * def.drift_cents } else { 0.0 };
    let base_hz = midi_to_hz(note.midi + drift / 100.0);
    let lfos: Vec<(crate::model::Lfo, f32)> = def.lfos.iter().map(|l| (*l, l.phase.unwrap_or_else(|| phrase_rng.unit()))).collect();
    let wander = |rng: &mut Rng| [(0.08 + 0.3 * rng.unit(), rng.unit()), (0.5 + 0.8 * rng.unit(), rng.unit())];
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
                    pw: 0.5,
                    // Removes the aliasing-prone energy below the fundamental.
                    highpass: Some(Biquad::highpass(base_hz * base_ratio, std::f32::consts::FRAC_1_SQRT_2, sample_rate)),
                    fm: None,
                    wander: wander(&mut rng),
                    wander_now: 1.0,
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
                pw: osc.pw,
                highpass: None,
                fm: (osc.fm_index > 0.0 || osc.fm_env > 0.0).then_some((osc.fm_index, osc.fm_ratio, osc.fm_env, rng.unit())),
                wander: wander(&mut rng),
                wander_now: 1.0,
            });
        }
    }
    // Oscillators that drift also wander: each by up to a third of the drift,
    // slowly, so two of them never beat the same way twice.
    let wander_depth = def.drift_cents * 0.35 * std::f32::consts::LN_2 / 1200.0;

    // A note inside a phrase picks the envelopes up where the phrase has them.
    let (mut amp_env, mut filter_env) = if tied_from {
        (Envelope::held_for(&amp, into_phrase, sample_rate), Envelope::held_for(&def.filter_env, into_phrase, sample_rate))
    } else {
        (Envelope::new(&amp, sample_rate), Envelope::new(&def.filter_env, sample_rate))
    };
    // One stage per filter: key-tracked base cutoff, envelope depth, drive, and a stereo pair of filters.
    enum Pair {
        Svf([Svf; 2]),
        Ladder([Ladder; 2]),
    }
    struct Stage {
        mode: FilterMode,
        key_factor: f32,
        base_cutoff: f32,
        resonance: f32,
        env_depth: f32,
        drive: f32,
        drive_norm: f32,
        pair: Pair,
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
                pair: if f.slope >= 24 && f.mode == FilterMode::Lowpass { Pair::Ladder([Ladder::default(), Ladder::default()]) } else { Pair::Svf([Svf::default(), Svf::default()]) },
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
    let mut hz = base_hz;
    let (mut lfo_pan, mut lfo_amp, mut lfo_width, mut lfo_pw, mut lfo_fm) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for i in 0..max_len {
        if i == note_off && ctx.tied_to.is_none() {
            amp_env.release();
            filter_env.release();
        }
        if i % CONTROL == 0 {
            let t = i as f32 / sample_rate;
            // The vibrato and the LFOs' fade belong to the phrase, not the note.
            let phrase_t = t + into_phrase;
            hz = base_hz;
            // Portamento from the previous note, and the pitch envelope.
            if let (Some(from), true) = (ctx.glide_from, def.glide > 0.0) {
                let semis = (from - note.midi) * (-3.0 * t / def.glide).exp();
                hz *= 2f32.powf(semis / 12.0);
            }
            if let Some((depth, decay)) = def.pitch_env.filter(|_| !tied_from) {
                hz *= 2f32.powf(depth * (-4.6 * t / decay.max(0.001)).exp() / 12.0);
            }
            let (mut lfo_filter, mut lfo_pitch) = (0.0f32, 0.0f32);
            (lfo_pan, lfo_amp, lfo_width, lfo_pw, lfo_fm) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for (l, phase) in &lfos {
                let fade = if l.fade_in > 0.0 { (phrase_t / l.fade_in).min(1.0) } else { 1.0 };
                let v = (TAU * (l.rate_hz * phrase_t + phase)).sin() * l.depth * fade;
                match l.target {
                    LfoTarget::Filter => lfo_filter += v,
                    LfoTarget::Pitch => lfo_pitch += v,
                    LfoTarget::Pan => lfo_pan += v,
                    LfoTarget::Amp => lfo_amp += v,
                    LfoTarget::Width => lfo_width += v,
                    LfoTarget::Pw => lfo_pw += v,
                    LfoTarget::Fm => lfo_fm += v,
                }
            }
            if lfo_pitch != 0.0 {
                hz *= 2f32.powf(lfo_pitch / 1200.0);
            }
            if vib_active && phrase_t > vib.delay {
                let fade = ((phrase_t - vib.delay) / 0.25).min(1.0);
                let cents = vib.depth_cents * fade * (TAU * vib.rate_hz * (phrase_t - vib.delay)).sin();
                hz *= 2f32.powf(cents / 1200.0);
            }
            if wander_depth > 0.0 {
                for v in &mut voices {
                    let [(r1, p1), (r2, p2)] = v.wander;
                    v.wander_now = 1.0 + wander_depth * (0.7 * (TAU * (r1 * phrase_t + p1)).sin() + 0.3 * (TAU * (r2 * phrase_t + p2)).sin());
                }
            }

            let fe = filter_env.peek();
            let abs_t = note.start + t as f64;
            // Sweeps move the first stage; the others keep their own settings.
            for (si, st) in stages.iter_mut().enumerate() {
                let base = if si == 0 && sweeps_cutoff { sweep_value(sweeps, "cutoff", abs_t).map_or(st.base_cutoff, |v| v * st.key_factor) } else { st.base_cutoff };
                let res = if si == 0 && sweeps_res { sweep_value(sweeps, "res", abs_t).unwrap_or(st.resonance) } else { st.resonance };
                let cutoff = base * 2f32.powf(st.env_depth * fe + lfo_filter);
                match &mut st.pair {
                    Pair::Svf([a, b]) => {
                        a.set(cutoff, res, sample_rate);
                        b.set_like(a);
                    }
                    Pair::Ladder([a, b]) => {
                        a.set(cutoff, res, sample_rate);
                        b.set_like(a);
                    }
                }
            }
        }

        let (mut l, mut r) = (0.0f32, 0.0f32);
        let fe_now = filter_env.next();
        for v in &mut voices {
            let dt = (hz * v.ratio * v.wander_now / sample_rate).min(nyquist_dt);
            let pm = match &mut v.fm {
                Some((index, ratio, env_amount, phase)) => {
                    *phase = (*phase + dt * *ratio).rem_euclid(1.0);
                    (*index + *env_amount * fe_now + lfo_fm).max(0.0) * (TAU * *phase).sin() / TAU
                }
                None => 0.0,
            };
            let mut s = v.osc.next_pw(v.wave, dt, pm, v.pw + lfo_pw);
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

        for st in &mut stages {
            if st.drive > 0.0 {
                l = (l * st.drive).tanh() * st.drive_norm;
                r = (r * st.drive).tanh() * st.drive_norm;
            }
            match &mut st.pair {
                Pair::Svf([a, b]) => {
                    l = a.process(l, st.mode);
                    r = b.process(r, st.mode);
                }
                Pair::Ladder([a, b]) => {
                    l = a.process(l);
                    r = b.process(r);
                }
            }
        }

        let mut a = amp_env.next() * velocity_gain * (1.0 - lfo_amp.clamp(-1.0, 1.0) * 0.5);
        // Inside a phrase one note hands over to the next: equal power, since
        // their oscillators are not in phase.
        if tied_from && i < fade_len {
            a *= (std::f32::consts::FRAC_PI_2 * i as f32 / fade_len as f32).sin();
        }
        if ctx.tied_to.is_some() && i >= note_off {
            a *= (std::f32::consts::FRAC_PI_2 * (i - note_off) as f32 / fade_len as f32).cos();
        }
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
    StereoClip { offset, left: halve(&left), right: halve(&right) }
}
