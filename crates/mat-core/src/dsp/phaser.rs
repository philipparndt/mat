//! Phaser: a chain of first-order allpass stages swept by an LFO, with feedback.

use std::f32::consts::{PI, TAU};

use crate::model::PhaserSettings;

/// A phaser and the state it carries: its allpass stages, its feedback and its
/// LFO phase. Kept, it processes a signal that arrives a stretch at a time —
/// which is how `crate::stream` renders — to the samples one call over the
/// whole signal produces.
pub struct Phaser {
    settings: PhaserSettings,
    stages: usize,
    state: [[f32; 12]; 2],
    fb: [f32; 2],
    step: f32,
    phase: f32,
    mix: f32,
    sr: f32,
}

impl Phaser {
    pub fn new(ph: &PhaserSettings, sr: f32) -> Self {
        Phaser {
            settings: ph.clone(),
            stages: ph.stages.clamp(2, 12) as usize,
            state: [[0.0f32; 12]; 2],
            fb: [0.0f32; 2],
            step: ph.rate_hz / sr,
            phase: 0.0,
            mix: ph.mix.clamp(0.0, 1.0),
            sr,
        }
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let (ph, sr, stages, mix) = (&self.settings, self.sr, self.stages, self.mix);
        for i in 0..left.len() {
            self.phase = (self.phase + self.step) % 1.0;
            // Sweep between 200 Hz and ~4 kHz, scaled by depth; the right channel is offset.
            for (ch, buf) in [&mut *left, &mut *right].into_iter().enumerate() {
                let lfo = (TAU * (self.phase + ch as f32 * 0.25)).sin() * 0.5 + 0.5;
                let f = 200.0 * (20.0f32).powf(lfo * ph.depth);
                let a = (1.0 - (PI * f / sr).tan()) / (1.0 + (PI * f / sr).tan());
                let x = buf[i];
                let mut y = x + self.fb[ch] * ph.feedback;
                for s in self.state[ch].iter_mut().take(stages) {
                    let out = -a * y + *s;
                    *s = y + a * out;
                    y = out;
                }
                self.fb[ch] = y;
                buf[i] = x * (1.0 - mix) + y * mix;
            }
        }
    }
}

pub fn apply_phaser(ph: &PhaserSettings, left: &mut [f32], right: &mut [f32], sr: f32) {
    Phaser::new(ph, sr).process(left, right);
}
