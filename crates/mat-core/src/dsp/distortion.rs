//! Distortion: a waveshaper run at four times the sample rate, an optional
//! bit and sample-rate crusher, and a tone lowpass.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use crate::dsp::db_to_gain;
use crate::model::{DistortionMode, DistortionSettings};

const OVERSAMPLE: usize = 4;
/// Taps of the lowpass used up and down. Odd, and one more than a multiple of
/// eight, so both passes together delay the signal by a whole number of
/// samples: `LATENCY`.
const TAPS: usize = 65;
const LATENCY: usize = (TAPS - 1) * 2 / 2 / OVERSAMPLE;
const HISTORY: usize = TAPS.div_ceil(OVERSAMPLE);
const RING: usize = 128;

/// One channel's state.
#[derive(Clone)]
struct Channel {
    /// The last inputs, newest first in time order from `pos` backwards.
    input: [f32; HISTORY],
    input_pos: usize,
    /// The last shaped samples at the oversampled rate.
    shaped: [f32; RING],
    shaped_pos: usize,
    /// The dry signal, delayed to line up with the wet one.
    dry: [f32; LATENCY],
    dry_pos: usize,
    /// Sample-rate crusher: the held value and the phase until the next one.
    held: f32,
    hold_phase: f32,
    tone: [f32; 2],
    dc_x: f32,
    dc_y: f32,
}

/// A distortion and the state it carries, so a signal that arrives a stretch
/// at a time comes out as the samples one call over the whole signal produces.
pub struct Distortion {
    mode: DistortionMode,
    pre: f32,
    post: f32,
    mix: f32,
    /// Quantisation steps per unit, or 0 for none.
    steps: f32,
    /// Crusher rate as a fraction of the sample rate, or 0 for none.
    hold_step: f32,
    /// One-pole coefficient of the tone lowpass, or 0 for none.
    tone: f32,
    dc: f32,
    kernel: [f32; TAPS],
    channels: [Channel; 2],
}

impl Distortion {
    pub fn new(d: &DistortionSettings, sr: f32) -> Self {
        // 0..1 is 0..36 dB into the shaper. The output comes down by most of
        // that again, so turning the drive up adds dirt rather than level.
        let pre = db_to_gain(d.drive.clamp(0.0, 1.0) * 36.0);
        let post = pre.powf(-0.7) * db_to_gain(d.level_db);
        let channel = Channel {
            input: [0.0; HISTORY],
            input_pos: 0,
            shaped: [0.0; RING],
            shaped_pos: 0,
            dry: [0.0; LATENCY],
            dry_pos: 0,
            held: 0.0,
            hold_phase: 1.0,
            tone: [0.0; 2],
            dc_x: 0.0,
            dc_y: 0.0,
        };
        Distortion {
            mode: d.mode,
            pre,
            post,
            mix: d.mix.clamp(0.0, 1.0),
            steps: if d.bits > 0 && d.bits < 24 { (1u32 << (d.bits - 1)) as f32 } else { 0.0 },
            hold_step: if d.rate_hz > 0.0 && d.rate_hz < sr { d.rate_hz / sr } else { 0.0 },
            tone: if d.tone_hz > 0.0 && d.tone_hz < sr * 0.45 { 1.0 - (-TAU * d.tone_hz / sr).exp() } else { 0.0 },
            dc: 1.0 - TAU * 10.0 / sr,
            kernel: kernel(),
            channels: [channel.clone(), channel],
        }
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (ch, buf) in [left, right].into_iter().enumerate() {
            for x in buf.iter_mut() {
                *x = self.sample(ch, *x);
            }
        }
    }

    fn sample(&mut self, ch: usize, x: f32) -> f32 {
        let (mode, pre, kernel) = (self.mode, self.pre, &self.kernel);
        let c = &mut self.channels[ch];

        c.input_pos = (c.input_pos + 1) % HISTORY;
        c.input[c.input_pos] = x;
        // Up: the input with three zeros after every sample, lowpassed. Only
        // every fourth tap meets a sample, so each phase is a short sum.
        // Down: lowpass what the shaper added above the band and keep one in
        // four — the one that lines up with an input sample, so the delay
        // through both filters is a whole number of samples.
        let mut y = 0.0;
        for phase in 0..OVERSAMPLE {
            let mut up = 0.0;
            let mut k = 0;
            while phase + k * OVERSAMPLE < TAPS {
                up += kernel[phase + k * OVERSAMPLE] * c.input[(c.input_pos + HISTORY - k) % HISTORY];
                k += 1;
            }
            c.shaped_pos = (c.shaped_pos + 1) % RING;
            c.shaped[c.shaped_pos] = shape(mode, up * OVERSAMPLE as f32 * pre);
            if phase == 0 {
                for (j, h) in kernel.iter().enumerate() {
                    y += h * c.shaped[(c.shaped_pos + RING - j) % RING];
                }
            }
        }

        if self.hold_step > 0.0 {
            c.hold_phase += self.hold_step;
            if c.hold_phase >= 1.0 {
                c.hold_phase -= 1.0;
                c.held = y;
            }
            y = c.held;
        }
        if self.steps > 0.0 {
            y = (y * self.steps).round() / self.steps;
        }
        // The asymmetric shapes and the crusher leave a DC offset behind.
        let blocked = y - c.dc_x + self.dc * c.dc_y;
        c.dc_x = y;
        c.dc_y = blocked;
        y = blocked;
        if self.tone > 0.0 {
            c.tone[0] += self.tone * (y - c.tone[0]);
            c.tone[1] += self.tone * (c.tone[0] - c.tone[1]);
            y = c.tone[1];
        }

        let dry = c.dry[c.dry_pos];
        c.dry[c.dry_pos] = x;
        c.dry_pos = (c.dry_pos + 1) % LATENCY;
        dry * (1.0 - self.mix) + y * self.post * self.mix
    }
}

fn shape(mode: DistortionMode, v: f32) -> f32 {
    match mode {
        DistortionMode::Soft => v.tanh(),
        DistortionMode::Hard => v.clamp(-1.0, 1.0),
        DistortionMode::Fold => (v * FRAC_PI_2).sin(),
        // Hard on the way up, soft on the way down: even harmonics, like a
        // misbiased transistor.
        DistortionMode::Fuzz => {
            if v >= 0.0 {
                1.0 - (-v * 1.5).exp()
            } else {
                (v * 0.6).tanh()
            }
        }
    }
}

/// A Blackman-windowed sinc with its cutoff a little under the band edge of
/// the original rate, summing to one.
fn kernel() -> [f32; TAPS] {
    let mut h = [0.0f32; TAPS];
    let cutoff = 0.45 / OVERSAMPLE as f32;
    let mid = (TAPS - 1) as f32 / 2.0;
    for (i, tap) in h.iter_mut().enumerate() {
        let t = i as f32 - mid;
        let sinc = if t == 0.0 { 2.0 * cutoff } else { (TAU * cutoff * t).sin() / (PI * t) };
        let w = i as f32 / (TAPS - 1) as f32;
        *tap = sinc * (0.42 - 0.5 * (TAU * w).cos() + 0.08 * (2.0 * TAU * w).cos());
    }
    let sum: f32 = h.iter().sum();
    h.iter_mut().for_each(|tap| *tap /= sum);
    h
}

pub fn apply_distortion(d: &DistortionSettings, left: &mut [f32], right: &mut [f32], sr: f32) {
    Distortion::new(d, sr).process(left, right);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(drive: f32) -> DistortionSettings {
        DistortionSettings { drive, tone_hz: 0.0, ..DistortionSettings::default() }
    }

    fn sine(hz: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (TAU * hz * i as f32 / 48_000.0).sin() * amp).collect()
    }

    #[test]
    fn no_drive_passes_a_quiet_signal_through_delayed() {
        // High enough that the DC blocker leaves its phase alone.
        let x = sine(2000.0, 0.01, 4800);
        let (mut l, mut r) = (x.clone(), x.clone());
        apply_distortion(&settings(0.0), &mut l, &mut r, 48_000.0);
        for i in 1000..4800 {
            assert!((l[i] - x[i - LATENCY]).abs() < 1e-4, "sample {i}: {} vs {}", l[i], x[i - LATENCY]);
        }
    }

    #[test]
    fn drive_adds_odd_harmonics() {
        let x = sine(1000.0, 0.3, 48_000);
        let (mut l, mut r) = (x.clone(), x.clone());
        apply_distortion(&settings(0.7), &mut l, &mut r, 48_000.0);
        let bin = |hz: f32| {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, s) in l.iter().enumerate().skip(4800) {
                let p = TAU * hz * i as f32 / 48_000.0;
                re += s * p.cos();
                im += s * p.sin();
            }
            (re * re + im * im).sqrt()
        };
        assert!(bin(3000.0) > bin(1000.0) * 0.1);
        assert!(bin(2000.0) < bin(1000.0) * 0.01);
    }

    #[test]
    fn stretches_are_the_whole() {
        let x = sine(330.0, 0.5, 9000);
        let d = DistortionSettings { drive: 0.8, bits: 8, rate_hz: 11_025.0, mix: 0.7, ..DistortionSettings::default() };
        let (mut l, mut r) = (x.clone(), x.clone());
        apply_distortion(&d, &mut l, &mut r, 48_000.0);
        let (mut sl, mut sr) = (x.clone(), x);
        let mut dist = Distortion::new(&d, 48_000.0);
        for (a, b) in sl.chunks_mut(777).zip(sr.chunks_mut(777)) {
            dist.process(a, b);
        }
        assert_eq!(l, sl);
    }
}
