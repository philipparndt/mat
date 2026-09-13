//! Turntable scratching: a sample region played with a moving position and a
//! crossfader. Each hit is one move that spans the note's duration.

use std::f64::consts::PI;

use rayon::prelude::*;

use crate::arrange::TimedNote;
use crate::dsp::{StereoClip, db_to_gain};
use crate::model::{DrumKind, Pitch, ScratchDef};
use crate::sampler::audio_file::AudioFile;
use crate::sampler::sinc;

pub fn render(def: &ScratchDef, notes: &[TimedNote], sample_rate: f32) -> Result<Vec<StereoClip>, String> {
    let file = AudioFile::open(std::path::Path::new(&def.path))?;
    let rate = file.sample_rate;
    let start = (def.start * rate).round() as i64;
    let end = def.length.map_or(file.frames as i64, |l| (start + (l * rate).round() as i64).min(file.frames as i64));
    if end <= start + 64 {
        return Err(format!("scratch region in {} is empty", def.path));
    }
    let (left, right) = file.read_stereo(start, end)?;
    render_region(def, left, right, rate, notes, sample_rate)
}

/// Renders scratch moves on an in-memory record (`rate` = its sample rate).
pub fn render_region(def: &ScratchDef, left: Vec<f32>, right: Vec<f32>, rate: f64, notes: &[TimedNote], sample_rate: f32) -> Result<Vec<StereoClip>, String> {
    if left.len() < 64 {
        return Err("scratch region is empty".into());
    }
    let pad = (sinc::HALF_WIDTH * 4) as i64;
    let padded = |v: &Vec<f32>| {
        let mut p = vec![0.0f32; pad as usize];
        p.extend_from_slice(v);
        p.extend(std::iter::repeat_n(0.0f32, pad as usize));
        p
    };
    let (left, right) = (padded(&left), padded(&right));
    let region_len = (left.len() as i64 - 2 * pad) as f64;
    let gain = db_to_gain(def.gain_db);
    Ok(notes
        .par_iter()
        .filter_map(|note| {
            let Pitch::Drum(kind) = note.pitch else { return None };
            if !kind.is_scratch() {
                return None;
            }
            let frames = ((note.duration * sample_rate as f64).round() as usize).max(1);
            let speed = (rate / sample_rate as f64) * def.speed as f64;
            let dur = note.duration;
            // Position within the region (in file samples) and fader (0..1) over the move.
            let travel = (speed * sample_rate as f64 * dur).min(region_len - 1.0);
            let (pos, fader): (Box<dyn Fn(f64) -> f64 + Sync>, Box<dyn Fn(f64) -> f64 + Sync>) = match kind {
                DrumKind::Fwd => (Box::new(move |x| travel * x), Box::new(|_| 1.0)),
                DrumKind::Back => (Box::new(move |x| travel * (1.0 - x)), Box::new(|_| 1.0)),
                DrumKind::Baby => (Box::new(move |x| travel * 0.5 * (1.0 - (2.0 * PI * x).cos())), Box::new(|_| 1.0)),
                DrumKind::Scribble => (Box::new(move |x| travel * 0.15 * (1.0 - (8.0 * PI * x).cos())), Box::new(|_| 1.0)),
                // Chirp: fader open only around the reversal points, off while the arm turns around.
                DrumKind::Chirp => (
                    Box::new(move |x| travel * 0.5 * (1.0 - (2.0 * PI * x).cos())),
                    Box::new(|x| if (x % 0.5) < 0.3 { 1.0 } else { 0.0 }),
                ),
                DrumKind::Transform => (
                    Box::new(move |x| travel * 0.5 * (1.0 - (2.0 * PI * x).cos())),
                    Box::new(|x| if (x * 8.0) % 1.0 < 0.5 { 1.0 } else { 0.0 }),
                ),
                _ => return None,
            };
            let fade = (0.0015 * sample_rate) as usize;
            let mut out_l = vec![0.0f32; frames];
            let mut out_r = vec![0.0f32; frames];
            let mut gate_smooth = 0.0f64;
            let coef = 1.0 - (-1.0 / fade.max(1) as f64).exp();
            if def.keep_pitch {
                // Time-stretch: grains from the moving position, each played at
                // the record's own pitch, overlap-added with a Hann window.
                // Grains start every `hop` output samples at the moving position and
                // play at the record's own pitch, forwards or backwards with the
                // move. Short grains: Hann windows, 4x overlap (smooth, tonal).
                // Long grains (beats): triangular windows, 2x overlap, so every
                // bit of the record is heard once and hits stay sharp.
                let hop = ((def.grain * sample_rate) as usize).max(48);
                let slicing = def.grain > 0.06;
                let grain = if slicing { hop * 2 } else { hop * 4 };
                let native = rate / sample_rate as f64;
                let mut g = 0usize;
                while g < frames {
                    let x = g as f64 / frames as f64;
                    let x_next = ((g + hop) as f64 / frames as f64).min(1.0);
                    let backwards = pos(x_next) < pos(x);
                    let p0 = pos(x) + pad as f64;
                    for k in 0..grain {
                        let i = g + k;
                        if i >= frames {
                            break;
                        }
                        let w = if slicing {
                            let t = k as f64 / grain as f64;
                            1.0 - (2.0 * t - 1.0).abs()
                        } else {
                            0.25 * (1.0 - (2.0 * PI * k as f64 / grain as f64).cos())
                        };
                        // Center the grain on the position so overlaps line up.
                        let offset = (k as f64 - grain as f64 * 0.5) * native;
                        let p = if backwards { p0 - offset } else { p0 + offset };
                        if p < 0.0 || p >= left.len() as f64 - 1.0 {
                            continue;
                        }
                        out_l[i] += sinc::interpolate(&left, p, 1.0) * w as f32;
                        out_r[i] += sinc::interpolate(&right, p, 1.0) * w as f32;
                    }
                    g += hop;
                }
                for i in 0..frames {
                    let x = i as f64 / frames as f64;
                    gate_smooth += (fader(x) - gate_smooth) * coef;
                    let edge = (i.min(frames - 1 - i) as f64 / fade as f64).min(1.0);
                    let a = (gate_smooth * edge) as f32 * gain * (0.3 + 0.7 * note.velocity);
                    out_l[i] *= a;
                    out_r[i] *= a;
                }
            } else {
                for i in 0..frames {
                    let x = i as f64 / frames as f64;
                    let p = pos(x) + pad as f64;
                    let next = pos(((i + 1) as f64 / frames as f64).min(1.0)) + pad as f64;
                    let velocity = (next - p).abs().max(1e-3);
                    let cutoff = (1.0 / velocity).min(1.0) as f32;
                    gate_smooth += (fader(x) - gate_smooth) * coef;
                    let edge = (i.min(frames - 1 - i) as f64 / fade as f64).min(1.0);
                    let a = (gate_smooth * edge) as f32 * gain * (0.3 + 0.7 * note.velocity);
                    out_l[i] = sinc::interpolate(&left, p, cutoff) * a;
                    out_r[i] = sinc::interpolate(&right, p, cutoff) * a;
                }
            }
            Some(StereoClip { offset: (note.start * sample_rate as f64).round() as usize, left: out_l, right: out_r })
        })
        .collect())
}
