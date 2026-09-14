//! Tempo-synced stereo ping-pong delay with a darkening feedback path.

use super::filter::OnePole;
use super::quiet::{BLOCK, QUIET, is_silent};

pub struct PingPongDelay {
    left: Vec<f32>,
    right: Vec<f32>,
    pos: usize,
    feedback: f32,
    tone: [OnePole; 2],
    highpass: [OnePole; 2],
    mod_depth: f32,
    mod_step: f32,
    mod_phase: f32,
    sample_rate: f32,
    /// Every buffer and filter is exactly zero, so silent input is silent
    /// output and only the modulation's phase has to move. See `dsp::quiet`.
    quiet: bool,
}

impl PingPongDelay {
    pub fn new(delay_seconds: f64, feedback: f32, tone_hz: f32, sample_rate: f32) -> Self {
        let len = ((delay_seconds * sample_rate as f64) as usize).max(1);
        Self {
            left: vec![0.0; len],
            right: vec![0.0; len],
            pos: 0,
            feedback,
            tone: [OnePole::new(tone_hz, sample_rate), OnePole::new(tone_hz, sample_rate)],
            highpass: [OnePole::new(120.0, sample_rate), OnePole::new(120.0, sample_rate)],
            mod_depth: 0.0,
            mod_step: 0.0,
            mod_phase: 0.0,
            sample_rate,
            quiet: true,
        }
    }

    /// Modulates the delay time by ± `ms` at `rate_hz` (chorused echoes).
    pub fn set_modulation(&mut self, ms: f32, rate_hz: f32) {
        self.mod_depth = ms / 1000.0 * self.sample_rate;
        self.mod_step = rate_hz / self.sample_rate;
        if self.mod_depth > 0.0 {
            let extra = self.mod_depth.ceil() as usize + 2;
            self.left.resize(self.left.len() + extra, 0.0);
            self.right.resize(self.right.len() + extra, 0.0);
        }
    }

    /// Reads `delay` samples back from the write position with linear interpolation.
    #[inline]
    fn tap(buf: &[f32], pos: usize, delay: f32) -> f32 {
        let n = buf.len();
        let d = delay.clamp(1.0, n as f32 - 2.0);
        let i = d.floor() as usize;
        let frac = d - i as f32;
        let a = buf[(pos + n - i) % n];
        let b = buf[(pos + n - i - 1) % n];
        a + (b - a) * frac
    }

    pub fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len();
        let mut start = 0;
        while start < n {
            let end = (start + BLOCK).min(n);
            let silent_in = is_silent(in_l, in_r, start, end);
            if self.quiet && silent_in {
                if self.mod_depth > 0.0 {
                    self.mod_phase = (self.mod_phase + self.mod_step * (end - start) as f32) % 1.0;
                }
                out_l[start..end].fill(0.0);
                out_r[start..end].fill(0.0);
                start = end;
                continue;
            }
            self.quiet = false;
            let mut peak = 0.0f32;
            for i in start..end {
                let x = 0.5 * (in_l.get(i).copied().unwrap_or(0.0) + in_r.get(i).copied().unwrap_or(0.0));
                let (l, r) = self.step(x);
                out_l[i] = l;
                out_r[i] = r;
                peak = peak.max(l.abs()).max(r.abs());
            }
            if silent_in && peak < QUIET && self.state_peak() < QUIET {
                self.clear();
            }
            start = end;
        }
    }

    #[inline]
    fn step(&mut self, x: f32) -> (f32, f32) {
        let base = if self.mod_depth > 0.0 { (self.left.len() as f32 - self.mod_depth - 2.0).max(1.0) } else { 0.0 };
        let (dl, dr) = if self.mod_depth > 0.0 {
            self.mod_phase = (self.mod_phase + self.mod_step) % 1.0;
            let m = (self.mod_phase * std::f32::consts::TAU).sin() * self.mod_depth;
            (Self::tap(&self.left, self.pos, base + m), Self::tap(&self.right, self.pos, base - m))
        } else {
            (self.left[self.pos], self.right[self.pos])
        };
        let fl = self.highpass[0].highpass(self.tone[0].lowpass(dr * self.feedback));
        let fr = self.highpass[1].highpass(self.tone[1].lowpass(dl * self.feedback));
        self.left[self.pos] = x + fl;
        self.right[self.pos] = fr;
        self.pos = (self.pos + 1) % self.left.len();
        (dl, dr)
    }

    fn state_peak(&self) -> f32 {
        let filters = self.tone.iter().chain(&self.highpass).map(|f| f.state().abs());
        self.left.iter().chain(&self.right).map(|s| s.abs()).chain(filters).fold(0.0, f32::max)
    }

    fn clear(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.tone.iter_mut().chain(self.highpass.iter_mut()).for_each(OnePole::reset);
        self.quiet = true;
    }
}
