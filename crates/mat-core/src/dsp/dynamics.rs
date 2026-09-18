//! Bus compressor and tape-style saturation for the master.

use crate::dsp::filter::Svf;
use crate::model::{CompSettings, FilterMode, MasterSidechain};

/// A master sidechain and the envelope it carries. Kept, it ducks a signal
/// that arrives a stretch at a time — which is how `crate::stream` renders —
/// to the samples one call over the whole signal produces.
pub struct KeyedCompressor {
    settings: MasterSidechain,
    attack: f32,
    release: f32,
    ratio: f32,
    env_db: f32,
    lp: [Svf; 2],
    sr: f32,
}

impl KeyedCompressor {
    pub fn new(sc: &MasterSidechain, sr: f32) -> Self {
        KeyedCompressor {
            settings: sc.clone(),
            attack: (-1.0 / (sc.attack.max(0.0002) * sr)).exp(),
            release: (-1.0 / (sc.release.max(0.005) * sr)).exp(),
            ratio: sc.ratio.max(1.0),
            env_db: 0.0,
            lp: [Svf::default(), Svf::default()],
            sr,
        }
    }

    pub fn process(&mut self, key_l: &[f32], key_r: &[f32], left: &mut [f32], right: &mut [f32]) {
        let (sc, sr) = (&self.settings, self.sr);
        for i in 0..left.len() {
            let peak = key_l.get(i).copied().unwrap_or(0.0).abs().max(key_r.get(i).copied().unwrap_or(0.0).abs());
            let over = 20.0 * peak.max(1e-9).log10() - sc.threshold_db;
            let target = if over > 0.0 { -over * (1.0 - 1.0 / self.ratio) } else { 0.0 };
            let coef = if target < self.env_db { self.attack } else { self.release };
            self.env_db = target + (self.env_db - target) * coef;
            let g = 10f32.powf(self.env_db / 20.0);
            left[i] *= g;
            right[i] *= g;
            if sc.darken > 0.0 {
                let cutoff = (18_000.0 * 2f32.powf(sc.darken * self.env_db / 12.0)).max(200.0);
                for (f, s) in self.lp.iter_mut().zip([&mut left[i], &mut right[i]]) {
                    f.set(cutoff, 0.0, sr);
                    *s = f.process(*s, FilterMode::Lowpass);
                }
            }
        }
    }
}

/// Compresses `left`/`right` with the key signal as the detector. With
/// `darken`, a lowpass on the compressed signal closes along with the gain.
pub fn keyed_compress(sc: &MasterSidechain, key_l: &[f32], key_r: &[f32], left: &mut [f32], right: &mut [f32], sr: f32) {
    KeyedCompressor::new(sc, sr).process(key_l, key_r, left, right);
}

/// A compressor and the envelope it carries. Kept, it compresses a signal that
/// arrives a stretch at a time — which is how `crate::stream` renders — to the
/// samples one call over the whole signal produces.
pub struct Compressor {
    settings: CompSettings,
    attack: f32,
    release: f32,
    ratio: f32,
    makeup: f32,
    env_db: f32,
    last_gain: f32,
}

impl Compressor {
    pub fn new(settings: &CompSettings, sr: f32) -> Self {
        Compressor {
            settings: settings.clone(),
            attack: (-1.0 / (settings.attack.max(0.0005) * sr)).exp(),
            release: (-1.0 / (settings.release.max(0.005) * sr)).exp(),
            ratio: settings.ratio.max(1.0),
            makeup: 10f32.powf(settings.makeup_db / 20.0),
            // The gain the compressor is currently applying, in dB, not the
            // level it has heard: silence means nothing to turn down, so it
            // starts at unity. Starting it at -120 dB made every compressor
            // open from silence over its release time — a fade-in on the first
            // note of any track with `comp`, and on the first second of any
            // song whose master has one. `KeyedCompressor` starts at 0 too.
            env_db: 0.0,
            last_gain: 1.0,
        }
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let settings = &self.settings;
        let knee = 6.0f32;
        for i in 0..left.len() {
            let mut peak = left[i].abs().max(right[i].abs());
            if settings.feedback {
                // Detect after the gain stage: the previous gain shapes what the detector sees.
                peak *= self.last_gain;
            }
            let level_db = 20.0 * peak.max(1e-9).log10();
            let over = level_db - settings.threshold_db;
            // Soft knee: quadratic transition around the threshold.
            let reduction = if over <= -knee / 2.0 {
                0.0
            } else if over >= knee / 2.0 {
                over * (1.0 - 1.0 / self.ratio)
            } else {
                (1.0 - 1.0 / self.ratio) * (over + knee / 2.0).powi(2) / (2.0 * knee)
            };
            let target = -reduction;
            let coef = if target < self.env_db { self.attack } else { self.release };
            self.env_db = target + (self.env_db - target) * coef;
            let g = 10f32.powf(self.env_db / 20.0);
            self.last_gain = g;
            left[i] *= g * self.makeup;
            right[i] *= g * self.makeup;
        }
    }
}

/// Feed-forward stereo compressor with a peak detector, soft knee and
/// smoothed gain; the gain is linked between channels.
pub fn compress(settings: &CompSettings, left: &mut [f32], right: &mut [f32], sr: f32) {
    Compressor::new(settings, sr).process(left, right);
}

/// Soft clipper: linear below the threshold, a smooth knee above it that
/// never exceeds the threshold by more than about 2 dB.
pub fn clip(threshold_db: f32, left: &mut [f32], right: &mut [f32]) {
    let t = 10f32.powf(threshold_db / 20.0);
    for s in left.iter_mut().chain(right.iter_mut()) {
        let a = s.abs();
        if a > t {
            let over = (a - t) / t;
            *s = s.signum() * t * (1.0 + over.tanh() * 0.25);
        }
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
