//! Phaser: a chain of first-order allpass stages swept by an LFO, with feedback.

use std::f32::consts::{PI, TAU};

use crate::model::PhaserSettings;

pub fn apply_phaser(ph: &PhaserSettings, left: &mut [f32], right: &mut [f32], sr: f32) {
    let stages = ph.stages.clamp(2, 12) as usize;
    let mut state = [[0.0f32; 12]; 2];
    let mut fb = [0.0f32; 2];
    let step = ph.rate_hz / sr;
    let mut phase = 0.0f32;
    let mix = ph.mix.clamp(0.0, 1.0);
    for i in 0..left.len() {
        phase = (phase + step) % 1.0;
        // Sweep between 200 Hz and ~4 kHz, scaled by depth; the right channel is offset.
        for (ch, buf) in [&mut *left, &mut *right].into_iter().enumerate() {
            let lfo = (TAU * (phase + ch as f32 * 0.25)).sin() * 0.5 + 0.5;
            let f = 200.0 * (20.0f32).powf(lfo * ph.depth);
            let a = (1.0 - (PI * f / sr).tan()) / (1.0 + (PI * f / sr).tan());
            let x = buf[i];
            let mut y = x + fb[ch] * ph.feedback;
            for s in state[ch].iter_mut().take(stages) {
                let out = -a * y + *s;
                *s = y + a * out;
                y = out;
            }
            fb[ch] = y;
            buf[i] = x * (1.0 - mix) + y * mix;
        }
    }
}
