//! Stereo chorus: two modulated delay lines with LFOs in quadrature, which
//! widens and thickens a sound (the classic ensemble effect on trance pads).

use std::f32::consts::TAU;

use crate::model::ChorusSettings;

/// A chorus and the state it carries: its two delay lines and its LFO phase.
/// Kept, it processes a signal that arrives a stretch at a time — which is how
/// `crate::stream` renders — to the samples one call over the whole signal
/// produces.
pub struct Chorus {
    lines: [Vec<f32>; 2],
    size: usize,
    pos: usize,
    phase: f32,
    step: f32,
    base: f32,
    depth: f32,
    mix: f32,
}

impl Chorus {
    pub fn new(settings: &ChorusSettings, sr: f32) -> Self {
        let base = 0.012 * sr;
        let depth = settings.depth_ms / 1000.0 * sr;
        let size = (base + depth) as usize + 4;
        Chorus {
            lines: [vec![0.0f32; size], vec![0.0f32; size]],
            size,
            pos: 0,
            phase: 0.0,
            step: settings.rate_hz / sr,
            base,
            depth,
            mix: settings.mix.clamp(0.0, 1.0),
        }
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let (base, depth, size, mix) = (self.base, self.depth, self.size, self.mix);
        for i in 0..left.len() {
            let input = [left[i], right[i]];
            let mut wet = [0.0f32; 2];
            for ch in 0..2 {
                let lfo = (TAU * (self.phase + ch as f32 * 0.25)).sin();
                let delay = base + depth * 0.5 * (1.0 + lfo);
                let read = self.pos as f32 + size as f32 - delay;
                let i0 = read.floor() as usize % size;
                let frac = read - read.floor();
                let a = self.lines[ch][i0];
                let b = self.lines[ch][(i0 + 1) % size];
                wet[ch] = a + (b - a) * frac;
                self.lines[ch][self.pos] = input[ch];
            }
            self.pos = (self.pos + 1) % size;
            self.phase = (self.phase + self.step) % 1.0;
            // Cross the wet signals for extra width.
            left[i] = input[0] * (1.0 - mix * 0.5) + wet[1] * mix * 0.7;
            right[i] = input[1] * (1.0 - mix * 0.5) + wet[0] * mix * 0.7;
        }
    }
}

pub fn apply_chorus(settings: &ChorusSettings, left: &mut [f32], right: &mut [f32], sr: f32) {
    Chorus::new(settings, sr).process(left, right);
}
