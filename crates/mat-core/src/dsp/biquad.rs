//! Biquad filters (RBJ Audio EQ Cookbook) and a five-band track EQ.

use crate::dsp::quiet::{BLOCK, QUIET};
use std::f32::consts::{FRAC_1_SQRT_2, PI};

use crate::model::EqSettings;

#[derive(Clone, Copy)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn from_coefs(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, z1: 0.0, z2: 0.0 }
    }

    pub fn highpass(freq: f32, q: f32, sr: f32) -> Self {
        let (cos, alpha) = Self::prewarp(freq, q, sr);
        Self::from_coefs((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    pub fn lowpass(freq: f32, q: f32, sr: f32) -> Self {
        let (cos, alpha) = Self::prewarp(freq, q, sr);
        Self::from_coefs((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
    }

    pub fn peak(freq: f32, q: f32, gain_db: f32, sr: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, alpha) = Self::prewarp(freq, q, sr);
        Self::from_coefs(1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a)
    }

    pub fn low_shelf(freq: f32, gain_db: f32, sr: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, alpha) = Self::prewarp(freq, FRAC_1_SQRT_2, sr);
        let s = 2.0 * a.sqrt() * alpha;
        Self::from_coefs(
            a * ((a + 1.0) - (a - 1.0) * cos + s),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - s),
            (a + 1.0) + (a - 1.0) * cos + s,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - s,
        )
    }

    pub fn high_shelf(freq: f32, gain_db: f32, sr: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let (cos, alpha) = Self::prewarp(freq, FRAC_1_SQRT_2, sr);
        let s = 2.0 * a.sqrt() * alpha;
        Self::from_coefs(
            a * ((a + 1.0) + (a - 1.0) * cos + s),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - s),
            (a + 1.0) - (a - 1.0) * cos + s,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - s,
        )
    }

    fn prewarp(freq: f32, q: f32, sr: f32) -> (f32, f32) {
        let w = 2.0 * PI * freq.clamp(10.0, sr * 0.49) / sr;
        (w.cos(), w.sin() / (2.0 * q))
    }

    /// Transposed direct form II.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Applies a track EQ to a stereo buffer in place.
pub fn apply_eq(eq: &EqSettings, left: &mut [f32], right: &mut [f32], sr: f32) {
    let mut stages: Vec<Biquad> = Vec::new();
    if eq.lowcut_hz > 0.0 {
        stages.push(Biquad::highpass(eq.lowcut_hz, FRAC_1_SQRT_2, sr));
    }
    if eq.low_db != 0.0 {
        stages.push(Biquad::low_shelf(eq.low_freq_hz, eq.low_db, sr));
    }
    if eq.mid_db != 0.0 {
        stages.push(Biquad::peak(eq.mid_freq_hz, 0.9, eq.mid_db, sr));
    }
    if eq.high_db != 0.0 {
        stages.push(Biquad::high_shelf(eq.high_freq_hz, eq.high_db, sr));
    }
    if eq.highcut_hz > 0.0 {
        stages.push(Biquad::lowpass(eq.highcut_hz, FRAC_1_SQRT_2, sr));
    }
    for channel in [left, right] {
        let mut filters = stages.clone();
        // Silence skipped, as in the send effects (see `dsp::quiet`): a zero
        // block through filters whose state is below `QUIET` is left at zero
        // and the state cleared. A master EQ runs once per stem over the whole
        // song, and a stem is mostly rests — 250 ms a stem before this.
        let mut quiet = true;
        for block in channel.chunks_mut(BLOCK) {
            let silent = block.iter().all(|s| *s == 0.0);
            if quiet && silent {
                continue;
            }
            quiet = false;
            for s in block.iter_mut() {
                for f in &mut filters {
                    *s = f.process(*s);
                }
            }
            let peak = block.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            if silent && peak < QUIET && filters.iter().all(|f| f.z1.abs() < QUIET && f.z2.abs() < QUIET) {
                block.fill(0.0);
                filters.iter_mut().for_each(|f| {
                    f.z1 = 0.0;
                    f.z2 = 0.0;
                });
                quiet = true;
            }
        }
    }
}
