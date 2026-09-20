//! Zero-delay-feedback state variable filter (Cytomic / A. Simper topology)
//! and simple one-pole filters.

use std::f32::consts::PI;

use crate::model::FilterMode;

#[derive(Clone, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
}

impl Svf {
    pub fn new(cutoff: f32, resonance: f32, sample_rate: f32) -> Self {
        let mut f = Self::default();
        f.set(cutoff, resonance, sample_rate);
        f
    }

    /// `resonance` is 0..1; 1 is close to self-oscillation.
    #[inline]
    pub fn set(&mut self, cutoff: f32, resonance: f32, sample_rate: f32) {
        let cutoff = cutoff.clamp(10.0, sample_rate * 0.45);
        let g = (PI * cutoff / sample_rate).tan();
        self.k = 2.0 - 1.96 * resonance.clamp(0.0, 1.0);
        self.a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    /// Copies the coefficients of a filter already set, for the other channel.
    #[inline]
    pub fn set_like(&mut self, other: &Svf) {
        self.a1 = other.a1;
        self.a2 = other.a2;
        self.a3 = other.a3;
        self.k = other.k;
    }

    #[inline]
    pub fn process(&mut self, x: f32, mode: FilterMode) -> f32 {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        match mode {
            FilterMode::Lowpass => v2,
            FilterMode::Bandpass => v1,
            FilterMode::Highpass => x - self.k * v1 - v2,
            FilterMode::Off => x,
        }
    }
}

/// Four-pole (24 dB/octave) lowpass ladder, zero-delay feedback, with the
/// loop's input saturated — the sound of a transistor ladder pushed by its own
/// resonance: it thickens instead of ringing thin. Meant to run oversampled.
#[derive(Clone, Default)]
pub struct Ladder {
    s: [f32; 4],
    g: f32,
    k: f32,
    comp: f32,
}

impl Ladder {
    /// `resonance` is 0..1; 1 is at the edge of self-oscillation.
    #[inline]
    pub fn set(&mut self, cutoff: f32, resonance: f32, sample_rate: f32) {
        let cutoff = cutoff.clamp(10.0, sample_rate * 0.45);
        let g = (PI * cutoff / sample_rate).tan();
        self.g = g / (1.0 + g);
        self.k = 3.9 * resonance.clamp(0.0, 1.0);
        // A ladder loses its passband as the resonance comes up; give half of it back.
        self.comp = (1.0 + self.k).sqrt();
    }

    /// Copies the coefficients of a filter already set, for the other channel.
    #[inline]
    pub fn set_like(&mut self, other: &Ladder) {
        self.g = other.g;
        self.k = other.k;
        self.comp = other.comp;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let g = self.g;
        let g2 = g * g;
        let feedback = (1.0 - g) * (g2 * g * self.s[0] + g2 * self.s[1] + g * self.s[2] + self.s[3]);
        let mut y = ((x * self.comp - self.k * feedback) / (1.0 + self.k * g2 * g2)).tanh();
        for s in &mut self.s {
            let v = (y - *s) * g;
            y = v + *s;
            *s = y + v;
        }
        y
    }
}

#[derive(Clone, Default)]
pub struct OnePole {
    coef: f32,
    state: f32,
}

impl OnePole {
    pub fn new(cutoff: f32, sample_rate: f32) -> Self {
        Self { coef: 1.0 - (-2.0 * PI * cutoff / sample_rate).exp(), state: 0.0 }
    }

    #[inline]
    pub fn lowpass(&mut self, x: f32) -> f32 {
        self.state += self.coef * (x - self.state);
        self.state
    }

    #[inline]
    pub fn highpass(&mut self, x: f32) -> f32 {
        x - self.lowpass(x)
    }

    /// What the filter still holds, for deciding that an effect has gone quiet.
    pub fn state(&self) -> f32 {
        self.state
    }

    pub fn reset(&mut self) {
        self.state = 0.0;
    }
}
