//! Offline renderer: voices in parallel, then track mix, send effects and
//! master limiter.

use std::collections::HashMap;

use rayon::prelude::*;

use crate::arrange::{AudioClip, Duck, Timeline, TimelineTrack};
use crate::dsp::biquad::apply_eq;
use crate::dsp::chorus::apply_chorus;
use crate::sampler::audio_file::AudioFile;
use crate::sampler::sinc;
use crate::dsp::delay::PingPongDelay;
use crate::dsp::reverb::PlateReverb;
use crate::dsp::{StereoClip, db_to_gain, limiter, pan_gains};
use crate::instruments::{drums, synth, tb303};
use crate::model::{DrumKind, InstrumentKind, Master, Pitch};
use crate::sampler::Sampler;

pub struct Audio {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Audio {
    pub fn duration(&self) -> f64 {
        self.left.len() as f64 / self.sample_rate as f64
    }

    pub fn peak_db(&self) -> f32 {
        let peak = self.left.iter().chain(&self.right).fold(0.0f32, |m, s| m.max(s.abs()));
        20.0 * peak.max(1e-9).log10()
    }

    pub fn rms_db(&self) -> f32 {
        let n = (self.left.len() * 2).max(1) as f64;
        let sum: f64 = self.left.iter().chain(&self.right).map(|s| (*s as f64).powi(2)).sum();
        (10.0 * (sum / n).max(1e-18).log10()) as f32
    }
}

pub struct RenderReport {
    pub skipped_tracks: Vec<String>,
    pub warnings: Vec<String>,
}

/// One layer's share of the mix: what its tracks contribute, with their sends
/// and the master's linear stages applied, before the master's dynamics.
pub struct LayerAudio {
    pub layer: String,
    pub audio: Audio,
    /// What the layer was keyed by in the cache, when there was one and the
    /// layer could be cached — an Audio Unit track's cannot.
    pub key: Option<u64>,
    /// Read back rather than rendered.
    pub cached: bool,
}

/// A render: the mix and, when asked for, the layers it is the sum of.
pub struct Rendering {
    pub mix: Audio,
    /// Empty unless the render was asked to split.
    ///
    /// The layers sum, sample for sample, to the mix as it was before
    /// saturation, the bus compressor, the clipper and the limiter — every
    /// stage up to there is linear, so the sum of the parts is the whole.
    /// With [`RenderOptions::stems_through_master`] they have been through the
    /// master's own gain curve as well, and sum to the mix itself.
    pub layers: Vec<LayerAudio>,
    /// The peak of the layers' sum in dBFS — the mix before its dynamics —
    /// so a player that sums the layers itself knows how far above the
    /// limiter's ceiling that lands, and how much to turn them down. With
    /// `stems_through_master` the sum is the mix, already under the ceiling,
    /// and nothing needs turning down.
    pub layers_peak_db: f32,
    /// What the master's gain curve was, when the layers went through it: a
    /// number that changes with any change to the song, so a stem kept under
    /// it belongs to a render of exactly this mix. Nil when they did not.
    pub master_curve_key: Option<u64>,
    pub report: RenderReport,
}

/// The stages of the master chain a layer has been through, and the ones it
/// has not, by name — written into the stems' manifest so a player knows
/// what the sum of the stems is.
pub const LAYER_STAGES_APPLIED: &[&str] = &["delay", "reverb", "sidechain", "gain", "eq", "width"];
pub const LAYER_STAGES_SKIPPED: &[&str] = &["saturation", "comp", "clip", "limiter"];

/// The same, for layers written through the master's gain curve: there is
/// nothing the mix has been through that they have not.
pub const MASTERED_STAGES_APPLIED: &[&str] = &["delay", "reverb", "sidechain", "gain", "eq", "width", "saturation", "comp", "clip", "limiter"];

/// How to render.
#[derive(Default, Clone)]
pub struct RenderOptions {
    /// Keep the layers apart in the result, for stems.
    pub split: bool,
    /// A directory where each layer is kept by a hash of everything that
    /// shapes it, so a render after an edit to one track synthesises and
    /// reverberates that track's layer and reads the rest back. See
    /// `crate::render_cache`.
    pub cache: Option<std::path::PathBuf>,
    /// Put the layers through the master's own gain curve, so that summing
    /// them gives the mix and not the mix before its dynamics. See
    /// [`master_gain_curve`]. Nothing without `split`.
    pub stems_through_master: bool,
}

/// Renders the timeline. `stems` holds pre-rendered dry audio for tracks the
/// built-in engine cannot play (Audio Unit instruments), keyed by track index.
pub fn render(timeline: &Timeline, sample_rate: u32, stems: HashMap<usize, StereoClip>) -> (Audio, RenderReport) {
    let rendering = render_layers(timeline, sample_rate, stems, false);
    (rendering.mix, rendering.report)
}

pub fn render_layers(timeline: &Timeline, sample_rate: u32, stems: HashMap<usize, StereoClip>, split: bool) -> Rendering {
    render_with(timeline, sample_rate, stems, &RenderOptions { split, cache: None, stems_through_master: false })
}

/// Renders the timeline as layers, and the mix as their sum.
///
/// **Every track is rendered once, into its layer.** The layers used to be
/// made by rendering the song once per layer with the other tracks taken out,
/// and those were not the song: each went through the master's compressor and
/// limiter on its own peaks. Now each layer goes through its tracks' sends
/// into the delay and the reverb, the master sidechain keyed from the whole
/// song, and the master's gain, EQ and width; the mix is the layers' sum, and
/// only then saturation, the compressor, the clipper and the limiter.
///
/// **A layer is computed to its own end**, its last sound plus the reverb and
/// delay tail, and padded with silence to the song's. Its length is then a
/// fact about the layer alone, which is what lets a cached layer stay valid
/// when another track's last note moves. The mix goes through the layers
/// whether or not they are kept, so a render with a cache and one without are
/// the same numbers.
///
/// **Layers that are not cached are rendered side by side**, a few at a time:
/// each holds a handful of buffers the length of the song.
///
/// **A timeline that is only part of a song** — see [`crate::bars`] — renders
/// the same way and then has its lead-in dropped from the front of the mix and
/// of every layer, so what comes back begins at the first bar asked for.
pub fn render_with(timeline: &Timeline, sample_rate: u32, mut stems: HashMap<usize, StereoClip>, options: &RenderOptions) -> Rendering {
    let sr = sample_rate as f32;
    let mut skipped = Vec::new();
    let mut warnings = Vec::new();
    let timing = Timing::from_env();
    let master = &timeline.master;
    let tail = (tail_seconds(timeline) * sr as f64) as usize;

    // Which tracks go together: one group per layer, in the order the layers
    // first appear.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (ti, track) in timeline.tracks.iter().enumerate() {
        match groups.iter_mut().find(|(layer, _)| layer == &track.layer) {
            Some((_, members)) => members.push(ti),
            None => groups.push((track.layer.clone(), vec![ti])),
        }
    }

    let key_source = master.sidechain.as_ref().map(|s| s.source.clone());
    let is_key = |track: &TimelineTrack| key_source.as_ref().is_some_and(|k| &track.name == k || &track.layer == k);

    // What each layer is keyed by, and which are already in the cache.
    let cache = options.cache.as_ref().map(|dir| crate::render_cache::LayerCache::open(dir.clone()));
    let keys: Vec<Option<u64>> = groups
        .iter()
        .map(|(_, members)| cache.as_ref().and_then(|_| crate::render_cache::layer_key(timeline, members, sample_rate)))
        .collect();
    let cached: Vec<bool> = keys
        .iter()
        .map(|key| match (&cache, key) {
            (Some(cache), Some(key)) => cache.has(*key),
            _ => false,
        })
        .collect();
    let any_rendered = cached.iter().any(|c| !c);

    // The tracks whose sound has to be made: the members of every layer that
    // is not cached, the records their scratch tracks cut from, and the
    // sidechain's key when any layer is going to be ducked by it.
    let mut needed = vec![false; timeline.tracks.len()];
    for ((_, members), is_cached) in groups.iter().zip(&cached) {
        if *is_cached {
            continue;
        }
        for &ti in members {
            needed[ti] = true;
            if let InstrumentKind::Scratch(def) = &timeline.tracks[ti].instrument
                && let Some(source) = &def.source_track
            {
                for (si, other) in timeline.tracks.iter().enumerate() {
                    let wanted = if source == "mix" { !matches!(other.instrument, InstrumentKind::Scratch(_)) } else { &other.name == source };
                    if wanted {
                        needed[si] = true;
                    }
                }
            }
        }
    }
    if any_rendered {
        for (ti, track) in timeline.tracks.iter().enumerate() {
            if is_key(track) {
                needed[ti] = true;
            }
        }
    }

    // Plugins run on the calling thread, one at a time: many expect a single host thread.
    let mut plugin_results: Vec<(usize, Result<Vec<StereoClip>, String>)> = Vec::new();
    for (ti, track) in timeline.tracks.iter().enumerate() {
        if needed[ti]
            && let InstrumentKind::Clap(def) = &track.instrument
        {
            plugin_results.push((ti, render_clap(def, &track.notes, sr).map(|c| vec![c])));
        }
    }

    let results: Vec<(usize, Result<Vec<StereoClip>, String>)> = timeline
        .tracks
        .par_iter()
        .enumerate()
        .filter(|(ti, _)| needed[*ti])
        .filter_map(|(ti, track)| render_track(track, sr).map(|r| (ti, r)))
        .collect::<Vec<_>>()
        .into_iter()
        .chain(plugin_results)
        .collect();
    timing.mark(&format!("voices of {} of {} tracks", needed.iter().filter(|n| **n).count(), timeline.tracks.len()));
    let mut rendered = Vec::new();
    for (ti, result) in results {
        match result {
            Ok(clips) => rendered.push((ti, clips)),
            Err(e) => warnings.push(format!("track '{}': {e}", timeline.tracks[ti].name)),
        }
    }
    // Scratch tracks that use another track as their record come last: they
    // need that track's dry audio.
    for (ti, track) in timeline.tracks.iter().enumerate() {
        if !needed[ti] {
            continue;
        }
        let InstrumentKind::Scratch(def) = &track.instrument else { continue };
        let Some(src_name) = &def.source_track else { continue };
        let source_clips: Vec<&StereoClip> = if src_name == "mix" {
            rendered
                .iter()
                .filter(|(i, _)| !matches!(timeline.tracks[*i].instrument, InstrumentKind::Scratch(_)))
                .flat_map(|(_, c)| c.iter())
                .collect()
        } else {
            let Some(si) = timeline.tracks.iter().position(|t| &t.name == src_name) else { continue };
            match rendered.iter().find(|(i, _)| *i == si) {
                Some((_, clips)) => clips.iter().collect(),
                None => {
                    warnings.push(format!("track '{}': source track '{src_name}' has no audio", track.name));
                    continue;
                }
            }
        };
        let from = (def.start * sr as f64) as usize;
        let n = (def.length.unwrap_or(1.0) * sr as f64) as usize;
        let mut region = (vec![0.0f32; n], vec![0.0f32; n]);
        for clip in source_clips {
            for (i, (l, r)) in clip.left.iter().zip(&clip.right).enumerate() {
                let idx = clip.offset + i;
                if idx >= from && idx < from + n {
                    region.0[idx - from] += l;
                    region.1[idx - from] += r;
                }
            }
        }
        // The record should sound like the source track does in the mix: apply its gain and EQ.
        if let Some(src) = timeline.tracks.iter().find(|t| &t.name == src_name) {
            if let Some(eq) = &src.eq {
                apply_eq(eq, &mut region.0, &mut region.1, sr);
            }
            let g = db_to_gain(src.gain_db);
            region.0.iter_mut().chain(region.1.iter_mut()).for_each(|s| *s *= g);
        }
        match crate::instruments::scratch::render_region(def, region.0, region.1, sr as f64, &track.notes, sr) {
            Ok(clips) => rendered.push((ti, clips)),
            Err(e) => warnings.push(format!("track '{}': {e}", track.name)),
        }
    }
    for (ti, track) in timeline.tracks.iter().enumerate() {
        if let InstrumentKind::AudioUnit(au) = &track.instrument {
            match stems.remove(&ti) {
                Some(mut stem) => {
                    let gain = db_to_gain(au.gain_db);
                    stem.left.iter_mut().chain(stem.right.iter_mut()).for_each(|s| *s *= gain);
                    rendered.push((ti, vec![stem]));
                }
                None => skipped.push(track.name.clone()),
            }
        }
    }

    // Audio tracks are read while mixing, to keep memory low.
    let mut files_of: HashMap<usize, AudioFile> = HashMap::new();
    for (ti, track) in timeline.tracks.iter().enumerate() {
        if needed[ti]
            && let InstrumentKind::Audio(source) = &track.instrument
        {
            match AudioFile::open(std::path::Path::new(&source.path)) {
                Ok(file) => {
                    files_of.insert(ti, file);
                }
                Err(e) => warnings.push(format!("track '{}': {e}", track.name)),
            }
        }
    }
    let clips_of: HashMap<usize, Vec<StereoClip>> = rendered.into_iter().collect();
    let sources = TrackSources { timeline, clips_of: &clips_of, files_of: &files_of, sr };
    timing.mark("scratch, audio units, audio files");

    // The master sidechain's key: every key track of the song, whichever
    // layer it is in, so a layer on its own is ducked exactly as it is in the
    // mix. Key tracks are summed apart from the rest and added back after the
    // ducking, which they must not duck themselves.
    let key: [Vec<f32>; 2] = if key_source.is_some() && any_rendered {
        let audios: Vec<TrackAudio> = (0..timeline.tracks.len())
            .filter(|ti| is_key(&timeline.tracks[*ti]))
            .filter_map(|ti| sources.audio_of(ti, &mut warnings))
            .collect();
        let len = audios.iter().map(|a| a.start + a.left.len()).max().unwrap_or(0);
        let mut key = [vec![0.0f32; len], vec![0.0f32; len]];
        for audio in &audios {
            add_into(&mut key, audio, 1.0);
        }
        key
    } else {
        [Vec::new(), Vec::new()]
    };

    // The layers not in the cache, side by side.
    let to_render: Vec<usize> = (0..groups.len()).filter(|g| !cached[*g]).collect();
    let concurrency = std::thread::available_parallelism().map_or(2, |n| n.get() / 4).clamp(1, 3);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(concurrency).build().expect("a thread pool");
    let made: Vec<(usize, Vec<f32>, Vec<f32>, Vec<String>)> = pool.install(|| {
        to_render
            .par_iter()
            .map(|&g| {
                let started = std::time::Instant::now();
                let mut warnings = Vec::new();
                let members = &groups[g].1;
                let audios: Vec<(usize, TrackAudio)> =
                    members.iter().filter_map(|&ti| sources.audio_of(ti, &mut warnings).map(|a| (ti, a))).collect();
                let content = audios.iter().map(|(_, a)| a.start + a.left.len()).max().unwrap_or(0);
                let len = if content == 0 { 0 } else { content + tail };
                let key_len = if key_source.is_some() { len } else { 0 };
                let mut dry = [vec![0.0f32; len], vec![0.0f32; len]];
                let mut own_key = [vec![0.0f32; key_len], vec![0.0f32; key_len]];
                let mut reverb_bus = [vec![0.0f32; len], vec![0.0f32; len]];
                let mut delay_bus = [vec![0.0f32; len], vec![0.0f32; len]];
                for (ti, audio) in &audios {
                    let track = &timeline.tracks[*ti];
                    add_into(if is_key(track) { &mut own_key } else { &mut dry }, audio, 1.0);
                    add_into(&mut reverb_bus, audio, track.reverb);
                    add_into(&mut delay_bus, audio, track.delay);
                }
                drop(audios);
                let (left, right) = through_linear_master(timeline, master, sr, dry, &key, &own_key, reverb_bus, delay_bus);
                if let (Some(cache), Some(k)) = (&cache, keys[g])
                    && let Err(e) = cache.store(k, &left, &right)
                {
                    warnings.push(format!("layer '{}': not cached: {e}", groups[g].0));
                }
                timing.say(&format!("layer {}: rendered in {:.0} ms", groups[g].0, started.elapsed().as_secs_f64() * 1000.0));
                (g, left, right, warnings)
            })
            .collect()
    });
    let mut buffers: Vec<Option<(Vec<f32>, Vec<f32>)>> = (0..groups.len()).map(|_| None).collect();
    for (g, left, right, said) in made {
        buffers[g] = Some((left, right));
        warnings.extend(said);
    }
    timing.mark(&format!("{} layers rendered", to_render.len()));
    for g in 0..groups.len() {
        if buffers[g].is_some() {
            continue;
        }
        let (Some(cache), Some(k)) = (&cache, keys[g]) else { continue };
        match cache.load(k) {
            Ok(buffer) => buffers[g] = Some(buffer),
            Err(e) => {
                // A cache file that went between the check and the read: said,
                // and the layer is silent in this render rather than the render
                // failing. The next render renders it.
                warnings.push(format!("layer '{}': cached audio could not be read: {e}", groups[g].0));
                buffers[g] = Some((Vec::new(), Vec::new()));
            }
        }
    }
    timing.mark(&format!("{} layers read from the cache", cached.iter().filter(|c| **c).count()));

    let len = buffers.iter().flatten().map(|(l, _)| l.len()).max().unwrap_or(0);
    let mut mix = [vec![0.0f32; len], vec![0.0f32; len]];
    for (left, right) in buffers.iter().flatten() {
        for (i, (l, r)) in left.iter().zip(right).enumerate() {
            mix[0][i] += l;
            mix[1][i] += r;
        }
    }
    timing.mark("sum");

    let [mut left, mut right] = mix;
    let layers_peak_db = {
        let peak = left.iter().chain(&right).fold(0.0f32, |m, s| m.max(s.abs()));
        20.0 * peak.max(1e-9).log10()
    };
    // The mix before the master's dynamics, kept so the layers can be put
    // through what those dynamics did to it. See `master_gain_curve`.
    let pre = (options.split && options.stems_through_master).then(|| [left.clone(), right.clone()]);
    if master.saturation > 0.0 {
        crate::dsp::dynamics::saturate(master.saturation, &mut left, &mut right);
    }
    if let Some(comp) = &master.comp {
        crate::dsp::dynamics::compress(comp, &mut left, &mut right, sr);
    }
    if master.clip_db < 0.0 {
        crate::dsp::dynamics::clip(master.clip_db, &mut left, &mut right);
    }
    if master.limiter.enabled {
        limiter::limit(&mut left, &mut right, master.limiter.ceiling_db, master.limiter.release_ms, sr);
    }
    let end = trim_tail(&mut left, &mut right, sr);
    timing.mark("master dynamics");

    let lead_frames = timeline.window.as_ref().map_or(0, |w| w.lead_in_frames(sample_rate));
    let curve = pre.map(|pre| {
        fade_lead_in(&mut left, &mut right, lead_frames, sr);
        master_gain_curve(pre, (&left, &right), end)
    });
    let master_curve_key = curve.as_ref().map(curve_key);
    if curve.is_some() {
        timing.mark("master gain curve");
    }

    if let Some(cache) = &cache {
        cache.keep(keys.iter().flatten().copied());
    }

    // The layers end where the mix ends, with the same fade, so they are the
    // mix's length and still sum to it.
    let mut layers = if options.split {
        groups
            .into_iter()
            .zip(buffers)
            .zip(keys.iter().zip(&cached))
            .map(|(((layer, _), buffer), (key, was_cached))| {
                let (mut l, mut r) = buffer.unwrap_or_default();
                l.resize(end, 0.0);
                r.resize(end, 0.0);
                match &curve {
                    // The mix's own fade is already in the curve, so a layer
                    // through it must not be faded a second time.
                    Some(curve) => apply_curve(curve, &mut l, &mut r),
                    None => fade_out(&mut l, &mut r, sr),
                }
                LayerAudio { layer, audio: Audio { sample_rate, left: l, right: r }, key: *key, cached: *was_cached }
            })
            .collect()
    } else {
        Vec::new()
    };

    // Only part of the song: the lead-in was rendered so that notes which
    // begin just before the stretch are heard ringing at its start, and now it
    // goes, off the mix and off every layer alike, so that they still sum.
    if lead_frames > 0 {
        match curve.is_some() {
            true => {
                drain_lead_in(&mut left, &mut right, lead_frames);
                for layer in &mut layers {
                    drain_lead_in(&mut layer.audio.left, &mut layer.audio.right, lead_frames);
                }
            }
            false => {
                drop_lead_in(&mut left, &mut right, lead_frames, sr);
                for layer in &mut layers {
                    drop_lead_in(&mut layer.audio.left, &mut layer.audio.right, lead_frames, sr);
                }
            }
        }
    }

    Rendering { mix: Audio { sample_rate, left, right }, layers, layers_peak_db, master_curve_key, report: RenderReport { skipped_tracks: skipped, warnings } }
}

/// Drops the lead-in from the front of a render of part of a song, and fades
/// the cut over 5 ms: what is left begins in the middle of the music, and a
/// file whose first sample is far from zero clicks when it is played.
pub(crate) fn drop_lead_in(left: &mut Vec<f32>, right: &mut Vec<f32>, frames: usize, sr: f32) {
    if frames == 0 {
        return;
    }
    fade_lead_in(left, right, frames, sr);
    drain_lead_in(left, right, frames);
}

/// The fade alone, where the cut will be once the lead-in is gone. Taken
/// before the master's gain curve when there is one, so that the curve carries
/// it and a layer through the curve is faded once and not twice.
pub(crate) fn fade_lead_in(left: &mut [f32], right: &mut [f32], frames: usize, sr: f32) {
    if frames == 0 {
        return;
    }
    let frames = frames.min(left.len());
    let fade = ((0.005 * sr) as usize).min(left.len() - frames);
    for k in 0..fade {
        let g = k as f32 / fade as f32;
        left[frames + k] *= g;
        right[frames + k] *= g;
    }
}

/// The lead-in itself, off the front.
pub(crate) fn drain_lead_in(left: &mut Vec<f32>, right: &mut Vec<f32>, frames: usize) {
    let frames = frames.min(left.len());
    left.drain(..frames);
    right.drain(..frames);
}

/// How long the send effects ring after the last sound.
pub(crate) fn tail_seconds(timeline: &Timeline) -> f64 {
    let master = &timeline.master;
    (if master.reverb.enabled { 1.5 + 6.0 * master.reverb.decay as f64 } else { 0.5 })
        + if master.delay.enabled { delay_tail(timeline.delay_seconds, master.delay.feedback) } else { 0.0 }
}

/// Phase timings on stderr when `MAT_TIMING` is set: where a render's time
/// goes, which is the question every speed-up here starts from.
pub(crate) struct Timing {
    on: bool,
    last: std::sync::Mutex<std::time::Instant>,
}

impl Timing {
    pub(crate) fn from_env() -> Self {
        Timing { on: std::env::var_os("MAT_TIMING").is_some(), last: std::sync::Mutex::new(std::time::Instant::now()) }
    }

    pub(crate) fn mark(&self, phase: &str) {
        if !self.on {
            return;
        }
        let now = std::time::Instant::now();
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        eprintln!("timing {:>8.1} ms  {phase}", (now - *last).as_secs_f64() * 1000.0);
        *last = now;
    }

    pub(crate) fn say(&self, line: &str) {
        if self.on {
            eprintln!("timing            {line}");
        }
    }
}

/// A track as it sounds in the mix: its clips summed, its effects, its gain
/// and its pan, from `start` on.
struct TrackAudio {
    start: usize,
    left: Vec<f32>,
    right: Vec<f32>,
}

/// Where a track's audio comes from: rendered clips for most, a file read on
/// demand for an audio track.
struct TrackSources<'a> {
    timeline: &'a Timeline,
    clips_of: &'a HashMap<usize, Vec<StereoClip>>,
    files_of: &'a HashMap<usize, AudioFile>,
    sr: f32,
}

impl TrackSources<'_> {
    fn audio_of(&self, ti: usize, warnings: &mut Vec<String>) -> Option<TrackAudio> {
        let track = &self.timeline.tracks[ti];
        if track.silent {
            return None;
        }
        if let Some(clips) = self.clips_of.get(&ti) {
            return process_track(track, clips, self.sr);
        }
        let file = self.files_of.get(&ti)?;
        match render_audio(file, &track.clips, self.sr) {
            Ok(clips) => process_track(track, &clips, self.sr),
            Err(e) => {
                warnings.push(format!("track '{}': {e}", track.name));
                None
            }
        }
    }
}

fn add_into(bus: &mut [Vec<f32>; 2], audio: &TrackAudio, amount: f32) {
    if amount == 0.0 {
        return;
    }
    for (i, (l, r)) in audio.left.iter().zip(&audio.right).enumerate() {
        let idx = audio.start + i;
        bus[0][idx] += l * amount;
        bus[1][idx] += r * amount;
    }
}

/// The track's insert chain: EQ, compressor, chorus, phaser, ducking, gain
/// sweeps, then gain and pan.
fn process_track(track: &TimelineTrack, clips: &[StereoClip], sr: f32) -> Option<TrackAudio> {
    let start = clips.iter().map(|c| c.offset).min()?;
    let chorus_tail = if track.chorus.is_some() { (0.05 * sr) as usize } else { 0 };
    let end = clips.iter().map(|c| c.offset + c.left.len()).max().unwrap_or(start) + chorus_tail;
    let mut left = vec![0.0f32; end.saturating_sub(start)];
    let mut right = vec![0.0f32; left.len()];
    for clip in clips {
        for (i, (l, r)) in clip.left.iter().zip(&clip.right).enumerate() {
            let idx = clip.offset + i - start;
            if idx < left.len() {
                left[idx] += l;
                right[idx] += r;
            }
        }
    }
    if let Some(eq) = &track.eq {
        apply_eq(eq, &mut left, &mut right, sr);
    }
    if let Some(comp) = &track.comp {
        crate::dsp::dynamics::compress(comp, &mut left, &mut right, sr);
    }
    if let Some(chorus) = &track.chorus {
        apply_chorus(chorus, &mut left, &mut right, sr);
    }
    if let Some(ph) = &track.phaser {
        crate::dsp::phaser::apply_phaser(ph, &mut left, &mut right, sr);
    }
    if let Some(duck) = &track.duck {
        apply_duck(duck, start, &mut left, &mut right, sr);
    }
    let gain_sweeps: Vec<&crate::arrange::Sweep> = track.sweeps.iter().filter(|s| s.param == "gain").collect();
    if !gain_sweeps.is_empty() {
        for i in 0..left.len() {
            let t = (start + i) as f64 / sr as f64;
            let mut db = None;
            for s in &gain_sweeps {
                if t >= s.start {
                    let x = if s.end > s.start { ((t - s.start) / (s.end - s.start)).clamp(0.0, 1.0) as f32 } else { 1.0 };
                    db = Some(s.from + (s.to - s.from) * x);
                }
            }
            if let Some(db) = db {
                let g = db_to_gain(db);
                left[i] *= g;
                right[i] *= g;
            }
        }
    }
    let gain = db_to_gain(track.gain_db);
    let (pl, pr) = pan_gains(track.pan);
    let (gl, gr) = (gain * pl, gain * pr);
    for (l, r) in left.iter_mut().zip(right.iter_mut()) {
        *l *= gl;
        *r *= gr;
    }
    Some(TrackAudio { start, left, right })
}

/// The send effects and the master's linear stages over one group of tracks:
/// the delay and the reverb from the group's sends, the sidechain keyed from
/// `key` (the whole song's key tracks) with the group's own key tracks added
/// back afterwards, then master gain, EQ and width. Everything here is linear
/// in the group's signal, which is what lets the groups be summed afterwards.
fn through_linear_master(
    timeline: &Timeline,
    master: &Master,
    sr: f32,
    mut mix: [Vec<f32>; 2],
    key: &[Vec<f32>; 2],
    own_key: &[Vec<f32>; 2],
    mut reverb_bus: [Vec<f32>; 2],
    delay_bus: [Vec<f32>; 2],
) -> (Vec<f32>, Vec<f32>) {
    let len = mix[0].len();
    let mut wet = [vec![0.0f32; len], vec![0.0f32; len]];
    if master.delay.enabled {
        let mut delay = PingPongDelay::new(timeline.delay_seconds, master.delay.feedback, master.delay.tone_hz, sr);
        delay.set_modulation(master.delay.mod_ms, master.delay.mod_rate_hz);
        let [wl, wr] = &mut wet;
        delay.process(&delay_bus[0], &delay_bus[1], wl, wr);
        for ch in 0..2 {
            for i in 0..len {
                mix[ch][i] += wet[ch][i];
                // Let echoes bloom into the reverb as well.
                reverb_bus[ch][i] += wet[ch][i] * 0.3;
            }
        }
    }
    if master.reverb.enabled {
        let mut reverb = PlateReverb::new(&master.reverb, sr);
        let [wl, wr] = &mut wet;
        reverb.process(&reverb_bus[0], &reverb_bus[1], wl, wr);
        for ch in 0..2 {
            for i in 0..len {
                mix[ch][i] += wet[ch][i];
            }
        }
    }

    let [mut left, mut right] = mix;
    if let Some(sc) = &master.sidechain {
        crate::dsp::dynamics::keyed_compress(sc, &key[0], &key[1], &mut left, &mut right, sr);
        for ch in 0..2 {
            let dst = if ch == 0 { &mut left } else { &mut right };
            for (d, k) in dst.iter_mut().zip(&own_key[ch]) {
                *d += k;
            }
        }
    }
    let master_gain = db_to_gain(master.gain_db);
    for s in left.iter_mut().chain(right.iter_mut()) {
        *s *= master_gain;
    }
    if let Some(eq) = &master.eq {
        apply_eq(eq, &mut left, &mut right, sr);
    }
    if (master.width - 1.0).abs() > 1e-3 {
        for i in 0..left.len() {
            let mid = (left[i] + right[i]) * 0.5;
            let side = (left[i] - right[i]) * 0.5 * master.width;
            left[i] = mid + side;
            right[i] = mid - side;
        }
    }
    (left, right)
}

pub(crate) fn render_track(track: &TimelineTrack, sr: f32) -> Option<Result<Vec<StereoClip>, String>> {
    match &track.instrument {
        InstrumentKind::Sampler(def) => Some(
            Sampler::load(std::path::Path::new(&def.load)).and_then(|sampler| sampler.render(&track.notes, def, sr)),
        ),
        InstrumentKind::Samples(def) => Some(Sampler::from_zones(&def.zones).and_then(|sampler| sampler.render(&track.notes, &def.settings, sr))),
        InstrumentKind::Scratch(def) if def.source_track.is_none() => Some(crate::instruments::scratch::render(def, &track.notes, sr)),
        InstrumentKind::Scratch(_) => None,
        InstrumentKind::Synth(def) => Some(Ok(
            track
                .notes
                .par_iter()
                .enumerate()
                .map(|(i, note)| {
                    // Portamento glides from the last note that started before this one.
                    let from = if def.glide > 0.0 {
                        track.notes[..i].iter().rev().find(|p| p.start < note.start - 1e-6).map(|p| p.midi)
                    } else {
                        None
                    };
                    synth::render_note_from(def, note, from, &track.sweeps, sr, note.seed)
                })
                .collect(),
        )),
        InstrumentKind::Drums(kit) => Some(Ok(
            track
                .notes
                .par_iter()
                .enumerate()
                .map(|(i, note)| {
                    let Pitch::Drum(kind) = note.pitch else { unreachable!() };
                    let choke = (kind == DrumKind::OpenHat)
                        .then(|| {
                            track.notes[i + 1..]
                                .iter()
                                .find(|n| matches!(n.pitch, Pitch::Drum(DrumKind::Hat | DrumKind::OpenHat)) && n.start > note.start)
                                .map(|n| n.start - note.start)
                        })
                        .flatten();
                    drums::render_hit(note, kit.voice(kind), choke, sr, note.seed)
                })
                .collect(),
        )),
        InstrumentKind::Tb303(def) => Some(Ok(vec![tb303::render(def, &track.notes, &track.sweeps, sr)])),
        InstrumentKind::AudioUnit(_) | InstrumentKind::Audio(_) | InstrumentKind::Clap(_) => None,
    }
}

/// A plugin track being played, a stretch at a time: the plugin's own block
/// loop, with the track's gain over what comes out of it.
pub(crate) struct ClapPlayer {
    run: crate::clap_host::ClapRun,
    gain: f32,
    /// Frames of the track given out so far, which is where the next clip
    /// begins.
    made: usize,
}

impl ClapPlayer {
    /// Whether the plugin has been played to the end of the track.
    pub(crate) fn finished(&self) -> bool {
        self.run.finished()
    }

    /// The track up to frame `to`, as a clip that begins where the last one
    /// ended.
    pub(crate) fn render_to(&mut self, to: usize) -> Result<StereoClip, String> {
        let (mut left, mut right) = self.run.render_to(to)?;
        left.iter_mut().chain(right.iter_mut()).for_each(|s| *s *= self.gain);
        let offset = self.made;
        self.made += left.len();
        Ok(StereoClip { offset, left, right })
    }
}

/// Loads a plugin, gives it its patch and its parameters, and sets it playing
/// the track's notes. Every call on what comes back has to be made from the
/// thread that made this one.
pub(crate) fn start_clap(def: &crate::model::ClapDef, notes: &[crate::arrange::TimedNote], sr: f32) -> Result<ClapPlayer, String> {
    let instance = crate::clap_host::ClapInstance::load(&def.plugin, def.plugin_id.as_deref())?;
    if let Some(patch) = &def.patch {
        instance.load_preset(std::path::Path::new(patch))?;
    }
    let all = instance.params();
    let mut params = Vec::new();
    for (name, value) in &def.params {
        let wanted = name.to_lowercase();
        let found = all
            .iter()
            .find(|p| p.name.to_lowercase() == wanted)
            .or_else(|| all.iter().find(|p| format!("{} {}", p.module, p.name).to_lowercase().contains(&wanted)))
            .ok_or_else(|| format!("{} has no parameter '{name}' (list them with: mat plugin-params \"{}\")", instance.name, def.plugin))?;
        params.push((found.id, value.clamp(found.min, found.max)));
    }
    let end = notes.iter().map(|n| n.start + n.duration).fold(0.0, f64::max);
    let run = instance.start(notes, &params, end + 4.0, sr)?;
    Ok(ClapPlayer { run, gain: db_to_gain(def.gain_db), made: 0 })
}

pub(crate) fn render_clap(def: &crate::model::ClapDef, notes: &[crate::arrange::TimedNote], sr: f32) -> Result<StereoClip, String> {
    start_clap(def, notes, sr)?.render_to(usize::MAX)
}

/// Reads the clips of an audio track, resampling if the file rate differs.
pub(crate) fn render_audio(file: &AudioFile, clips: &[AudioClip], sr: f32) -> Result<Vec<StereoClip>, String> {
    let file_rate = file.sample_rate;
    let duration = file.frames as f64 / file_rate;
    let fade = (0.005 * sr) as usize;
    let mut out = Vec::new();
    for clip in clips {
        let (mut at, mut src) = (clip.at, clip.source_start);
        let mut length = clip.length.unwrap_or(duration - src);
        if at < 0.0 {
            src -= at;
            length += at;
            at = 0.0;
        }
        length = length.min(duration - src);
        if length <= 0.0 {
            continue;
        }
        let frames_out = (length * sr as f64) as usize;
        let (mut left, mut right) = if (file_rate - sr as f64).abs() < 0.5 {
            let from = (src * file_rate).round() as i64;
            file.read_stereo(from, from + frames_out as i64)?
        } else {
            let speed = file_rate / sr as f64;
            let pad = (sinc::HALF_WIDTH * 4) as i64;
            let from = (src * file_rate).floor() as i64;
            let to = ((src + length) * file_rate).ceil() as i64;
            let (dl, dr) = file.read_stereo(from - pad, to + pad)?;
            let cutoff = (1.0 / speed).min(1.0) as f32;
            let offset = pad as f64 + (src * file_rate - from as f64);
            let pos = |i: usize| offset + i as f64 * speed;
            ((0..frames_out).map(|i| sinc::interpolate(&dl, pos(i), cutoff)).collect(), (0..frames_out).map(|i| sinc::interpolate(&dr, pos(i), cutoff)).collect())
        };
        let n = left.len();
        for k in 0..fade.min(n / 2) {
            let g = k as f32 / fade as f32;
            if src > 0.0 {
                left[k] *= g;
                right[k] *= g;
            }
            left[n - 1 - k] *= g;
            right[n - 1 - k] *= g;
        }
        out.push(StereoClip { offset: (at * sr as f64).round() as usize, left, right });
    }
    Ok(out)
}

/// Sidechain ducking: dips the gain at every trigger time and recovers
/// with a smooth cosine curve.
fn apply_duck(duck: &Duck, start: usize, left: &mut [f32], right: &mut [f32], sr: f32) {
    let attack = (duck.attack.max(0.001) * sr) as usize;
    let release = (duck.release.max(0.01) * sr) as usize;
    let mut shape = vec![0.0f32; left.len()];
    for &t in &duck.times {
        let hit = (t * sr as f64) as usize;
        let from = hit.saturating_sub(start);
        if hit + attack + release < start || from >= shape.len() {
            continue;
        }
        for k in 0..attack + release {
            let idx = hit + k;
            if idx < start {
                continue;
            }
            let Some(s) = shape.get_mut(idx - start) else { break };
            let v = if k < attack {
                k as f32 / attack as f32
            } else {
                0.5 * (1.0 + (std::f32::consts::PI * (k - attack) as f32 / release as f32).cos())
            };
            *s = s.max(v);
        }
    }
    for (i, s) in shape.iter().enumerate() {
        let g = 1.0 - duck.depth * s;
        left[i] *= g;
        right[i] *= g;
    }
}

fn delay_tail(delay_seconds: f64, feedback: f32) -> f64 {
    if feedback <= 0.0 {
        return delay_seconds * 2.0;
    }
    // Repeats until the echo falls below -60 dB.
    let repeats = (0.001f64.ln() / (feedback as f64).ln()).min(40.0);
    delay_seconds * (repeats + 1.0)
}

/// Makes a seamless loop of `seconds`: everything after the loop point
/// (reverb and delay tails, releases) is mixed back into the start.
pub fn fold_loop(audio: &mut Audio, seconds: f64) {
    let n = (seconds * audio.sample_rate as f64).round() as usize;
    if audio.left.len() <= n {
        audio.left.resize(n, 0.0);
        audio.right.resize(n, 0.0);
        return;
    }
    for ch in [&mut audio.left, &mut audio.right] {
        let tail: Vec<f32> = ch[n..].to_vec();
        ch.truncate(n);
        for (i, s) in tail.iter().enumerate() {
            ch[i % n] += s;
        }
    }
}

/// Removes trailing near-silence and applies a short fade-out. Returns the
/// length kept, so the layers of a split render can be cut to the same.
pub(crate) fn trim_tail(left: &mut Vec<f32>, right: &mut Vec<f32>, sr: f32) -> usize {
    let threshold = 10f32.powf(-70.0 / 20.0);
    let last = (0..left.len()).rev().find(|&i| left[i].abs() > threshold || right[i].abs() > threshold);
    let end = last.map_or(0, |i| (i + (0.1 * sr) as usize).min(left.len()));
    left.truncate(end);
    right.truncate(end);
    fade_out(left, right, sr);
    end
}

/// Below this the sum is too near zero to be divided by: its own rounding is
/// most of what is left of it, and the ratio it gives is noise. -120 dBFS.
const CURVE_FLOOR: f32 = 1e-6;

/// No master can multiply a sample by more than this. Saturation's gain on a
/// small signal is its drive over `tanh(drive)`, at most 4; a compressor's
/// make-up is the only other lift, and 36 dB of it is far past anything a song
/// asks for. A wider clamp than the chain can reach, so it never decides a
/// sample — only catches one the arithmetic lost.
const CURVE_CEILING: f32 = 64.0;

/// What the master's dynamics did to the mix, sample for sample and per
/// channel, as a number to multiply by.
///
/// Every stage after the layers are summed — saturation, the bus compressor,
/// the clipper, the limiter, the fade at the end — comes out as the summed
/// signal times something, the nonlinear ones included: a waveshaper's output
/// divided by its input *is* that something at that sample. So `post / pre` is
/// exact, and a layer multiplied by it is that layer's share of the mastered
/// mix: `sum(layer[n] * g[n]) = g[n] * pre[n] = post[n]`.
///
/// Where the sum is under [`CURVE_FLOOR`] the last gain is held. The layers
/// there sum to nothing, so `g * 0 = 0` whatever `g` is and the mix is
/// reproduced either way; holding keeps a layer that is loud only because
/// another cancels it from being multiplied by a number rounding chose.
///
/// `pre` is consumed and written over: it is the length of the mix, and a
/// second buffer of that size is worth avoiding.
pub(crate) fn master_gain_curve(mut pre: [Vec<f32>; 2], post: (&[f32], &[f32]), end: usize) -> [Vec<f32>; 2] {
    let post = [post.0, post.1];
    for (curve, post) in pre.iter_mut().zip(post) {
        curve.truncate(end);
        let mut held = 1.0f32;
        for (g, post) in curve.iter_mut().zip(post) {
            let ratio = if g.abs() > CURVE_FLOOR { *post / *g } else { held };
            held = if ratio.is_finite() { ratio.clamp(0.0, CURVE_CEILING) } else { held };
            *g = held;
        }
        // The mix cannot outlast what it was made of, so this is never short —
        // but a curve is the mix's length whatever happens, or a layer would
        // be multiplied over part of itself and left alone over the rest.
        curve.resize(end, held);
    }
    pre
}

/// Multiplies a layer by the master's gain curve, in place.
pub(crate) fn apply_curve(curve: &[Vec<f32>; 2], left: &mut [f32], right: &mut [f32]) {
    for (channel, samples) in curve.iter().zip([left, right]) {
        for (s, g) in samples.iter_mut().zip(channel) {
            *s *= g;
        }
    }
}

/// A number for the curve, so a stem written through it can be kept under a
/// name no other render of the song would claim. Any change anywhere in the
/// song changes the mix, and so the curve.
pub(crate) fn curve_key(curve: &[Vec<f32>; 2]) -> u64 {
    let mut hash = crate::hash::Fnv::default();
    for channel in curve {
        hash = hash.u64(channel.len() as u64);
        for g in channel {
            hash = hash.u64(g.to_bits() as u64);
        }
    }
    hash.finish()
}

/// How long the fade at the very end of a render is.
pub const FADE_SECONDS: f32 = 0.05;

/// A 50 ms fade at the very end.
pub(crate) fn fade_out(left: &mut [f32], right: &mut [f32], sr: f32) {
    let end = left.len();
    let fade = ((FADE_SECONDS * sr) as usize).min(end);
    for k in 0..fade {
        let g = k as f32 / fade as f32;
        left[end - 1 - k] *= g;
        right[end - 1 - k] *= g;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two tracks in two layers, with the things that used to make a solo
    /// render a different take: a random drift per note, an LFO with a random
    /// phase, and sends into the delay and the reverb.
    const SONG: &str = "tempo 120
instrument a synth
  osc saw voices=3 spread=12
  drift 8
  lfo pitch rate=5 depth=10
instrument b synth
  osc square
pattern p
  C4:q D4 E4 F4 |
track one
  instrument a
  reverb 0.3
  delay 0.2
  play p
track two
  instrument b
  layer other
  pan 0.4
  play p transpose=5
master
  gain 2
  limiter off
  comp off
";

    fn timeline() -> Timeline {
        crate::compile(SONG).expect("the test song compiles").0
    }

    /// With a linear master, the layers sum to the mix sample for sample —
    /// which is only true when each track was rendered once, as itself.
    #[test]
    fn the_layers_sum_to_the_mix_when_the_master_is_linear() {
        let rendering = render_layers(&timeline(), 48_000, HashMap::new(), true);
        assert_eq!(rendering.layers.iter().map(|l| l.layer.as_str()).collect::<Vec<_>>(), ["one", "other"]);
        let mix = &rendering.mix;
        assert!(mix.left.len() > 48_000, "the song renders something");
        let mut worst = 0.0f32;
        for (i, (l, r)) in mix.left.iter().zip(&mix.right).enumerate() {
            let (mut sl, mut sr) = (0.0f32, 0.0f32);
            for layer in &rendering.layers {
                assert_eq!(layer.audio.left.len(), mix.left.len(), "a layer is the mix's length");
                sl += layer.audio.left[i];
                sr += layer.audio.right[i];
            }
            worst = worst.max((sl - l).abs()).max((sr - r).abs());
        }
        assert!(worst < 1e-4, "the layers differ from the mix by up to {worst}");
    }


    /// The same song with a master that is anything but linear: saturation, a
    /// bus compressor and a limiter, all working.
    const LOUD: &str = "tempo 120
instrument a synth
  osc saw voices=3 spread=12
  drift 8
  lfo pitch rate=5 depth=10
instrument b synth
  osc square
pattern p
  C4:q D4 E4 F4 |
track one
  instrument a
  gain 6
  reverb 0.3
  delay 0.2
  play p
track two
  instrument b
  layer other
  gain 6
  pan 0.4
  play p transpose=5
master
  gain 6
  saturation 0.6
  comp threshold=-18 ratio=4 attack=5ms release=120ms makeup=3
  clip -2
  limiter ceiling=-1 release=80ms
";

    /// The point of `stems_through_master`: with a master that saturates,
    /// compresses, clips and limits, the layers still sum to the mix — not to
    /// the mix before those, which is 4 dB louder and moves with the music.
    #[test]
    fn the_layers_sum_to_the_mastered_mix_when_they_go_through_its_gain() {
        let timeline = crate::compile(LOUD).expect("the loud song compiles").0;
        let options = RenderOptions { split: true, cache: None, stems_through_master: true };
        let rendering = render_with(&timeline, 48_000, HashMap::new(), &options);
        let mix = &rendering.mix;
        assert!(mix.left.len() > 48_000, "the song renders something");
        assert!(rendering.master_curve_key.is_some(), "the curve is named");
        let mut worst = 0.0f32;
        for i in 0..mix.left.len() {
            let (mut sl, mut sr) = (0.0f32, 0.0f32);
            for layer in &rendering.layers {
                assert_eq!(layer.audio.left.len(), mix.left.len(), "a layer is the mix's length");
                sl += layer.audio.left[i];
                sr += layer.audio.right[i];
            }
            worst = worst.max((sl - mix.left[i]).abs()).max((sr - mix.right[i]).abs());
        }
        // -100 dBFS: what is left is the order the three numbers were added in.
        assert!(worst < 1e-5, "the layers differ from the mastered mix by up to {worst}");
    }

    /// And without it they do not: the difference is the master's own work,
    /// which is what the user hears as the stems being quieter than the mix.
    #[test]
    fn the_layers_do_not_sum_to_the_mastered_mix_without_it() {
        let timeline = crate::compile(LOUD).expect("the loud song compiles").0;
        let rendering = render_layers(&timeline, 48_000, HashMap::new(), true);
        let mix = &rendering.mix;
        let mut worst = 0.0f32;
        for i in 0..mix.left.len() {
            let sl: f32 = rendering.layers.iter().map(|l| l.audio.left[i]).sum();
            worst = worst.max((sl - mix.left[i]).abs());
        }
        assert!(worst > 0.05, "this master would have to be doing something, and moved by {worst}");
    }

    /// Asking for the layers through the master must not change the mix.
    #[test]
    fn the_mix_is_the_same_whether_or_not_the_layers_go_through_the_master() {
        let timeline = crate::compile(LOUD).expect("the loud song compiles").0;
        let plain = render_layers(&timeline, 48_000, HashMap::new(), true).mix;
        let options = RenderOptions { split: true, cache: None, stems_through_master: true };
        let through = render_with(&timeline, 48_000, HashMap::new(), &options).mix;
        assert_eq!(plain.left.len(), through.left.len());
        let worst = plain.left.iter().zip(&through.left).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert_eq!(worst, 0.0, "the mixes differ by up to {worst}");
    }

    /// Where the layers cancel, the sum is too near zero to divide by and the
    /// last gain is held — and the sum is still the mix, because the layers
    /// there sum to nothing whatever they are multiplied by.
    #[test]
    fn a_sum_of_nothing_is_still_the_mix() {
        let pre = [vec![1.0f32, 0.0, 1e-9, 0.5], vec![1.0f32, 0.0, 1e-9, 0.5]];
        let curve = master_gain_curve(pre, (&[0.5, 0.0, 0.0, 0.25], &[0.5, 0.0, 0.0, 0.25]), 4);
        assert_eq!(curve[0], vec![0.5, 0.5, 0.5, 0.5], "the gain is held across what cannot be divided");
        assert!(curve[0].iter().all(|g| g.is_finite()));
    }

    /// Asking for the layers must not change the mix.
    #[test]
    fn the_mix_is_the_same_whether_or_not_it_is_split() {
        let timeline = timeline();
        let (plain, _) = render(&timeline, 48_000, HashMap::new());
        let split = render_layers(&timeline, 48_000, HashMap::new(), true).mix;
        assert_eq!(plain.left.len(), split.left.len());
        let worst = plain.left.iter().zip(&split.left).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "the mixes differ by up to {worst}");
    }
}
