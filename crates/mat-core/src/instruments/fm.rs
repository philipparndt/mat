//! FM synth: up to six sine operators, each with its own envelope, that either
//! sound (carriers) or bend the phase of a lower-numbered operator
//! (modulators) — the bright, glassy, percussive sounds of 80s digital synths
//! that a filter cannot make: electric pianos, bells, slap and rubber basses,
//! marimbas, brass.
//!
//! Like the subtractive voice it runs at twice the sample rate, because FM
//! sidebands reach far above the notes that make them.

use std::f32::consts::TAU;

use crate::arrange::TimedNote;
use crate::dsp::envelope::Envelope;
use crate::dsp::{Rng, StereoClip, midi_to_hz};
use crate::instruments::synth::halve;
use crate::model::FmDef;

const VOICE_GAIN: f32 = 0.22;
const OVERSAMPLE: usize = 2;
/// Pitch moves once per this many oversampled samples.
const CONTROL: usize = 16;

/// `level` is in steps like a DX7's: every 0.125 down is 6 dB, 1 is full.
fn level_gain(level: f32) -> f32 {
    if level <= 0.0 { 0.0 } else { 2f32.powf((level.min(1.0) - 1.0) * 8.0) }
}

/// A modulator at full level bends its target's phase by this many radians.
const FULL_INDEX: f32 = 4.0 * std::f32::consts::PI;
/// An operator feeding back at 1 bends its own phase by this much.
const FULL_FEEDBACK: f32 = 4.0;

struct Op {
    step: f32,
    /// One phase per channel: the two sides are detuned against each other by `stereo`.
    phase: [f32; 2],
    env: Envelope,
    gain: f32,
    feedback: f32,
    last: [[f32; 2]; 2],
    into: Option<usize>,
}

pub fn render_note(def: &FmDef, note: &TimedNote, output_rate: f32) -> StereoClip {
    let sample_rate = output_rate * OVERSAMPLE as f32;
    let mut rng = Rng::new(note.seed);
    let offset = (note.start * output_rate as f64).round() as usize;
    let note_off = ((note.duration * sample_rate as f64).round() as usize).max(1);
    let release = def.operators.iter().filter(|o| o.into.is_none()).map(|o| o.env.release).fold(0.0f32, f32::max);
    let max_len = note_off + ((release * 3.0 + 0.05) * sample_rate) as usize;

    let drift = if def.drift_cents > 0.0 { rng.bipolar() * def.drift_cents } else { 0.0 };
    let base_hz = midi_to_hz(note.midi + drift / 100.0);
    let octaves_up = ((note.midi - 60.0) / 12.0).max(0.0);
    let side = 2f32.powf(def.stereo_cents / 2400.0);
    let mut ops: Vec<Op> = def
        .operators
        .iter()
        .map(|o| {
            // Velocity and key scaling work on the level, as on the hardware:
            // a soft or a high note is duller, not only quieter.
            let level = o.level - o.velocity * (1.0 - note.velocity) * 0.5 - o.keyscale * octaves_up * 0.125;
            let hz = match o.fixed_hz {
                Some(hz) => hz,
                None => base_hz * o.ratio * 2f32.powf(o.detune_cents / 1200.0),
            };
            let start = rng.unit();
            Op {
                step: hz / sample_rate,
                // A modulator that starts anywhere gives every note another attack; carriers may.
                phase: if o.into.is_some() { [0.0, 0.0] } else { [start, start] },
                env: Envelope::new(&o.env, sample_rate),
                gain: level_gain(level) * if o.into.is_some() { FULL_INDEX / TAU } else { 1.0 },
                feedback: o.feedback * FULL_FEEDBACK / TAU,
                last: [[0.0; 2]; 2],
                into: o.into,
            }
        })
        .collect();
    let carriers = ops.iter().filter(|o| o.into.is_none()).count().max(1);
    let out_gain = VOICE_GAIN / (carriers as f32).sqrt();

    let vib = def.vibrato;
    let vib_active = vib.rate_hz > 0.0 && vib.depth_cents > 0.0;
    let mut left = Vec::with_capacity(max_len);
    let mut right = Vec::with_capacity(max_len);
    let mut pitch = [1.0f32; 2];
    let mut pm = vec![[0.0f32; 2]; ops.len()];
    for i in 0..max_len {
        if i == note_off {
            ops.iter_mut().for_each(|o| o.env.release());
        }
        if i % CONTROL == 0 {
            let t = i as f32 / sample_rate;
            let mut factor = 1.0;
            if let Some((depth, decay)) = def.pitch_env {
                factor *= 2f32.powf(depth * (-4.6 * t / decay.max(0.001)).exp() / 12.0);
            }
            if vib_active && t > vib.delay {
                let fade = ((t - vib.delay) / 0.25).min(1.0);
                factor *= 2f32.powf(vib.depth_cents * fade * (TAU * vib.rate_hz * (t - vib.delay)).sin() / 1200.0);
            }
            pitch = [factor / side, factor * side];
        }
        pm.iter_mut().for_each(|p| *p = [0.0, 0.0]);
        let mut out = [0.0f32; 2];
        let mut sounding = false;
        // From the highest operator down: a modulator is always above its target.
        for k in (0..ops.len()).rev() {
            let op = &mut ops[k];
            let env = op.env.next() * op.gain;
            sounding |= op.into.is_none() && !op.env.is_done();
            for ch in 0..2 {
                let fb = if op.feedback > 0.0 { (op.last[ch][0] + op.last[ch][1]) * 0.5 * op.feedback } else { 0.0 };
                let y = (TAU * (op.phase[ch] + pm[k][ch] + fb)).sin();
                op.last[ch] = [y, op.last[ch][0]];
                op.phase[ch] += op.step * pitch[ch];
                if op.phase[ch] >= 1.0 {
                    op.phase[ch] -= 1.0;
                }
                match op.into {
                    Some(target) => pm[target][ch] += y * env,
                    None => out[ch] += y * env,
                }
            }
        }
        left.push(out[0] * out_gain);
        right.push(out[1] * out_gain);
        if !sounding {
            break;
        }
    }
    StereoClip { offset, left: halve(&left), right: halve(&right) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Adsr, FmOperator, Pitch};

    fn note(midi: f32, velocity: f32) -> TimedNote {
        TimedNote { start: 0.0, duration: 0.5, pitch: Pitch::Note(midi), midi, velocity, accent: false, slide: false, seed: 1, region: None }
    }

    fn brightness(clip: &StereoClip) -> f32 {
        let diff: f32 = clip.left.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        let power: f32 = clip.left.iter().map(|s| s * s).sum();
        diff / power
    }

    #[test]
    fn a_modulator_brightens_and_velocity_opens_it() {
        let env = Adsr { attack: 0.001, decay: 0.5, sustain: 1.0, release: 0.05 };
        let carrier = FmOperator { env, ..FmOperator::default() };
        let modulator = FmOperator { env, level: 0.8, velocity: 1.0, into: Some(0), ..FmOperator::default() };
        let plain = FmDef { operators: vec![carrier.clone()], ..FmDef::default() };
        let fm = FmDef { operators: vec![carrier, modulator], ..FmDef::default() };
        let sine = brightness(&render_note(&plain, &note(60.0, 1.0), 48_000.0));
        let hard = brightness(&render_note(&fm, &note(60.0, 1.0), 48_000.0));
        let soft = brightness(&render_note(&fm, &note(60.0, 0.3), 48_000.0));
        assert!(hard > sine * 4.0, "{hard} vs {sine}");
        assert!(soft < hard * 0.7, "{soft} vs {hard}");
    }
}
