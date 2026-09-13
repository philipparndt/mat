//! Stereo chorus: two modulated delay lines with LFOs in quadrature, which
//! widens and thickens a sound (the classic ensemble effect on trance pads).

use std::f32::consts::TAU;

use crate::model::ChorusSettings;

pub fn apply_chorus(settings: &ChorusSettings, left: &mut [f32], right: &mut [f32], sr: f32) {
    let base = 0.012 * sr;
    let depth = settings.depth_ms / 1000.0 * sr;
    let size = (base + depth) as usize + 4;
    let mut lines = [vec![0.0f32; size], vec![0.0f32; size]];
    let mut pos = 0usize;
    let step = settings.rate_hz / sr;
    let mut phase = 0.0f32;
    let mix = settings.mix.clamp(0.0, 1.0);
    for i in 0..left.len() {
        let input = [left[i], right[i]];
        let mut wet = [0.0f32; 2];
        for ch in 0..2 {
            let lfo = (TAU * (phase + ch as f32 * 0.25)).sin();
            let delay = base + depth * 0.5 * (1.0 + lfo);
            let read = pos as f32 + size as f32 - delay;
            let i0 = read.floor() as usize % size;
            let frac = read - read.floor();
            let a = lines[ch][i0];
            let b = lines[ch][(i0 + 1) % size];
            wet[ch] = a + (b - a) * frac;
            lines[ch][pos] = input[ch];
        }
        pos = (pos + 1) % size;
        phase = (phase + step) % 1.0;
        // Cross the wet signals for extra width.
        left[i] = input[0] * (1.0 - mix * 0.5) + wet[1] * mix * 0.7;
        right[i] = input[1] * (1.0 - mix * 0.5) + wet[0] * mix * 0.7;
    }
}
