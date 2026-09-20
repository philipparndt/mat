//! How a mix translates: what is left of it on a speaker that is not the one
//! it was mixed on.
//!
//! A [`Device`] is a model of a playback system — what it cannot play, how it
//! colours what it can, how much of the stereo picture it keeps, what its
//! bass protection does when it is loud, what noise it plays into. The models
//! are approximations drawn from published measurements, not the measurements
//! themselves: they answer "does the bass line survive a small speaker", not
//! "what is the Sonos Five's response at 63 Hz".
//!
//! [`radiate`] puts a mix through a device, [`audition`] makes that listenable
//! on the device somebody is wearing, and [`report`] measures what changed.

use rayon::prelude::*;
use serde::Serialize;
use std::f32::consts::FRAC_1_SQRT_2;

use crate::dsp::Rng;
use crate::dsp::biquad::Biquad;
use crate::render::Audio;
use crate::sampler::sinc;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Headphones,
    Speaker,
}

/// One stage of a device's frequency response.
#[derive(Clone, Copy, Debug)]
pub enum Stage {
    /// Butterworth, second or fourth order: where the device stops playing.
    Highpass { hz: f32, order: u8 },
    Lowpass { hz: f32, order: u8 },
    Peak { hz: f32, q: f32, db: f32 },
    LowShelf { hz: f32, db: f32 },
    HighShelf { hz: f32, db: f32 },
}

/// A driver's protection: everything under `below_hz` is turned down while it
/// peaks over `threshold_db`, which is where it is at full volume.
#[derive(Clone, Copy, Debug)]
pub struct BassLimit {
    pub below_hz: f32,
    pub threshold_db: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub name: &'static str,
    pub title: &'static str,
    pub about: &'static str,
    pub kind: Kind,
    pub stages: &'static [Stage],
    /// How much of the side signal is left: 1 is stereo, 0 is one speaker.
    pub width: f32,
    /// One woofer, or one subwoofer: mono under this, whatever the width.
    pub mono_below_hz: f32,
    pub bass_limit: Option<BassLimit>,
    /// The noise the device plays into, as RMS against the music's at normal
    /// volume. Low and rumbling: a road.
    pub noise_db: Option<f32>,
}

/// How loud the device is playing: what its bass protection has to do, and
/// how far the music is above the noise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Volume {
    Quiet,
    Normal,
    Loud,
}

impl Volume {
    /// Headroom under the bass protection's full-volume threshold.
    fn headroom_db(self) -> f32 {
        match self {
            Volume::Quiet => 60.0,
            Volume::Normal => 8.0,
            Volume::Loud => 0.0,
        }
    }

    fn noise_db(self) -> f32 {
        match self {
            Volume::Quiet => 6.0,
            Volume::Normal => 0.0,
            Volume::Loud => -6.0,
        }
    }
}

use Stage::*;

pub const DEVICES: &[Device] = &[
    Device {
        name: "airpods-max",
        title: "AirPods Max",
        about: "closed headphones: full range down to 20 Hz, lifted lows, perfect separation",
        kind: Kind::Headphones,
        stages: &[LowShelf { hz: 100.0, db: 4.0 }, Peak { hz: 4500.0, q: 1.0, db: -2.0 }],
        width: 1.0,
        mono_below_hz: 0.0,
        bass_limit: None,
        noise_db: None,
    },
    Device {
        name: "earbuds",
        title: "In-ear buds (AirPods Pro and the like)",
        about: "sealed in-ears: full range, a little bass lift, perfect separation",
        kind: Kind::Headphones,
        stages: &[Highpass { hz: 20.0, order: 2 }, LowShelf { hz: 100.0, db: 3.0 }, Peak { hz: 3000.0, q: 1.2, db: 1.5 }],
        width: 1.0,
        mono_below_hz: 0.0,
        bass_limit: None,
        noise_db: None,
    },
    Device {
        name: "monitors",
        title: "Studio monitors",
        about: "flat nearfields in a room: the reference, with both speakers reaching both ears",
        kind: Kind::Speaker,
        stages: &[Highpass { hz: 42.0, order: 2 }],
        width: 1.0,
        mono_below_hz: 0.0,
        bass_limit: None,
        noise_db: None,
    },
    Device {
        name: "sonos-five",
        title: "Sonos Five",
        about: "one box: narrow stereo, mono lows, nothing under 35 Hz, loudness contour, bass protection",
        kind: Kind::Speaker,
        stages: &[
            Highpass { hz: 36.0, order: 4 },
            LowShelf { hz: 90.0, db: 2.5 },
            Peak { hz: 300.0, q: 0.8, db: 1.5 },
            HighShelf { hz: 9000.0, db: -2.0 },
        ],
        width: 0.35,
        mono_below_hz: 150.0,
        bass_limit: Some(BassLimit { below_hz: 120.0, threshold_db: -12.0 }),
        noise_db: None,
    },
    Device {
        name: "smart-speaker",
        title: "Small smart speaker (Sonos One, HomePod mini, Echo)",
        about: "one small driver: mono, nothing under 60 Hz, a bump above it to fake the rest",
        kind: Kind::Speaker,
        stages: &[
            Highpass { hz: 60.0, order: 4 },
            Peak { hz: 115.0, q: 1.0, db: 3.0 },
            HighShelf { hz: 8000.0, db: -3.0 },
        ],
        width: 0.0,
        mono_below_hz: 0.0,
        bass_limit: Some(BassLimit { below_hz: 150.0, threshold_db: -16.0 }),
        noise_db: None,
    },
    Device {
        name: "bluetooth",
        title: "Portable Bluetooth speaker",
        about: "mono, nothing under 80 Hz, boomy above it, hard bass protection",
        kind: Kind::Speaker,
        stages: &[
            Highpass { hz: 80.0, order: 4 },
            Peak { hz: 140.0, q: 1.2, db: 4.0 },
            Peak { hz: 2500.0, q: 1.0, db: 2.0 },
            HighShelf { hz: 10000.0, db: -4.0 },
        ],
        width: 0.0,
        mono_below_hz: 0.0,
        bass_limit: Some(BassLimit { below_hz: 200.0, threshold_db: -18.0 }),
        noise_db: None,
    },
    Device {
        name: "car",
        title: "Typical car, on the road",
        about: "cabin gain in the lows, a hole in the low mids, hard upper mids, road noise over all of it",
        kind: Kind::Speaker,
        stages: &[
            Highpass { hz: 35.0, order: 2 },
            LowShelf { hz: 80.0, db: 5.0 },
            Peak { hz: 250.0, q: 1.0, db: -3.0 },
            Peak { hz: 3000.0, q: 1.2, db: 3.0 },
            HighShelf { hz: 8000.0, db: -3.0 },
        ],
        width: 0.6,
        mono_below_hz: 80.0,
        bass_limit: None,
        noise_db: Some(-10.0),
    },
    Device {
        name: "laptop",
        title: "Laptop speakers",
        about: "nothing under 120 Hz, forward upper mids, some stereo",
        kind: Kind::Speaker,
        stages: &[
            Highpass { hz: 120.0, order: 4 },
            Peak { hz: 220.0, q: 1.0, db: 2.0 },
            Peak { hz: 3000.0, q: 1.0, db: 2.5 },
            HighShelf { hz: 12000.0, db: -3.0 },
        ],
        width: 0.5,
        mono_below_hz: 0.0,
        bass_limit: Some(BassLimit { below_hz: 300.0, threshold_db: -16.0 }),
        noise_db: None,
    },
    Device {
        name: "tv",
        title: "Television speakers",
        about: "nothing under 100 Hz, boxy low mids, dull top, narrow",
        kind: Kind::Speaker,
        stages: &[
            Highpass { hz: 100.0, order: 4 },
            Peak { hz: 250.0, q: 1.0, db: 3.0 },
            Peak { hz: 2000.0, q: 1.0, db: 2.0 },
            HighShelf { hz: 6000.0, db: -4.0 },
        ],
        width: 0.4,
        mono_below_hz: 0.0,
        bass_limit: None,
        noise_db: None,
    },
    Device {
        name: "phone",
        title: "Phone speaker",
        about: "a band from 350 Hz to 12 kHz with a peak at 3.5 kHz, all but mono: no bass at all",
        kind: Kind::Speaker,
        stages: &[
            Highpass { hz: 350.0, order: 4 },
            Peak { hz: 3500.0, q: 1.0, db: 5.0 },
            Lowpass { hz: 12000.0, order: 2 },
        ],
        width: 0.1,
        mono_below_hz: 0.0,
        bass_limit: None,
        noise_db: None,
    },
    Device {
        name: "club",
        title: "Club PA",
        about: "mono subs with a lot of level, and a dance floor where nobody stands between the speakers",
        kind: Kind::Speaker,
        stages: &[
            Highpass { hz: 30.0, order: 4 },
            LowShelf { hz: 80.0, db: 6.0 },
            Peak { hz: 3000.0, q: 1.0, db: 2.0 },
            HighShelf { hz: 10000.0, db: -2.0 },
        ],
        width: 0.4,
        mono_below_hz: 120.0,
        bass_limit: None,
        noise_db: None,
    },
    Device {
        name: "mono",
        title: "Mono sum",
        about: "left plus right and nothing else: the oldest check there is",
        kind: Kind::Speaker,
        stages: &[],
        width: 0.0,
        mono_below_hz: 0.0,
        bass_limit: None,
        noise_db: None,
    },
];

pub fn device(name: &str) -> Option<&'static Device> {
    DEVICES.iter().find(|d| d.name.eq_ignore_ascii_case(name))
}

impl Device {
    /// The lowest frequency the device plays, if it stops anywhere.
    pub fn plays_from_hz(&self) -> Option<f32> {
        self.stages.iter().find_map(|s| match s {
            Highpass { hz, .. } => Some(*hz),
            _ => None,
        })
    }
}

fn butterworth(high: bool, hz: f32, order: u8, sr: f32) -> Vec<Biquad> {
    let qs: &[f32] = if order >= 4 { &[0.541_196, 1.306_563] } else { &[FRAC_1_SQRT_2] };
    qs.iter().map(|q| if high { Biquad::highpass(hz, *q, sr) } else { Biquad::lowpass(hz, *q, sr) }).collect()
}

fn filters(stages: &[Stage], sr: f32) -> Vec<Biquad> {
    stages
        .iter()
        .flat_map(|stage| match *stage {
            Highpass { hz, order } => butterworth(true, hz, order, sr),
            Lowpass { hz, order } => butterworth(false, hz, order, sr),
            Peak { hz, q, db } => vec![Biquad::peak(hz, q, db, sr)],
            LowShelf { hz, db } => vec![Biquad::low_shelf(hz, db, sr)],
            HighShelf { hz, db } => vec![Biquad::high_shelf(hz, db, sr)],
        })
        .collect()
}

fn run(filters: &[Biquad], channel: &mut [f32]) {
    let mut filters = filters.to_vec();
    for s in channel.iter_mut() {
        for f in filters.iter_mut() {
            *s = f.process(*s);
        }
    }
}

fn copy(audio: &Audio) -> Audio {
    Audio { sample_rate: audio.sample_rate, left: audio.left.clone(), right: audio.right.clone() }
}

/// The device's linear part: its width and its frequency response. What a
/// layer goes through on its own, since the sum of the layers through it is
/// the mix through it.
pub fn linear(device: &Device, audio: &Audio) -> Audio {
    let mut out = copy(audio);
    let sr = audio.sample_rate as f32;
    if device.width < 1.0 || device.mono_below_hz > 0.0 {
        let mut highpass = if device.mono_below_hz > 0.0 { butterworth(true, device.mono_below_hz, 4, sr) } else { Vec::new() };
        for (l, r) in out.left.iter_mut().zip(out.right.iter_mut()) {
            let mid = (*l + *r) * 0.5;
            let mut side = (*l - *r) * 0.5 * device.width;
            for f in highpass.iter_mut() {
                side = f.process(side);
            }
            *l = mid + side;
            *r = mid - side;
        }
    }
    let stages = filters(device.stages, sr);
    run(&stages, &mut out.left);
    run(&stages, &mut out.right);
    out
}

/// What the bass protection did over the mix.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct BassReduction {
    pub below_hz: f32,
    pub max_db: f32,
    /// The share of the mix's length it was down by more than 1 dB.
    pub share: f32,
}

pub struct Radiated {
    pub audio: Audio,
    pub bass: Option<BassReduction>,
}

/// The mix as the device puts it into the room at this volume.
pub fn radiate(device: &Device, audio: &Audio, volume: Volume) -> Radiated {
    let mut out = linear(device, audio);
    let bass = device.bass_limit.map(|limit| protect(&limit, volume, &mut out));
    Radiated { audio: out, bass }
}

fn protect(limit: &BassLimit, volume: Volume, audio: &mut Audio) -> BassReduction {
    let sr = audio.sample_rate as f32;
    let threshold = crate::dsp::db_to_gain(limit.threshold_db + volume.headroom_db());
    let mut low_l = audio.left.clone();
    let mut low_r = audio.right.clone();
    let lowpass = butterworth(false, limit.below_hz, 4, sr);
    run(&lowpass, &mut low_l);
    run(&lowpass, &mut low_r);
    let attack = (-1.0 / (0.001 * sr)).exp();
    let release = (-1.0 / (0.200 * sr)).exp();
    let (mut env, mut min_gain, mut working) = (0.0f32, 1.0f32, 0usize);
    for i in 0..low_l.len() {
        let level = low_l[i].abs().max(low_r[i].abs());
        let coef = if level > env { attack } else { release };
        env = level + (env - level) * coef;
        let gain = if env > threshold { threshold / env } else { 1.0 };
        // The rest of the signal is the signal less its lows, so a protection
        // that does nothing changes nothing.
        audio.left[i] += (gain - 1.0) * low_l[i];
        audio.right[i] += (gain - 1.0) * low_r[i];
        min_gain = min_gain.min(gain);
        working += usize::from(gain < 0.891);
    }
    BassReduction { below_hz: limit.below_hz, max_db: -20.0 * min_gain.log10(), share: working as f32 / low_l.len().max(1) as f32 }
}

/// Road noise as long as `frames`, at `rms` — low and rumbling, a little of it
/// reaching into the mids.
fn road_noise(frames: usize, rms: f32, sr: f32, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut noise: Vec<f32> = (0..frames).map(|_| rng.bipolar()).collect();
    run(&[Biquad::low_shelf(200.0, 24.0, sr), Biquad::highpass(25.0, FRAC_1_SQRT_2, sr), Biquad::lowpass(4000.0, FRAC_1_SQRT_2, sr)], &mut noise);
    let power = noise.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / frames.max(1) as f64;
    let gain = rms / (power.sqrt() as f32).max(1e-9);
    noise.iter_mut().for_each(|s| *s *= gain);
    noise
}

fn noise_rms(device: &Device, radiated: &Audio, volume: Volume) -> Option<f32> {
    device.noise_db.map(|db| crate::dsp::db_to_gain(radiated.rms_db() + db + volume.noise_db()))
}

/// What is heard of a radiated mix by somebody listening on `monitor`: the
/// noise the device plays into is added; a speaker heard on headphones
/// reaches both ears, as it does in a room; and the monitor's own colour is
/// taken out, so it is not heard twice. What the monitor cannot play stays
/// unplayed — see [`monitor_limit`].
pub fn audition(device: &Device, radiated: &Audio, monitor: Option<&Device>, volume: Volume) -> Audio {
    let mut out = copy(radiated);
    let sr = out.sample_rate as f32;
    let frames = out.left.len();
    if let Some(rms) = noise_rms(device, radiated, volume) {
        for (channel, seed) in [(&mut out.left, 0x0ca5), (&mut out.right, 0x0ca6)] {
            for (s, n) in channel.iter_mut().zip(road_noise(frames, rms, sr, seed)) {
                *s += n;
            }
        }
    }
    let Some(monitor) = monitor else { return out };
    if monitor.kind == Kind::Headphones && device.kind == Kind::Speaker {
        crossfeed(&mut out);
    }
    let inverse: Vec<Stage> = monitor
        .stages
        .iter()
        .filter_map(|stage| match *stage {
            Peak { hz, q, db } => Some(Peak { hz, q, db: -db }),
            LowShelf { hz, db } => Some(LowShelf { hz, db: -db }),
            HighShelf { hz, db } => Some(HighShelf { hz, db: -db }),
            Highpass { .. } | Lowpass { .. } => None,
        })
        .collect();
    let inverse = filters(&inverse, sr);
    run(&inverse, &mut out.left);
    run(&inverse, &mut out.right);
    out
}

/// Each ear hears the far speaker too: later, quieter and without its highs,
/// which the head is in the way of.
fn crossfeed(audio: &mut Audio) {
    let sr = audio.sample_rate as f32;
    let delay = ((0.000_27 * sr) as usize).max(1);
    let shadow = [Biquad::lowpass(700.0, FRAC_1_SQRT_2, sr)];
    let (mut far_l, mut far_r) = (audio.right.clone(), audio.left.clone());
    run(&shadow, &mut far_l);
    run(&shadow, &mut far_r);
    for i in (delay..audio.left.len()).rev() {
        audio.left[i] += 0.5 * far_l[i - delay];
        audio.right[i] += 0.5 * far_r[i - delay];
    }
    // The lows arrive twice now; the highs once.
    let level = [Biquad::low_shelf(700.0, -2.5, sr)];
    run(&level, &mut audio.left);
    run(&level, &mut audio.right);
}

/// Said when the monitor itself cannot play what a simulation might contain.
pub fn monitor_limit(monitor: &Device) -> Option<String> {
    let from = monitor.plays_from_hz().filter(|hz| *hz > 45.0)?;
    Some(format!(
        "{} plays nothing under {from:.0} Hz, so no simulation heard on it says anything about the bass below that; the numbers still do",
        monitor.title
    ))
}

/// Integrated loudness after ITU-R BS.1770: K-weighted, in 400 ms blocks,
/// gated at -70 LUFS and at 10 LU under what is left. -120 for silence.
pub fn loudness(audio: &Audio) -> f32 {
    let sr = audio.sample_rate as f32;
    let hop = ((0.1 * sr) as usize).max(1);
    let weighting = [Biquad::high_shelf(1681.97, 4.0, sr), Biquad::highpass(38.135, 0.5, sr)];
    let hops = audio.left.len() / hop;
    let mut energy = vec![0.0f64; hops.max(1)];
    let last = energy.len() - 1;
    for channel in [&audio.left, &audio.right] {
        let mut k = weighting;
        for (i, s) in channel.iter().enumerate().take(hops.max(1) * hop) {
            let y = k.iter_mut().fold(*s, |x, f| f.process(x));
            let at = (i / hop).min(last);
            energy[at] += (y as f64).powi(2);
        }
    }
    let blocks: Vec<f64> = match energy.len() {
        0..4 => vec![energy.iter().sum::<f64>() / audio.left.len().max(1) as f64],
        _ => energy.windows(4).map(|w| w.iter().sum::<f64>() / (4 * hop) as f64).collect(),
    };
    let lufs = |z: f64| -0.691 + 10.0 * z.max(1e-12).log10();
    let mean = |over: f64| {
        let kept: Vec<f64> = blocks.iter().copied().filter(|z| lufs(*z) > over).collect();
        (!kept.is_empty()).then(|| kept.iter().sum::<f64>() / kept.len() as f64)
    };
    let Some(ungated) = mean(-70.0) else { return -120.0 };
    mean(lufs(ungated) - 10.0).map_or(-120.0, |z| lufs(z) as f32)
}

pub const BANDS: [(&str, f32, f32); 6] = [
    ("sub", 20.0, 60.0),
    ("bass", 60.0, 120.0),
    ("low-mid", 120.0, 400.0),
    ("mid", 400.0, 2000.0),
    ("presence", 2000.0, 6000.0),
    ("air", 6000.0, 20000.0),
];

/// Mean powers of one band: each side, their sum as one speaker plays it,
/// and their product.
#[derive(Clone, Copy)]
struct BandPower {
    left: f64,
    right: f64,
    mono: f64,
    cross: f64,
}

impl BandPower {
    fn db(&self) -> f32 {
        (10.0 * ((self.left + self.right) / 2.0).max(1e-14).log10()) as f32
    }
}

fn band_powers(audio: &Audio) -> Vec<BandPower> {
    let sr = audio.sample_rate as f32;
    BANDS
        .par_iter()
        .map(|(_, from, to)| {
            let mut band: Vec<Biquad> = butterworth(true, *from, 4, sr);
            band.extend(butterworth(false, *to, 4, sr));
            let (mut fl, mut fr) = (band.clone(), band);
            let mut p = BandPower { left: 0.0, right: 0.0, mono: 0.0, cross: 0.0 };
            for (l, r) in audio.left.iter().zip(&audio.right) {
                let l = fl.iter_mut().fold(*l, |x, f| f.process(x)) as f64;
                let r = fr.iter_mut().fold(*r, |x, f| f.process(x)) as f64;
                p.left += l * l;
                p.right += r * r;
                p.mono += (l + r) * (l + r) / 4.0;
                p.cross += l * r;
            }
            let n = audio.left.len().max(1) as f64;
            BandPower { left: p.left / n, right: p.right / n, mono: p.mono / n, cross: p.cross / n }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct Band {
    pub name: &'static str,
    pub from_hz: f32,
    pub to_hz: f32,
    /// Against the whole mix's RMS.
    pub level_db: f32,
    /// The band's share of the mix's energy, 0 to 1.
    pub share: f32,
    /// What summing left and right costs: 0 for a mono band, -3 for two
    /// unrelated sides, and further down the more they cancel.
    pub mono_loss_db: f32,
    /// -1 to 1: how alike the sides are.
    pub correlation: f32,
}

#[derive(Debug, Serialize)]
pub struct MixReport {
    pub seconds: f64,
    pub loudness_lufs: f32,
    pub peak_db: f32,
    pub bands: Vec<Band>,
    pub findings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LayerChange {
    pub layer: String,
    /// The layer's loudness on the device against its loudness in the mix;
    /// -60 when nothing of it is left.
    pub change_lu: f32,
    /// That, against what the whole mix lost: under zero, the layer sinks.
    pub against_mix_lu: f32,
}

#[derive(Debug, Serialize)]
pub struct DeviceReport {
    pub device: &'static str,
    pub title: &'static str,
    pub loudness_change_lu: f32,
    /// Per band of [`BANDS`], in dB.
    pub band_change_db: Vec<f32>,
    pub bass_reduction: Option<BassReduction>,
    /// Per band, the music over the noise in dB, when the device has noise.
    pub over_noise_db: Option<Vec<f32>>,
    pub layers: Vec<LayerChange>,
    pub findings: Vec<String>,
}

/// A mix measured: how loud, where its energy is, and how much of each band
/// is left when its sides are summed.
pub struct Measured {
    pub report: MixReport,
    powers: Vec<BandPower>,
}

pub fn measure(audio: &Audio) -> Measured {
    let powers = band_powers(audio);
    let total: f64 = powers.iter().map(|p| (p.left + p.right) / 2.0).sum::<f64>().max(1e-14);
    let bands: Vec<Band> = BANDS
        .iter()
        .zip(&powers)
        .map(|((name, from, to), p)| {
            let stereo = ((p.left + p.right) / 2.0).max(1e-14);
            Band {
                name,
                from_hz: *from,
                to_hz: *to,
                level_db: (10.0 * (stereo / total).log10()) as f32,
                share: (stereo / total) as f32,
                mono_loss_db: (10.0 * (p.mono.max(1e-14) / stereo).log10()).min(0.0) as f32,
                correlation: (p.cross / (p.left * p.right).sqrt().max(1e-14)) as f32,
            }
        })
        .collect();
    let mut report = MixReport { seconds: audio.duration(), loudness_lufs: loudness(audio), peak_db: audio.peak_db(), bands, findings: Vec::new() };
    report.findings = mix_findings(&report);
    Measured { report, powers }
}

/// A band counts when it holds this much of the mix.
const AUDIBLE_SHARE: f32 = 0.01;

fn mix_findings(mix: &MixReport) -> Vec<String> {
    let mut findings = Vec::new();
    let (sub, bass) = (&mix.bands[0], &mix.bands[1]);
    if sub.share > AUDIBLE_SHARE && sub.level_db > bass.level_db + 3.0 {
        findings.push(format!(
            "the low end's weight is under 60 Hz ({:.0} dB over the 60-120 Hz band): most speakers start above that and will play the song without its bass. Give the bass something at 100-300 Hz to be heard by — an octave layer, some drive — and leave the sub as the weight under it",
            sub.level_db - bass.level_db
        ));
    }
    for band in &mix.bands[..2] {
        if band.share > AUDIBLE_SHARE && band.correlation < 0.8 {
            findings.push(format!(
                "the {} band is not mono (correlation {:.2}): one woofer sums it, and what is out of phase is gone. Keep everything under 120 Hz in the middle",
                band.name, band.correlation
            ));
        }
    }
    for band in &mix.bands[2..] {
        if band.share > AUDIBLE_SHARE && band.mono_loss_db < -3.5 {
            findings.push(format!(
                "the {} band loses {:.1} dB when left and right are summed, more than two unrelated sides would: its width is made of phase (detuned sides, a chorus, a Haas delay), and a single speaker cancels it",
                band.name, -band.mono_loss_db
            ));
        }
    }
    if mix.peak_db > -0.5 {
        findings.push(format!(
            "peaks reach {:.1} dBFS: a speaker with its own DSP resamples and limits what it is given, and a lossy codec overshoots. Leave 1 dB",
            mix.peak_db
        ));
    }
    if mix.loudness_lufs > -9.0 {
        findings.push(format!(
            "{:.1} LUFS is very loud: streaming turns it down to -14 or so, and what the limiter took to get there does not come back",
            mix.loudness_lufs
        ));
    }
    findings
}

/// A layer of the mix, for [`report`] to follow through the device.
pub struct Layer<'a> {
    pub name: &'a str,
    pub audio: &'a Audio,
    /// Its loudness in the mix, from [`loudness`]: the same for every device.
    pub loudness: f32,
}

/// Measures what the device made of the mix. `radiated` is [`radiate`]'s, by
/// the same device at the same volume.
pub fn report(device: &Device, mix: &Measured, radiated: &Radiated, layers: &[Layer], volume: Volume) -> DeviceReport {
    let powers = band_powers(&radiated.audio);
    let band_change_db: Vec<f32> = powers.iter().zip(&mix.powers).map(|(now, was)| now.db() - was.db()).collect();
    let loudness_change_lu = loudness(&radiated.audio) - mix.report.loudness_lufs;

    let over_noise_db = noise_rms(device, &radiated.audio, volume).map(|rms| {
        let sr = radiated.audio.sample_rate;
        let frames = sr as usize * 10;
        let noise = Audio { sample_rate: sr, left: road_noise(frames, rms, sr as f32, 0x0ca5), right: road_noise(frames, rms, sr as f32, 0x0ca6) };
        band_powers(&noise).iter().zip(&powers).map(|(noise, music)| music.db() - noise.db()).collect::<Vec<f32>>()
    });

    let layers: Vec<LayerChange> = layers
        .par_iter()
        .filter(|layer| layer.loudness > -70.0)
        .map(|layer| {
            let change_lu = (loudness(&linear(device, layer.audio)) - layer.loudness).max(-60.0);
            LayerChange { layer: layer.name.to_string(), change_lu, against_mix_lu: change_lu - loudness_change_lu }
        })
        .collect();

    let mut findings = Vec::new();
    let audible = |i: usize| mix.report.bands[i].share > AUDIBLE_SHARE;
    let names = |picked: &[usize]| {
        let names: Vec<&str> = picked.iter().map(|i| BANDS[*i].0).collect();
        match names.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            _ => names.concat(),
        }
    };
    let gone: Vec<usize> = (0..BANDS.len()).filter(|i| audible(*i) && band_change_db[*i] < -12.0).collect();
    if !gone.is_empty() {
        let share: f32 = gone.iter().map(|i| mix.report.bands[*i].share).sum();
        findings.push(format!("plays next to nothing of the {} band{}, which {} {:.0}% of the mix's energy", names(&gone), if gone.len() == 1 { "" } else { "s" }, if gone.len() == 1 { "holds" } else { "hold" }, share * 100.0));
    }
    if device.width < 0.5 {
        let thinned: Vec<usize> = (2..BANDS.len()).filter(|i| audible(*i) && mix.report.bands[*i].mono_loss_db < -3.5).collect();
        if !thinned.is_empty() {
            findings.push(format!("sums most of left and right, and the {} band{} thin out as their width cancels", names(&thinned), if thinned.len() == 1 { "" } else { "s" }));
        }
    }
    if let Some(bass) = radiated.bass.filter(|b| b.max_db > 3.0 && b.share > 0.05) {
        findings.push(format!(
            "its bass protection pulls everything under {:.0} Hz down by up to {:.1} dB, {:.0}% of the time: the lows pump, and are quieter than they were mixed",
            bass.below_hz, bass.max_db, bass.share * 100.0
        ));
    }
    if let Some(over) = &over_noise_db {
        let covered: Vec<usize> = (0..BANDS.len()).filter(|i| audible(*i) && over[*i] < 3.0).collect();
        if !covered.is_empty() {
            findings.push(format!("road noise covers the {} band{}: what is quiet there is not heard", names(&covered), if covered.len() == 1 { "" } else { "s" }));
        }
    }
    let mut sunk: Vec<&LayerChange> = layers.iter().filter(|l| l.against_mix_lu < -6.0).collect();
    sunk.sort_by(|a, b| a.against_mix_lu.total_cmp(&b.against_mix_lu));
    for layer in sunk {
        findings.push(match layer.change_lu <= -40.0 {
            true => format!("layer '{}' is gone", layer.layer),
            false => format!("layer '{}' sinks {:.0} dB into the mix", layer.layer, -layer.against_mix_lu),
        });
    }

    DeviceReport { device: device.name, title: device.title, loudness_change_lu, band_change_db, bass_reduction: radiated.bass, over_noise_db, layers, findings }
}

/// The mix at another sample rate, for a player whose device wants one.
pub fn resample(audio: &Audio, sample_rate: u32) -> Audio {
    if audio.sample_rate == sample_rate {
        return copy(audio);
    }
    let step = audio.sample_rate as f64 / sample_rate as f64;
    let cutoff = (1.0 / step).min(1.0) as f32;
    let frames = (audio.left.len() as f64 / step) as usize;
    let channel = |data: &[f32]| (0..frames).into_par_iter().map(|i| sinc::interpolate(data, i as f64 * step, cutoff)).collect::<Vec<f32>>();
    Audio { sample_rate, left: channel(&audio.left), right: channel(&audio.right) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(hz: f32, right_gain: f32) -> Audio {
        let left: Vec<f32> = (0..SR as usize * 2).map(|i| 0.5 * (std::f32::consts::TAU * hz * i as f32 / SR as f32).sin()).collect();
        let right = left.iter().map(|s| s * right_gain).collect();
        Audio { sample_rate: SR, left, right }
    }

    #[test]
    fn a_mono_device_cancels_what_is_out_of_phase() {
        let out = radiate(device("mono").unwrap(), &sine(1000.0, -1.0), Volume::Normal);
        assert!(out.audio.peak_db() < -80.0, "{}", out.audio.peak_db());
        let mix = measure(&sine(1000.0, -1.0));
        assert!(mix.report.bands[3].mono_loss_db < -40.0);
        assert!(mix.report.bands[3].correlation < -0.99);
    }

    #[test]
    fn a_phone_plays_no_sub_bass_and_keeps_the_mids() {
        let phone = device("phone").unwrap();
        let sub = sine(45.0, 1.0);
        assert!(radiate(phone, &sub, Volume::Normal).audio.rms_db() < sub.rms_db() - 50.0);
        let mid = sine(1500.0, 1.0);
        assert!((radiate(phone, &mid, Volume::Normal).audio.rms_db() - mid.rms_db()).abs() < 2.0);
    }

    #[test]
    fn a_device_heard_on_itself_is_the_mix() {
        let max = device("airpods-max").unwrap();
        let mix = sine(80.0, 1.0);
        let heard = audition(max, &radiate(max, &mix, Volume::Normal).audio, Some(max), Volume::Normal);
        let settled = SR as usize;
        let worst = heard.left[settled..].iter().zip(&mix.left[settled..]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst < 1e-3, "{worst}");
    }

    #[test]
    fn bass_protection_works_only_when_it_is_loud() {
        let five = device("sonos-five").unwrap();
        let bass = sine(70.0, 1.0);
        assert!(radiate(five, &bass, Volume::Quiet).bass.unwrap().max_db < 0.1);
        assert!(radiate(five, &bass, Volume::Loud).bass.unwrap().max_db > 3.0);
    }

    #[test]
    fn loudness_of_a_full_scale_stereo_sine_is_about_zero() {
        let mut tone = sine(997.0, 1.0);
        tone.left.iter_mut().chain(tone.right.iter_mut()).for_each(|s| *s *= 2.0);
        assert!(loudness(&tone).abs() < 0.3, "{}", loudness(&tone));
    }

    #[test]
    fn a_layer_the_device_cannot_play_is_reported_gone() {
        let phone = device("phone").unwrap();
        let (sub, lead) = (sine(45.0, 1.0), sine(1500.0, 1.0));
        let mix = Audio { sample_rate: SR, left: sub.left.iter().zip(&lead.left).map(|(a, b)| a + b).collect(), right: sub.right.iter().zip(&lead.right).map(|(a, b)| a + b).collect() };
        let layers = [Layer { name: "sub", audio: &sub, loudness: loudness(&sub) }, Layer { name: "lead", audio: &lead, loudness: loudness(&lead) }];
        let report = report(phone, &measure(&mix), &radiate(phone, &mix, Volume::Normal), &layers, Volume::Normal);
        assert!(report.findings.iter().any(|f| f == "layer 'sub' is gone"), "{:?}", report.findings);
        assert!(!report.findings.iter().any(|f| f.contains("'lead'")), "{:?}", report.findings);
    }
}
