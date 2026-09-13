//! Building blocks for sound generation and processing.

pub mod biquad;
pub mod chorus;
pub mod delay;
pub mod dynamics;
pub mod envelope;
pub mod filter;
pub mod limiter;
pub mod osc;
pub mod reverb;

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn midi_to_hz(midi: f32) -> f32 {
    440.0 * 2f32.powf((midi - 69.0) / 12.0)
}

/// Constant-power pan law, normalized to unity gain at center.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (angle.cos() * std::f32::consts::SQRT_2, angle.sin() * std::f32::consts::SQRT_2)
}

/// Small, fast, deterministic PRNG (xorshift64*), so renders are reproducible.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    #[inline]
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in [-1, 1).
    #[inline]
    pub fn bipolar(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

/// A block of stereo audio starting at `offset` samples.
pub struct StereoClip {
    pub offset: usize,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}
