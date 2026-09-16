//! Look-ahead brickwall limiter.
//!
//! The gain curve is computed from the future: a sliding minimum over the
//! look-ahead window followed by a box filter of the same length yields a
//! smooth gain that is guaranteed to stay below the required gain at every
//! peak. That costs one look-ahead window of latency and nothing else, so the
//! same limiter runs over a signal that arrives a stretch at a time as over
//! one that is all there — see [`Limiter`].

use std::collections::VecDeque;

/// The limiter and everything it carries between samples: the samples held for
/// the look-ahead, the sliding minimum over them, the box filter's window and
/// the gain.
///
/// Feed it with [`Limiter::push`] and take what is ready with
/// [`Limiter::take`]; [`Limiter::finish`] releases the last look-ahead window,
/// whose window is short because the signal has ended. What comes out is what
/// [`limit`] writes over the whole signal — it is that function, kept.
pub struct Limiter {
    ceiling: f32,
    look: usize,
    release: f32,
    /// Pushed and not yet limited, oldest first: `next_out .. next_in`.
    held: VecDeque<(f32, f32)>,
    /// Indices and their required gain, rising, for the sliding minimum.
    rising: VecDeque<(usize, f32)>,
    /// The last `look` window minima, for the box filter, and their sum.
    window: VecDeque<f32>,
    sum: f64,
    gain: f32,
    next_in: usize,
    next_out: usize,
    /// Limited and waiting to be taken.
    ready: Vec<(f32, f32)>,
}

impl Limiter {
    pub fn new(ceiling_db: f32, release_ms: f32, sample_rate: f32) -> Self {
        Limiter {
            ceiling: 10f32.powf(ceiling_db / 20.0),
            look: ((0.005 * sample_rate) as usize).max(1),
            release: (-1.0 / (release_ms.max(1.0) / 1000.0 * sample_rate)).exp(),
            held: VecDeque::new(),
            rising: VecDeque::new(),
            window: VecDeque::new(),
            sum: 0.0,
            gain: 1.0,
            next_in: 0,
            next_out: 0,
            ready: Vec::new(),
        }
    }

    /// How many samples the look-ahead holds back.
    pub fn latency(&self) -> usize {
        self.look
    }

    pub fn push(&mut self, l: f32, r: f32) {
        let peak = l.abs().max(r.abs());
        let required = if peak > self.ceiling { self.ceiling / peak } else { 1.0 };
        while self.rising.back().is_some_and(|&(_, back)| back >= required) {
            self.rising.pop_back();
        }
        self.rising.push_back((self.next_in, required));
        self.held.push_back((l, r));
        self.next_in += 1;
        if self.next_in >= self.next_out + self.look {
            self.emit();
        }
    }

    /// Limits sample `next_out`, whose look-ahead window has arrived — or, once
    /// the signal has ended, as much of that window as there is.
    fn emit(&mut self) {
        let i = self.next_out;
        while self.rising.front().is_some_and(|&(j, _)| j < i) {
            self.rising.pop_front();
        }
        let window_min = self.rising.front().expect("a sample in the window").1;
        self.sum += window_min as f64;
        self.window.push_back(window_min);
        if self.window.len() > self.look {
            self.sum -= self.window.pop_front().expect("a full window") as f64;
        }
        // Before the window is full, pad with unity gain.
        let count = self.window.len();
        let avg = ((self.sum + (self.look - count) as f64) / self.look as f64) as f32;
        let avg = avg.min(window_min);
        self.gain = if avg < self.gain { avg } else { avg + (self.gain - avg) * self.release };
        let (l, r) = self.held.pop_front().expect("a held sample");
        self.ready.push(((l * self.gain).clamp(-self.ceiling, self.ceiling), (r * self.gain).clamp(-self.ceiling, self.ceiling)));
        self.next_out += 1;
    }

    /// The signal has ended: the last look-ahead window's worth comes out.
    pub fn finish(&mut self) {
        while self.next_out < self.next_in {
            self.emit();
        }
    }

    /// Takes everything limited so far, appending it to `left` and `right`.
    pub fn take(&mut self, left: &mut Vec<f32>, right: &mut Vec<f32>) {
        for (l, r) in self.ready.drain(..) {
            left.push(l);
            right.push(r);
        }
    }
}

pub fn limit(left: &mut [f32], right: &mut [f32], ceiling_db: f32, release_ms: f32, sample_rate: f32) {
    let n = left.len();
    if n == 0 {
        return;
    }
    let mut limiter = Limiter::new(ceiling_db, release_ms, sample_rate);
    let (mut out_l, mut out_r) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for i in 0..n {
        limiter.push(left[i], right[i]);
        limiter.take(&mut out_l, &mut out_r);
    }
    limiter.finish();
    limiter.take(&mut out_l, &mut out_r);
    left.copy_from_slice(&out_l);
    right.copy_from_slice(&out_r);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> Vec<f32> {
        (0..48000).map(|i| (i as f32 * 0.05).sin() * (1.0 + (i % 5000) as f32 / 1000.0)).collect()
    }

    #[test]
    fn never_exceeds_ceiling() {
        let mut l = ramp();
        let mut r = l.clone();
        limit(&mut l, &mut r, -1.0, 50.0, 48000.0);
        let ceiling = 10f32.powf(-1.0 / 20.0);
        assert!(l.iter().chain(r.iter()).all(|s| s.abs() <= ceiling + 1e-6));
    }

    /// A signal fed a stretch at a time is the signal fed whole, sample for
    /// sample — which is what lets a streamed render end in the same master as
    /// an ordinary one.
    #[test]
    fn a_signal_in_stretches_is_limited_as_one_in_full() {
        let (source_l, source_r): (Vec<f32>, Vec<f32>) = (ramp(), ramp().iter().map(|s| -s * 0.8).collect());
        let (mut whole_l, mut whole_r) = (source_l.clone(), source_r.clone());
        limit(&mut whole_l, &mut whole_r, -1.0, 50.0, 48000.0);

        let mut limiter = Limiter::new(-1.0, 50.0, 48000.0);
        let (mut got_l, mut got_r) = (Vec::new(), Vec::new());
        let mut at = 0;
        for length in [1usize, 7, 1024, 4096, 30_000, 48_000] {
            let to = (at + length).min(source_l.len());
            for i in at..to {
                limiter.push(source_l[i], source_r[i]);
            }
            at = to;
            limiter.take(&mut got_l, &mut got_r);
        }
        limiter.finish();
        limiter.take(&mut got_l, &mut got_r);
        assert_eq!(got_l, whole_l);
        assert_eq!(got_r, whole_r);
    }
}
