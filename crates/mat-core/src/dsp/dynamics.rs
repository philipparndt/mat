//! Bus compressor and tape-style saturation for the master.

use crate::model::CompSettings;

/// Feed-forward stereo compressor with a peak detector, soft knee and
/// smoothed gain; the gain is linked between channels.
pub fn compress(settings: &CompSettings, left: &mut [f32], right: &mut [f32], sr: f32) {
    let attack = (-1.0 / (settings.attack.max(0.0005) * sr)).exp();
    let release = (-1.0 / (settings.release.max(0.005) * sr)).exp();
    let knee = 6.0f32;
    let ratio = settings.ratio.max(1.0);
    let makeup = 10f32.powf(settings.makeup_db / 20.0);
    let mut env_db = -120.0f32;
    for i in 0..left.len() {
        let peak = left[i].abs().max(right[i].abs());
        let level_db = 20.0 * peak.max(1e-9).log10();
        let over = level_db - settings.threshold_db;
        // Soft knee: quadratic transition around the threshold.
        let reduction = if over <= -knee / 2.0 {
            0.0
        } else if over >= knee / 2.0 {
            over * (1.0 - 1.0 / ratio)
        } else {
            (1.0 - 1.0 / ratio) * (over + knee / 2.0).powi(2) / (2.0 * knee)
        };
        let target = -reduction;
        let coef = if target < env_db { attack } else { release };
        env_db = target + (env_db - target) * coef;
        let g = 10f32.powf(env_db / 20.0) * makeup;
        left[i] *= g;
        right[i] *= g;
    }
}

/// Gentle tape-style saturation: soft clipping with a touch of even
/// harmonics, level-compensated so the perceived loudness stays put.
pub fn saturate(amount: f32, left: &mut [f32], right: &mut [f32]) {
    if amount <= 0.0 {
        return;
    }
    let drive = 1.0 + amount * 3.0;
    let norm = 1.0 / drive.tanh();
    let asym = amount * 0.08;
    for s in left.iter_mut().chain(right.iter_mut()) {
        let x = *s * drive;
        *s = ((x + asym * x * x).tanh() - asym.tanh() * 0.0) * norm;
    }
}
