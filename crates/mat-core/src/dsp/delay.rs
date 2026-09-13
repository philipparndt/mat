//! Tempo-synced stereo ping-pong delay with a darkening feedback path.

use super::filter::OnePole;

pub struct PingPongDelay {
    left: Vec<f32>,
    right: Vec<f32>,
    pos: usize,
    feedback: f32,
    tone: [OnePole; 2],
    highpass: [OnePole; 2],
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
        }
    }

    pub fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        for i in 0..out_l.len() {
            let x = 0.5 * (in_l.get(i).copied().unwrap_or(0.0) + in_r.get(i).copied().unwrap_or(0.0));
            let dl = self.left[self.pos];
            let dr = self.right[self.pos];
            let fl = self.highpass[0].highpass(self.tone[0].lowpass(dr * self.feedback));
            let fr = self.highpass[1].highpass(self.tone[1].lowpass(dl * self.feedback));
            self.left[self.pos] = x + fl;
            self.right[self.pos] = fr;
            self.pos = (self.pos + 1) % self.left.len();
            out_l[i] = dl;
            out_r[i] = dr;
        }
    }
}
