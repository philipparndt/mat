//! Offline look-ahead brickwall limiter.
//!
//! Because the whole signal is available, the gain curve is computed from the
//! future: a sliding minimum over the look-ahead window followed by a box
//! filter of the same length yields a smooth gain that is guaranteed to stay
//! below the required gain at every peak.

use std::collections::VecDeque;

pub fn limit(left: &mut [f32], right: &mut [f32], ceiling_db: f32, release_ms: f32, sample_rate: f32) {
    let n = left.len();
    if n == 0 {
        return;
    }
    let ceiling = 10f32.powf(ceiling_db / 20.0);
    let look = ((0.005 * sample_rate) as usize).max(1);

    let required: Vec<f32> = (0..n)
        .map(|i| {
            let peak = left[i].abs().max(right[i].abs());
            if peak > ceiling { ceiling / peak } else { 1.0 }
        })
        .collect();

    // Sliding minimum over [i, i + look).
    let mut window_min = vec![1.0f32; n];
    let mut deque: VecDeque<usize> = VecDeque::new();
    for i in (0..n).rev() {
        while deque.back().is_some_and(|&j| required[j] >= required[i]) {
            deque.pop_back();
        }
        deque.push_back(i);
        while deque.front().is_some_and(|&j| j >= i + look) {
            deque.pop_front();
        }
        window_min[i] = required[*deque.front().unwrap()];
    }

    // Box filter over (i - look, i], then release smoothing.
    let release = (-1.0 / (release_ms.max(1.0) / 1000.0 * sample_rate)).exp();
    let mut sum = 0.0f64;
    let mut gain = 1.0f32;
    for i in 0..n {
        sum += window_min[i] as f64;
        let count = if i >= look {
            sum -= window_min[i - look] as f64;
            look
        } else {
            i + 1
        };
        // Before the window is full, pad with unity gain.
        let avg = ((sum + (look - count) as f64) / look as f64) as f32;
        let avg = avg.min(window_min[i]);
        gain = if avg < gain { avg } else { avg + (gain - avg) * release };
        left[i] = (left[i] * gain).clamp(-ceiling, ceiling);
        right[i] = (right[i] * gain).clamp(-ceiling, ceiling);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn never_exceeds_ceiling() {
        let mut l: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.05).sin() * (1.0 + (i % 5000) as f32 / 1000.0)).collect();
        let mut r = l.clone();
        super::limit(&mut l, &mut r, -1.0, 50.0, 48000.0);
        let ceiling = 10f32.powf(-1.0 / 20.0);
        assert!(l.iter().chain(r.iter()).all(|s| s.abs() <= ceiling + 1e-6));
    }
}
