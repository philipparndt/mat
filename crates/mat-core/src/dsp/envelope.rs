//! ADSR envelope with linear attack and exponential decay/release.

use crate::model::Adsr;

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Attack,
    Decay,
    Release,
    Done,
}

pub struct Envelope {
    stage: Stage,
    level: f32,
    attack_step: f32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
}

/// Coefficient for a one-pole approach that covers 99% of the distance in `seconds`.
fn coef(seconds: f32, sample_rate: f32) -> f32 {
    (-(100f32.ln()) / (seconds * sample_rate)).exp()
}

impl Envelope {
    pub fn new(adsr: &Adsr, sample_rate: f32) -> Self {
        Self {
            stage: Stage::Attack,
            level: 0.0,
            attack_step: 1.0 / (adsr.attack.max(0.001) * sample_rate),
            decay_coef: coef(adsr.decay.max(0.001), sample_rate),
            sustain: adsr.sustain,
            release_coef: coef(adsr.release.max(0.005), sample_rate),
        }
    }

    pub fn release(&mut self) {
        if self.stage != Stage::Done {
            self.stage = Stage::Release;
        }
    }

    pub fn is_done(&self) -> bool {
        self.stage == Stage::Done
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        match self.stage {
            Stage::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => self.level = self.sustain + (self.level - self.sustain) * self.decay_coef,
            Stage::Release => {
                self.level *= self.release_coef;
                if self.level < 1e-4 {
                    self.level = 0.0;
                    self.stage = Stage::Done;
                }
            }
            Stage::Done => {}
        }
        self.level
    }
}
