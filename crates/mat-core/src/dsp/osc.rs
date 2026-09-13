//! Band-limited oscillators using PolyBLEP anti-aliasing.

use std::f32::consts::TAU;

use crate::model::Waveform;

#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

#[derive(Clone)]
pub struct Oscillator {
    phase: f32,
    /// Integrator state for the triangle wave.
    tri: f32,
}

impl Oscillator {
    pub fn new(phase: f32) -> Self {
        let phase = phase.rem_euclid(1.0);
        // Start the integrator where a centered triangle would be at this phase.
        let tri = if phase < 0.5 { -0.25 + phase } else { 0.75 - phase };
        Self { phase, tri }
    }

    /// `dt` is frequency / sample rate and must be below 0.5.
    #[inline]
    pub fn next(&mut self, wave: Waveform, dt: f32) -> f32 {
        self.next_pm(wave, dt, 0.0)
    }

    /// Like `next`, with a phase offset in cycles (phase modulation).
    #[inline]
    pub fn next_pm(&mut self, wave: Waveform, dt: f32, pm: f32) -> f32 {
        let t = if pm == 0.0 { self.phase } else { (self.phase + pm).rem_euclid(1.0) };
        let out = match wave {
            Waveform::Sine => (t * TAU).sin(),
            Waveform::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
            Waveform::Square => square(t, dt),
            Waveform::Triangle => {
                self.tri = dt * square(t, dt) + (1.0 - dt) * self.tri;
                self.tri * 4.0
            }
        };
        self.phase += dt;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        out
    }
}

#[inline]
fn square(t: f32, dt: f32) -> f32 {
    let naive = if t < 0.5 { 1.0 } else { -1.0 };
    naive + poly_blep(t, dt) - poly_blep((t + 0.5) % 1.0, dt)
}
