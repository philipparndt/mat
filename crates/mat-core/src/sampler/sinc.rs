//! Band-limited interpolation with a Kaiser-windowed sinc kernel. The kernel
//! is widened when a sample plays faster than its native rate, so pitching up
//! does not alias.

use std::sync::OnceLock;

/// Zero crossings on each side of the kernel center.
pub const HALF_WIDTH: usize = 16;
const RESOLUTION: usize = 1024;
const BETA: f64 = 8.6;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..50 {
        term *= (half / k as f64).powi(2);
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

fn table() -> &'static [f32] {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let n = HALF_WIDTH * RESOLUTION + 2;
        let norm = bessel_i0(BETA);
        (0..n)
            .map(|i| {
                let x = i as f64 / RESOLUTION as f64;
                let sinc = if i == 0 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
                let w = (x / HALF_WIDTH as f64).min(1.0);
                let window = bessel_i0(BETA * (1.0 - w * w).max(0.0).sqrt()) / norm;
                (sinc * window) as f32
            })
            .collect()
    })
}

/// Interpolates `data` at fractional position `pos`. `cutoff` is 1.0 at
/// native speed and `1 / speed` when playing faster.
#[inline]
pub fn interpolate(data: &[f32], pos: f64, cutoff: f32) -> f32 {
    let table = table();
    let reach = (HALF_WIDTH as f32 / cutoff).ceil() as i64;
    let center = pos.floor() as i64;
    let frac = (pos - center as f64) as f32;
    let scale = cutoff * RESOLUTION as f32;
    let mut sum = 0.0f32;
    let lo = (center - reach + 1).max(0);
    let hi = (center + reach).min(data.len() as i64 - 1);
    for k in lo..=hi {
        let distance = ((k - center) as f32 - frac).abs() * scale;
        let idx = distance as usize;
        if idx + 1 >= table.len() {
            continue;
        }
        let t = distance - idx as f32;
        let h = table[idx] + (table[idx + 1] - table[idx]) * t;
        sum += data[k as usize] * h;
    }
    sum * cutoff
}

#[cfg(test)]
mod tests {
    #[test]
    fn reproduces_samples_at_integer_positions() {
        let data: Vec<f32> = (0..64).map(|i| (i as f32 * 0.3).sin()).collect();
        for i in 20..40 {
            let v = super::interpolate(&data, i as f64, 1.0);
            assert!((v - data[i]).abs() < 1e-3, "{v} vs {}", data[i]);
        }
    }
}
