//! Rendering a song in order of time, so it can be heard while it renders.
//!
//! **Why it is here.** A four-minute song takes seconds to render, and an
//! editor cannot play a note of it until the last one is mixed: every track is
//! synthesised in full, every layer is reverberated in full, and only then does
//! the master run over the sum. Nothing can be heard until everything is done.
//! `mat render --stream` turns that around. It walks the song from its first
//! bar to its last and finishes each stretch as it passes: the notes that start
//! in it, their tracks' inserts, their layers' sends, the master. A stretch is
//! written the moment it is finished, and the ones after it are still to come.
//!
//! **Why it is the same render.** Every effect that carries something from one
//! sample to the next — a filter's state, a delay line, the reverb tank, a
//! compressor's envelope, an LFO's phase, the limiter's look-ahead — is *kept*
//! here from one stretch to the next, rather than started again. So a stretch
//! boundary is not a boundary at all: the delay goes on repeating across it,
//! the reverb goes on ringing, the sidechain goes on breathing. That is what
//! separates this from `mat render --bars`, which renders a window of the song
//! from silence and cannot carry a tail into it. A streamed render is the
//! ordinary render's own samples, and [`crate::stream::tests`] says so over
//! songs with reverb, delay, sidechain and a bus compressor.
//!
//! **What it costs.** Nothing much. The parallelism moves: it is inside a
//! stretch rather than across the song, and a stretch holds every layer where
//! `render_with` runs three side by side, so the whole render comes out about
//! as fast either way. The first sound arrives in a fraction of a second
//! instead of after all of it.
//!
//! **What has to be rendered first.** A voice that is a function of its note is
//! made when its bar is reached: a synth note, a drum hit, a sampler's zones, a
//! region of an audio file. A tb303 is not a function of its note — one voice
//! runs the length of the track — but it is still made a stretch at a time,
//! because everything it carries is in [`crate::instruments::tb303::Player`]
//! and carries across a boundary like the master's own effects do. Nor is a
//! CLAP plugin, which is asked for blocks of 256 frames and keeps its own
//! state between them — so it is asked for the blocks the stretch needs and no
//! more, on the one thread it is played from. What is left, and is rendered in
//! full before the first stretch, is a scratch track, which cuts its record
//! out of a file or another track, and the tracks it cuts from. So is an Audio
//! Unit track, whose audio another process has already rendered.

use std::collections::HashMap;

use rayon::prelude::*;

use crate::arrange::{Duck, Sweep, Timeline, TimelineTrack};
use crate::dsp::biquad::EqChain;
use crate::dsp::chorus::Chorus;
use crate::dsp::delay::PingPongDelay;
use crate::dsp::dynamics::{Compressor, KeyedCompressor};
use crate::dsp::limiter::Limiter;
use crate::dsp::phaser::Phaser;
use crate::dsp::quiet::BLOCK;
use crate::dsp::reverb::PlateReverb;
use crate::dsp::{StereoClip, db_to_gain, pan_gains};
use crate::instruments::drums;
use crate::instruments::synth;
use crate::model::{DrumKind, InstrumentKind, Pitch};
use crate::render::{Audio, LayerAudio, RenderOptions, RenderReport, Rendering, Timing};
use crate::sampler::Prepared;
use crate::sampler::Sampler;
use crate::sampler::audio_file::AudioFile;

/// The first stretch rendered, in [`BLOCK`]s. Small, because nothing can be
/// heard until it lands.
const FIRST_BLOCKS: usize = 16;
/// The largest stretch, in [`BLOCK`]s. The stretches double up to it: the
/// first is quick to reach and the later ones are cheap per sample.
const MOST_BLOCKS: usize = 256;
/// How much rendered audio a track holds on to after it has been through its
/// inserts, before it is worth moving the rest of the buffer down over it.
const COMPACT: usize = 1 << 16;

/// A stretch of the finished mix, in order, ready to be written.
pub struct Part {
    /// Where it begins in the finished render, in frames.
    pub from: usize,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Part {
    pub fn frames(&self) -> usize {
        self.left.len()
    }
}

/// A render in progress. [`Stream::next`] renders and returns the next stretch
/// of the mix; [`Stream::finish`] gives back the whole render, exactly as
/// [`crate::render::render_with`] would have.
pub struct Stream<'a> {
    timeline: &'a Timeline,
    sample_rate: u32,
    sr: f32,
    split: bool,
    timing: Timing,

    groups: Vec<(String, Vec<usize>)>,
    keys: Vec<Option<u64>>,
    cached: Vec<bool>,
    cache: Option<crate::render_cache::LayerCache>,

    tracks: Vec<TrackState>,
    layers: Vec<LayerState>,
    key_tracks: Vec<usize>,
    key_used: bool,

    /// The plugin tracks being played, and the track each one is. They are not
    /// in [`TrackState`] because a plugin is played on one thread and the
    /// tracks are rendered across all of them: these are run here, on the
    /// thread that calls [`Stream::next`], before the rest of the stretch. It
    /// is also what makes a `Stream` not `Send`.
    claps: Vec<(usize, crate::render::ClapPlayer)>,

    master: MasterState,

    /// The next frame to render, counted from the render's own start (the
    /// lead-in included, when this is part of a song).
    at: usize,
    blocks: usize,
    /// The mix as the master has left it, from frame 0.
    mix_left: Vec<f32>,
    mix_right: Vec<f32>,
    /// The mix before the master's dynamics, from frame 0, kept only when the
    /// layers are to go through the master's gain curve — see
    /// `render::master_gain_curve`. Empty otherwise.
    pre_left: Vec<f32>,
    pre_right: Vec<f32>,
    through_master: bool,
    /// The peak of the layers' sum, before the master's dynamics.
    layers_peak: f32,
    /// Frames already given out by `next`, counted from the first bar asked
    /// for — the lead-in dropped.
    emitted: usize,
    lead_in: usize,
    lead_faded: bool,
    finished: bool,

    warnings: Vec<String>,
    skipped: Vec<String>,
}

// ---------------------------------------------------------------- the tracks

/// Where a track's audio comes from, a stretch at a time.
enum Voices {
    /// A note at a time, in the order the notes are played.
    Synth(crate::model::SynthDef),
    Drums(Box<crate::model::DrumKit>),
    Sampler(Box<Prepared>),
    /// A region of a file at a time, in the order the clips are played.
    Audio(Box<AudioFile>),
    /// One voice running the length of the track, a stretch of samples at a
    /// time, carrying its filter, slide and envelopes across the boundary.
    Tb303(Box<crate::instruments::tb303::Player>),
    /// Rendered before the first stretch; nothing left to make.
    Ready,
}

/// One track, as much of it as has been rendered.
struct TrackState {
    ti: usize,
    needed: bool,
    voices: Voices,
    /// The next note (or audio clip) to render.
    next: usize,
    /// Rendered and not yet sent through the inserts, from frame `dry_at`.
    dry_at: usize,
    dry_left: Vec<f32>,
    dry_right: Vec<f32>,
    /// The first frame the track has any sound at — `process_track`'s `start`,
    /// which is where its inserts begin.
    start: Option<usize>,
    /// The frame after the last one any clip reaches.
    content: usize,
    chain: Option<TrackChain>,
    /// How far the inserts have been fed, in frames.
    fed: usize,
    /// Through the inserts and not yet mixed, from frame `out_at`.
    out_at: usize,
    out_left: Vec<f32>,
    out_right: Vec<f32>,
    /// Every note rendered: `content` will not grow again.
    all_rendered: bool,
    /// Where the track's audio ends, once the inserts have been fed to there.
    /// `None` while there is more of it to come.
    end: Option<usize>,
    warnings: Vec<String>,
}

/// A track's insert chain and everything it carries between samples.
struct TrackChain {
    eq: Option<EqChain>,
    comp: Option<Compressor>,
    chorus: Option<Chorus>,
    phaser: Option<Phaser>,
    duck: Option<Duck>,
    gain_sweeps: Vec<Sweep>,
    gl: f32,
    gr: f32,
    sr: f32,
}

impl TrackChain {
    fn new(track: &TimelineTrack, sr: f32) -> Self {
        let gain = db_to_gain(track.gain_db);
        let (pl, pr) = pan_gains(track.pan);
        TrackChain {
            eq: track.eq.as_ref().map(|eq| EqChain::new(eq, sr)),
            comp: track.comp.as_ref().map(|c| Compressor::new(c, sr)),
            chorus: track.chorus.as_ref().map(|c| Chorus::new(c, sr)),
            phaser: track.phaser.as_ref().map(|p| Phaser::new(p, sr)),
            duck: track.duck.clone(),
            gain_sweeps: track.sweeps.iter().filter(|s| s.param == "gain").cloned().collect(),
            gl: gain * pl,
            gr: gain * pr,
            sr,
        }
    }

    /// The tail a chorus adds past the last sample of a track's clips, as
    /// `process_track` adds it.
    fn tail(&self) -> usize {
        if self.chorus.is_some() { (0.05 * self.sr) as usize } else { 0 }
    }

    /// The next stretch of the track, which begins at frame `at`.
    fn process(&mut self, at: usize, left: &mut [f32], right: &mut [f32]) {
        if let Some(eq) = &mut self.eq {
            eq.process(left, right);
        }
        if let Some(comp) = &mut self.comp {
            comp.process(left, right);
        }
        if let Some(chorus) = &mut self.chorus {
            chorus.process(left, right);
        }
        if let Some(phaser) = &mut self.phaser {
            phaser.process(left, right);
        }
        if let Some(duck) = &self.duck {
            duck_into(duck, at, left, right, self.sr);
        }
        if !self.gain_sweeps.is_empty() {
            for i in 0..left.len() {
                let t = (at + i) as f64 / self.sr as f64;
                let mut db = None;
                for s in &self.gain_sweeps {
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
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            *l *= self.gl;
            *r *= self.gr;
        }
    }
}

/// The sidechain dip over frames `at .. at + left.len()`, which is
/// `render::apply_duck`'s curve read at absolute frames instead of over one
/// buffer.
fn duck_into(duck: &Duck, at: usize, left: &mut [f32], right: &mut [f32], sr: f32) {
    let attack = (duck.attack.max(0.001) * sr) as usize;
    let release = (duck.release.max(0.01) * sr) as usize;
    let span = attack + release;
    let mut shape = vec![0.0f32; left.len()];
    for &t in &duck.times {
        let hit = (t * sr as f64) as usize;
        if hit + span <= at || hit >= at + shape.len() {
            continue;
        }
        for k in 0..span {
            let idx = hit + k;
            if idx < at {
                continue;
            }
            let Some(s) = shape.get_mut(idx - at) else { break };
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

// ---------------------------------------------------------------- the layers

/// One layer: its send effects, the master's linear stages, and what it has
/// come to so far.
struct LayerState {
    delay: Option<PingPongDelay>,
    reverb: Option<PlateReverb>,
    sidechain: Option<KeyedCompressor>,
    eq: Option<EqChain>,
    /// Read back from the cache rather than rendered, and sliced from here.
    from_cache: Option<(Vec<f32>, Vec<f32>)>,
    /// The layer so far, which is what the cache is given at the end and what
    /// a stem is cut from.
    left: Vec<f32>,
    right: Vec<f32>,
    /// How long the layer is: its last sound plus the send effects' tail.
    /// Known once every track in it has been rendered whole.
    length: Option<usize>,
}

/// The master's dynamics, and the state they carry.
struct MasterState {
    saturation: f32,
    comp: Option<Compressor>,
    clip_db: f32,
    limiter: Option<Limiter>,
}

// ---------------------------------------------------------------- the render

impl<'a> Stream<'a> {
    /// Sets a streamed render going. Nothing of the song is rendered here
    /// except what cannot be rendered a bar at a time — see the module's docs.
    pub fn start(timeline: &'a Timeline, sample_rate: u32, mut stems: HashMap<usize, StereoClip>, options: &RenderOptions) -> Stream<'a> {
        let sr = sample_rate as f32;
        let timing = Timing::from_env();
        let master = &timeline.master;
        let mut warnings = Vec::new();
        let mut skipped = Vec::new();

        // Which tracks go together: one group per layer, in the order the
        // layers first appear — as `render::render_with` groups them.
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (ti, track) in timeline.tracks.iter().enumerate() {
            match groups.iter_mut().find(|(layer, _)| layer == &track.layer) {
                Some((_, members)) => members.push(ti),
                None => groups.push((track.layer.clone(), vec![ti])),
            }
        }

        let key_source = master.sidechain.as_ref().map(|s| s.source.clone());
        let is_key = |track: &TimelineTrack| key_source.as_ref().is_some_and(|k| &track.name == k || &track.layer == k);

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

        // The tracks whose sound has to be made, exactly as `render_with`
        // chooses them: the members of every layer that is not cached, the
        // records their scratch tracks cut from, and the sidechain's key.
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

        // A scratch track cuts its record out of another track, so that track
        // has to be there whole before the first stretch — and so does every
        // track when the record is the mix.
        let mut eager = vec![false; timeline.tracks.len()];
        for (ti, track) in timeline.tracks.iter().enumerate() {
            if !needed[ti] {
                continue;
            }
            match &track.instrument {
                InstrumentKind::Scratch(def) => {
                    eager[ti] = true;
                    if let Some(source) = &def.source_track {
                        for (si, other) in timeline.tracks.iter().enumerate() {
                            let wanted = if source == "mix" { !matches!(other.instrument, InstrumentKind::Scratch(_)) } else { &other.name == source };
                            if wanted {
                                eager[si] = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Plugins are loaded and set going here, on the calling thread, one at
        // a time: many expect a single host thread. Nothing is asked of them
        // yet — `render_one` plays each one as far as the stretch it is
        // rendering — unless a scratch track cuts its record out of the
        // plugin's audio, which wants all of it before the first stretch.
        let mut plugin_results: Vec<(usize, Result<Vec<StereoClip>, String>)> = Vec::new();
        let mut claps: Vec<(usize, crate::render::ClapPlayer)> = Vec::new();
        for (ti, track) in timeline.tracks.iter().enumerate() {
            if needed[ti]
                && !track.silent
                && let InstrumentKind::Clap(def) = &track.instrument
            {
                match crate::render::start_clap(def, &track.notes, sr) {
                    Ok(mut player) => match eager[ti] {
                        true => plugin_results.push((ti, player.render_to(usize::MAX).map(|c| vec![c]))),
                        false => claps.push((ti, player)),
                    },
                    Err(e) => plugin_results.push((ti, Err(e))),
                }
            }
        }

        let results: Vec<(usize, Result<Vec<StereoClip>, String>)> = timeline
            .tracks
            .par_iter()
            .enumerate()
            .filter(|(ti, _)| needed[*ti] && eager[*ti] && !matches!(timeline.tracks[*ti].instrument, InstrumentKind::Clap(_)))
            .filter_map(|(ti, track)| crate::render::render_track(track, sr).map(|r| (ti, r)))
            .collect::<Vec<_>>()
            .into_iter()
            .chain(plugin_results)
            .collect();
        let mut ready: HashMap<usize, Vec<StereoClip>> = HashMap::new();
        for (ti, result) in results {
            match result {
                Ok(clips) => {
                    ready.insert(ti, clips);
                }
                Err(e) => warnings.push(format!("track '{}': {e}", timeline.tracks[ti].name)),
            }
        }
        // Scratch tracks that use another track as their record come last:
        // they need that track's dry audio.
        for (ti, track) in timeline.tracks.iter().enumerate() {
            if !needed[ti] {
                continue;
            }
            let InstrumentKind::Scratch(def) = &track.instrument else { continue };
            let Some(src_name) = &def.source_track else { continue };
            let source_clips: Vec<&StereoClip> = if src_name == "mix" {
                ready
                    .iter()
                    .filter(|(i, _)| !matches!(timeline.tracks[**i].instrument, InstrumentKind::Scratch(_)))
                    .flat_map(|(_, c)| c.iter())
                    .collect()
            } else {
                let Some(si) = timeline.tracks.iter().position(|t| &t.name == src_name) else { continue };
                match ready.get(&si) {
                    Some(clips) => clips.iter().collect(),
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
            // The record should sound like the source track does in the mix.
            if let Some(src) = timeline.tracks.iter().find(|t| &t.name == src_name) {
                if let Some(eq) = &src.eq {
                    crate::dsp::biquad::apply_eq(eq, &mut region.0, &mut region.1, sr);
                }
                let g = db_to_gain(src.gain_db);
                region.0.iter_mut().chain(region.1.iter_mut()).for_each(|s| *s *= g);
            }
            match crate::instruments::scratch::render_region(def, region.0, region.1, sr as f64, &track.notes, sr) {
                Ok(clips) => {
                    ready.insert(ti, clips);
                }
                Err(e) => warnings.push(format!("track '{}': {e}", track.name)),
            }
        }
        for (ti, track) in timeline.tracks.iter().enumerate() {
            if let InstrumentKind::AudioUnit(au) = &track.instrument {
                match stems.remove(&ti) {
                    Some(mut stem) => {
                        let gain = db_to_gain(au.gain_db);
                        stem.left.iter_mut().chain(stem.right.iter_mut()).for_each(|s| *s *= gain);
                        ready.insert(ti, vec![stem]);
                    }
                    None => skipped.push(track.name.clone()),
                }
            }
        }
        timing.mark("what cannot be rendered a bar at a time");

        let mut tracks: Vec<TrackState> = timeline
            .tracks
            .iter()
            .enumerate()
            .map(|(ti, track)| {
                let mut state = TrackState {
                    ti,
                    needed: needed[ti] && !track.silent,
                    voices: Voices::Ready,
                    next: 0,
                    dry_at: 0,
                    dry_left: Vec::new(),
                    dry_right: Vec::new(),
                    start: None,
                    content: 0,
                    chain: None,
                    fed: 0,
                    out_at: 0,
                    out_left: Vec::new(),
                    out_right: Vec::new(),
                    all_rendered: true,
                    end: None,
                    warnings: Vec::new(),
                };
                if !state.needed {
                    return state;
                }
                if let Some(clips) = ready.remove(&ti) {
                    state.take(&clips);
                    return state;
                }
                state.voices = match &track.instrument {
                    InstrumentKind::Synth(def) => Voices::Synth(def.clone()),
                    InstrumentKind::Drums(kit) => Voices::Drums(Box::new(kit.clone())),
                    InstrumentKind::Sampler(def) => match Sampler::load(std::path::Path::new(&def.load)).and_then(|s| s.prepare(&track.notes, def, sr)) {
                        Ok(prepared) => Voices::Sampler(Box::new(prepared)),
                        Err(e) => {
                            state.warnings.push(format!("track '{}': {e}", track.name));
                            Voices::Ready
                        }
                    },
                    InstrumentKind::Samples(def) => match Sampler::from_zones(&def.zones).and_then(|s| s.prepare(&track.notes, &def.settings, sr)) {
                        Ok(prepared) => Voices::Sampler(Box::new(prepared)),
                        Err(e) => {
                            state.warnings.push(format!("track '{}': {e}", track.name));
                            Voices::Ready
                        }
                    },
                    InstrumentKind::Audio(source) => match AudioFile::open(std::path::Path::new(&source.path)) {
                        Ok(file) => Voices::Audio(Box::new(file)),
                        Err(e) => {
                            state.warnings.push(format!("track '{}': {e}", track.name));
                            Voices::Ready
                        }
                    },
                    InstrumentKind::Tb303(def) => match crate::instruments::tb303::Player::new(def, &track.notes, &track.sweeps, sr) {
                        Some(player) => Voices::Tb303(Box::new(player)),
                        None => Voices::Ready,
                    },
                    _ => Voices::Ready,
                };
                // A plugin track makes nothing of itself: `render_one` plays
                // the plugin and puts its clips here, and there is more of it
                // to come until the plugin says otherwise.
                state.all_rendered = matches!(state.voices, Voices::Ready) && !claps.iter().any(|(i, _)| *i == ti);
                state
            })
            .collect();
        for track in &mut tracks {
            warnings.append(&mut track.warnings);
        }
        timing.mark("samplers and audio files opened");

        let layers: Vec<LayerState> = (0..groups.len())
            .map(|g| {
                let from_cache = if cached[g] {
                    match (&cache, keys[g]) {
                        (Some(cache), Some(key)) => match cache.load(key) {
                            Ok(buffer) => Some(buffer),
                            Err(e) => {
                                // A cache file that went between the check and
                                // the read: said, and the layer is silent in
                                // this render rather than the render failing.
                                warnings.push(format!("layer '{}': cached audio could not be read: {e}", groups[g].0));
                                Some((Vec::new(), Vec::new()))
                            }
                        },
                        _ => Some((Vec::new(), Vec::new())),
                    }
                } else {
                    None
                };
                let length = from_cache.as_ref().map(|(l, _)| l.len());
                LayerState {
                    delay: master.delay.enabled.then(|| {
                        let mut delay = PingPongDelay::new(timeline.delay_seconds, master.delay.feedback, master.delay.tone_hz, sr);
                        delay.set_modulation(master.delay.mod_ms, master.delay.mod_rate_hz);
                        delay
                    }),
                    reverb: master.reverb.enabled.then(|| PlateReverb::new(&master.reverb, sr)),
                    sidechain: master.sidechain.as_ref().map(|sc| KeyedCompressor::new(sc, sr)),
                    eq: master.eq.as_ref().map(|eq| EqChain::new(eq, sr)),
                    from_cache,
                    left: Vec::new(),
                    right: Vec::new(),
                    length,
                }
            })
            .collect();

        let key_tracks: Vec<usize> = (0..timeline.tracks.len()).filter(|ti| is_key(&timeline.tracks[*ti])).collect();

        Stream {
            timeline,
            sample_rate,
            sr,
            split: options.split,
            timing,
            groups,
            keys,
            cached,
            cache,
            tracks,
            layers,
            key_tracks,
            key_used: key_source.is_some() && any_rendered,
            claps,
            master: MasterState {
                saturation: master.saturation,
                comp: master.comp.as_ref().map(|c| Compressor::new(c, sr)),
                clip_db: master.clip_db,
                limiter: master.limiter.enabled.then(|| Limiter::new(master.limiter.ceiling_db, master.limiter.release_ms, sr)),
            },
            at: 0,
            blocks: FIRST_BLOCKS,
            mix_left: Vec::new(),
            mix_right: Vec::new(),
            pre_left: Vec::new(),
            pre_right: Vec::new(),
            through_master: options.split && options.stems_through_master,
            layers_peak: 0.0,
            emitted: 0,
            lead_in: timeline.window.as_ref().map_or(0, |w| w.lead_in_frames(sample_rate)),
            lead_faded: false,
            finished: false,
            warnings,
            skipped,
        }
    }

    fn render_one(&mut self) -> Part {
        let started = std::time::Instant::now();
        let from = self.at;
        let to = from + self.blocks * BLOCK;
        self.blocks = (self.blocks * 2).min(MOST_BLOCKS);

        // The plugins first, here, on this one thread: each is played up to
        // where the stretch ends, in the blocks it would have been played in
        // anyway, and what it made is its track's audio for this stretch.
        let (sr, timeline) = (self.sr, self.timeline);
        for (ti, player) in &mut self.claps {
            match player.render_to(to + BLOCK) {
                Ok(clip) => {
                    self.tracks[*ti].add_clip(&clip);
                    self.tracks[*ti].all_rendered = player.finished();
                }
                Err(e) => {
                    self.warnings.push(format!("track '{}': {e}", timeline.tracks[*ti].name));
                    self.tracks[*ti].all_rendered = true;
                }
            }
        }
        if !self.claps.is_empty() {
            self.timing.mark("plugins");
        }

        // Every track a stretch further, and a block beyond it, so that the
        // inserts — whose silence check works in whole blocks — can be fed in
        // whole blocks from the track's own start and still reach `to`.
        self.tracks.par_iter_mut().for_each(|track| track.render_to(&timeline.tracks[track.ti], to + BLOCK, sr));
        for track in &mut self.tracks {
            self.warnings.append(&mut track.warnings);
        }

        // A layer is as long as its last sound plus the send effects' tail,
        // and that is known once every track in it has been rendered whole —
        // which is what lets a cached layer stay valid when another track's
        // last note moves. See `render::render_with`.
        let tail = (crate::render::tail_seconds(timeline) * sr as f64) as usize;
        for (g, layer) in self.layers.iter_mut().enumerate() {
            if layer.length.is_some() {
                continue;
            }
            let members = &self.groups[g].1;
            if !members.iter().all(|&ti| !self.tracks[ti].needed || self.tracks[ti].end.is_some()) {
                continue;
            }
            let content = members.iter().filter_map(|&ti| self.tracks[ti].end).max().unwrap_or(0);
            layer.length = Some(if content == 0 { 0 } else { content + tail });
        }

        // The master sidechain's key: every key track of the song, whichever
        // layer it is in, so a layer on its own is ducked as it is in the mix.
        let key = if self.key_used {
            let mut key = [vec![0.0f32; to - from], vec![0.0f32; to - from]];
            let mut length = 0;
            for &ti in &self.key_tracks {
                length = length.max(self.tracks[ti].add_into(&mut key, from, to, 1.0));
            }
            key[0].truncate(length.saturating_sub(from));
            key[1].truncate(length.saturating_sub(from));
            key
        } else {
            [Vec::new(), Vec::new()]
        };

        let (tracks, groups, timing) = (&self.tracks, &self.groups, &self.timing);
        let made: Vec<(usize, Vec<f32>, Vec<f32>)> = self
            .layers
            .par_iter_mut()
            .enumerate()
            .map(|(g, layer)| {
                let end = layer.length.map_or(to, |len| len.min(to));
                if end <= from {
                    return (g, Vec::new(), Vec::new());
                }
                if let Some((left, right)) = &layer.from_cache {
                    let take = |buffer: &Vec<f32>| buffer[from.min(buffer.len())..end.min(buffer.len())].to_vec();
                    return (g, take(left), take(right));
                }
                let n = end - from;
                let mut dry = [vec![0.0f32; n], vec![0.0f32; n]];
                let mut own_key = [vec![0.0f32; n], vec![0.0f32; n]];
                let mut reverb_bus = [vec![0.0f32; n], vec![0.0f32; n]];
                let mut delay_bus = [vec![0.0f32; n], vec![0.0f32; n]];
                for &ti in &groups[g].1 {
                    let track = &timeline.tracks[ti];
                    let keyed = tracks[ti].is_key(timeline, ti);
                    tracks[ti].add_into(if keyed { &mut own_key } else { &mut dry }, from, end, 1.0);
                    tracks[ti].add_into(&mut reverb_bus, from, end, track.reverb);
                    tracks[ti].add_into(&mut delay_bus, from, end, track.delay);
                }
                let (left, right) = layer.through_linear_master(timeline, dry, &key, &own_key, reverb_bus, delay_bus);
                (g, left, right)
            })
            .collect();
        timing.say(&format!("{from}..{to}: layers in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0));

        // The mix is the layers' sum, and reaches as far as the longest of them.
        let mut n = 0;
        for (g, left, right) in &made {
            self.layers[*g].left.extend_from_slice(left);
            self.layers[*g].right.extend_from_slice(right);
            n = n.max(left.len());
        }
        for track in &mut self.tracks {
            track.release(to);
        }
        let mut mix = [vec![0.0f32; n], vec![0.0f32; n]];
        for (_, left, right) in &made {
            for (i, (l, r)) in left.iter().zip(right).enumerate() {
                mix[0][i] += l;
                mix[1][i] += r;
            }
        }

        let [mut left, mut right] = mix;
        self.layers_peak = left.iter().chain(&right).fold(self.layers_peak, |m, s| m.max(s.abs()));
        if self.through_master {
            self.pre_left.extend_from_slice(&left);
            self.pre_right.extend_from_slice(&right);
        }
        if self.master.saturation > 0.0 {
            crate::dsp::dynamics::saturate(self.master.saturation, &mut left, &mut right);
        }
        if let Some(comp) = &mut self.master.comp {
            comp.process(&mut left, &mut right);
        }
        if self.master.clip_db < 0.0 {
            crate::dsp::dynamics::clip(self.master.clip_db, &mut left, &mut right);
        }
        self.at = from + n;
        // Every layer at its end: the song is rendered, and the limiter can
        // let go of its look-ahead.
        let over = self.layers.iter().all(|l| l.length.is_some_and(|len| len <= self.at));
        match &mut self.master.limiter {
            Some(limiter) => {
                for (l, r) in left.iter().zip(&right) {
                    limiter.push(*l, *r);
                }
                if over {
                    limiter.finish();
                }
                let (mut done_left, mut done_right) = (Vec::new(), Vec::new());
                limiter.take(&mut done_left, &mut done_right);
                self.mix_left.append(&mut done_left);
                self.mix_right.append(&mut done_right);
            }
            None => {
                self.mix_left.append(&mut left);
                self.mix_right.append(&mut right);
            }
        }
        self.finished = over;

        // Only part of the song: the lead-in was rendered so that notes which
        // begin just before the stretch are heard ringing at its start, and it
        // is dropped here. Its first 5 ms fade, because a stretch begins in
        // the middle of the music and a file whose first sample is far from
        // zero clicks; nothing is given out until there is a whole fade to
        // give, which is a few milliseconds, once.
        let ready = self.mix_left.len().saturating_sub(self.lead_in);
        let fade = (0.005 * self.sr) as usize;
        if self.lead_in > 0 && !self.lead_faded {
            if ready < fade && !self.finished {
                return Part { from: self.emitted, left: Vec::new(), right: Vec::new() };
            }
            let fade = fade.min(ready);
            for k in 0..fade {
                let g = k as f32 / fade as f32;
                self.mix_left[self.lead_in + k] *= g;
                self.mix_right[self.lead_in + k] *= g;
            }
            self.lead_faded = true;
        }
        let part = Part {
            from: self.emitted,
            left: self.mix_left[self.lead_in + self.emitted..self.lead_in + ready].to_vec(),
            right: self.mix_right[self.lead_in + self.emitted..self.lead_in + ready].to_vec(),
        };
        self.emitted = ready;
        part
    }

    /// The finished render, as `render::render_with` gives it: the mix, the
    /// layers when they were asked for, and what the render has to say.
    ///
    /// The mix here is the mix [`Stream::next`] gave out, with trailing
    /// near-silence cut and the last 50 ms faded — so it can be *shorter* than
    /// what was given out, and never longer, and never different before the
    /// fade.
    pub fn finish(mut self) -> Rendering {
        let (mut left, mut right) = (std::mem::take(&mut self.mix_left), std::mem::take(&mut self.mix_right));
        let layers_peak_db = 20.0 * self.layers_peak.max(1e-9).log10();
        let end = crate::render::trim_tail(&mut left, &mut right, self.sr);
        self.timing.mark("streamed");

        // What the master's dynamics did to the mix, stretch after stretch,
        // as one number per sample: the fade over the lead-in and the fade at
        // the end are in `left`/`right` and not in the pre-master mix, so both
        // are in the curve and a layer through it needs neither of them again.
        let curve = self.through_master.then(|| {
            let pre = [std::mem::take(&mut self.pre_left), std::mem::take(&mut self.pre_right)];
            crate::render::master_gain_curve(pre, (&left, &right), end)
        });
        let master_curve_key = curve.as_ref().map(crate::render::curve_key);

        if let Some(cache) = &self.cache {
            for (g, layer) in self.layers.iter().enumerate() {
                if self.cached[g] {
                    continue;
                }
                let Some(key) = self.keys[g] else { continue };
                if let Err(e) = cache.store(key, &layer.left, &layer.right) {
                    self.warnings.push(format!("layer '{}': not cached: {e}", self.groups[g].0));
                }
            }
            cache.keep(self.keys.iter().flatten().copied());
        }

        // The layers end where the mix ends, with the same fade, so they are
        // the mix's length and still sum to it.
        let mut layers: Vec<LayerAudio> = if self.split {
            self.groups
                .iter()
                .zip(&mut self.layers)
                .zip(self.keys.iter().zip(&self.cached))
                .map(|(((layer, _), state), (key, was_cached))| {
                    let (mut l, mut r) = (std::mem::take(&mut state.left), std::mem::take(&mut state.right));
                    l.resize(end, 0.0);
                    r.resize(end, 0.0);
                    match &curve {
                        Some(curve) => crate::render::apply_curve(curve, &mut l, &mut r),
                        None => crate::render::fade_out(&mut l, &mut r, self.sr),
                    }
                    LayerAudio { layer: layer.clone(), audio: Audio { sample_rate: self.sample_rate, left: l, right: r }, key: *key, cached: *was_cached }
                })
                .collect()
        } else {
            Vec::new()
        };

        if self.lead_in > 0 {
            // The mix's lead-in only has to go: its fade over the cut was made
            // while it streamed, so that what was given out and what comes back
            // here are the same samples.
            let frames = self.lead_in.min(left.len());
            left.drain(..frames);
            right.drain(..frames);
            for layer in &mut layers {
                match curve.is_some() {
                    // The fade over the cut came with the curve; only the
                    // lead-in itself is left to drop.
                    true => crate::render::drain_lead_in(&mut layer.audio.left, &mut layer.audio.right, self.lead_in),
                    false => crate::render::drop_lead_in(&mut layer.audio.left, &mut layer.audio.right, self.lead_in, self.sr),
                }
            }
        }

        Rendering {
            mix: Audio { sample_rate: self.sample_rate, left, right },
            layers,
            layers_peak_db,
            master_curve_key,
            report: RenderReport { skipped_tracks: self.skipped, warnings: self.warnings },
        }
    }
}

impl Iterator for Stream<'_> {
    type Item = Part;

    /// Renders the next stretch of the song and gives back what it added to
    /// the mix, or `None` when the whole song has been rendered.
    ///
    /// The samples that come back are final: nothing later in the song changes
    /// them. The one exception is the very end of the render, where trailing
    /// near-silence is cut and the last samples fade — see [`Stream::finish`],
    /// which gives back what the render kept.
    fn next(&mut self) -> Option<Part> {
        loop {
            if self.finished {
                return None;
            }
            let part = self.render_one();
            if !part.left.is_empty() {
                return Some(part);
            }
        }
    }
}

impl LayerState {
    /// The send effects and the master's linear stages over this stretch of
    /// one layer — `render::through_linear_master`, with everything that
    /// carries between samples kept from the stretch before.
    fn through_linear_master(
        &mut self,
        timeline: &Timeline,
        mut mix: [Vec<f32>; 2],
        key: &[Vec<f32>; 2],
        own_key: &[Vec<f32>; 2],
        mut reverb_bus: [Vec<f32>; 2],
        delay_bus: [Vec<f32>; 2],
    ) -> (Vec<f32>, Vec<f32>) {
        let master = &timeline.master;
        let len = mix[0].len();
        let mut wet = [vec![0.0f32; len], vec![0.0f32; len]];
        if let Some(delay) = &mut self.delay {
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
        if let Some(reverb) = &mut self.reverb {
            let [wl, wr] = &mut wet;
            reverb.process(&reverb_bus[0], &reverb_bus[1], wl, wr);
            for ch in 0..2 {
                for i in 0..len {
                    mix[ch][i] += wet[ch][i];
                }
            }
        }

        let [mut left, mut right] = mix;
        if let Some(sidechain) = &mut self.sidechain {
            sidechain.process(&key[0], &key[1], &mut left, &mut right);
            for (dst, keyed) in [&mut left, &mut right].into_iter().zip(own_key) {
                for (d, k) in dst.iter_mut().zip(keyed) {
                    *d += k;
                }
            }
        }
        let master_gain = db_to_gain(master.gain_db);
        for s in left.iter_mut().chain(right.iter_mut()) {
            *s *= master_gain;
        }
        if let Some(eq) = &mut self.eq {
            eq.process(&mut left, &mut right);
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
}

impl TrackState {
    fn is_key(&self, timeline: &Timeline, ti: usize) -> bool {
        timeline.master.sidechain.as_ref().is_some_and(|sc| {
            let track = &timeline.tracks[ti];
            sc.source == track.name || sc.source == track.layer
        })
    }

    /// Everything a track that was rendered before the first stretch has.
    fn take(&mut self, clips: &[StereoClip]) {
        for clip in clips {
            self.add_clip(clip);
        }
        self.all_rendered = true;
    }

    fn add_clip(&mut self, clip: &StereoClip) {
        if clip.left.is_empty() {
            return;
        }
        let end = clip.offset + clip.left.len();
        self.content = self.content.max(end);
        if self.start.is_none() {
            self.start = Some(clip.offset);
            self.dry_at = clip.offset;
        }
        if end <= self.dry_at {
            return;
        }
        let want = end - self.dry_at;
        if self.dry_left.len() < want {
            self.dry_left.resize(want, 0.0);
            self.dry_right.resize(want, 0.0);
        }
        for (i, (l, r)) in clip.left.iter().zip(&clip.right).enumerate() {
            let at = clip.offset + i;
            if at < self.dry_at {
                continue;
            }
            self.dry_left[at - self.dry_at] += l;
            self.dry_right[at - self.dry_at] += r;
        }
    }

    /// Renders every voice that starts before frame `to`, and sends what is
    /// complete through the track's inserts.
    fn render_to(&mut self, track: &TimelineTrack, to: usize, sr: f32) {
        if !self.needed {
            return;
        }
        // A voice's clip begins at its note, rounded to a sample, so rendering
        // every note that starts before `to` leaves nothing before `to` to
        // come. Half a block of slack costs a note or two and no correctness.
        let horizon = to as f64 + 64.0;
        let mut clips: Vec<StereoClip> = Vec::new();
        // A voice that runs the length of the track knows where its own audio
        // ends only when it has run out, because the silent tail is trimmed
        // off it — and that end is less than what it gave out.
        let mut trimmed_to = None;
        match &mut self.voices {
            Voices::Synth(def) => {
                let mut until = self.next;
                while until < track.notes.len() && track.notes[until].start * sr as f64 <= horizon {
                    until += 1;
                }
                clips = track.notes[self.next..until]
                    .par_iter()
                    .enumerate()
                    .map(|(k, note)| {
                        let i = self.next + k;
                        // Portamento glides from the last note that started before this one.
                        let from = if def.glide > 0.0 {
                            track.notes[..i].iter().rev().find(|p| p.start < note.start - 1e-6).map(|p| p.midi)
                        } else {
                            None
                        };
                        synth::render_note_from(def, note, from, &track.sweeps, sr, note.seed)
                    })
                    .collect();
                self.next = until;
                self.all_rendered = until == track.notes.len();
            }
            Voices::Drums(kit) => {
                let mut until = self.next;
                while until < track.notes.len() && track.notes[until].start * sr as f64 <= horizon {
                    until += 1;
                }
                clips = track.notes[self.next..until]
                    .par_iter()
                    .enumerate()
                    .map(|(k, note)| {
                        let i = self.next + k;
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
                    .collect();
                self.next = until;
                self.all_rendered = until == track.notes.len();
            }
            Voices::Sampler(prepared) => {
                let mut until = self.next;
                while until < track.notes.len() && track.notes[until].start * sr as f64 <= horizon {
                    until += 1;
                }
                clips = track.notes[self.next..until]
                    .par_iter()
                    .enumerate()
                    .flat_map_iter(|(k, note)| prepared.render_note(self.next + k, note))
                    .collect();
                self.next = until;
                self.all_rendered = until == track.notes.len();
            }
            Voices::Audio(file) => {
                let mut until = self.next;
                while until < track.clips.len() && track.clips[until].at * sr as f64 <= horizon {
                    until += 1;
                }
                if until > self.next {
                    match crate::render::render_audio(file, &track.clips[self.next..until], sr) {
                        Ok(read) => clips = read,
                        Err(e) => self.warnings.push(format!("track '{}': {e}", track.name)),
                    }
                }
                self.next = until;
                self.all_rendered = until == track.clips.len();
            }
            Voices::Tb303(player) => {
                clips = vec![player.render_to(to)];
                self.all_rendered = player.finished();
                if self.all_rendered {
                    // What the whole-track render keeps: the silent tail is
                    // not part of the track, so the inserts are not fed it.
                    trimmed_to = Some(player.start_sample() + player.kept());
                }
            }
            Voices::Ready => {}
        }
        for clip in &clips {
            self.add_clip(clip);
        }
        if let Some(content) = trimmed_to {
            self.content = content;
        }

        let Some(start) = self.start else {
            if self.all_rendered {
                self.end = Some(0);
            }
            return;
        };
        if self.chain.is_none() {
            self.chain = Some(TrackChain::new(track, sr));
            self.fed = start;
            self.out_at = start;
        }
        let chain = self.chain.as_mut().expect("a chain");
        // The end of the track's buffer, as `process_track` finds it: past the
        // last sample any clip reaches, plus what a chorus adds.
        let end = self.all_rendered.then(|| (self.content + chain.tail()).max(start));
        let ceiling = end.map_or(to, |end| end.min(to));
        while self.fed < ceiling {
            // Whole blocks while the track goes on, so that the inserts' own
            // silence check sees the blocks it would see over the whole track;
            // then whatever is left, once, at the end.
            let n = if self.fed + BLOCK <= ceiling {
                BLOCK
            } else if end.is_some_and(|end| end == ceiling) {
                ceiling - self.fed
            } else {
                break;
            };
            let at = self.fed - self.dry_at;
            if self.dry_left.len() < at + n {
                self.dry_left.resize(at + n, 0.0);
                self.dry_right.resize(at + n, 0.0);
            }
            let mut left = self.dry_left[at..at + n].to_vec();
            let mut right = self.dry_right[at..at + n].to_vec();
            chain.process(self.fed, &mut left, &mut right);
            self.out_left.append(&mut left);
            self.out_right.append(&mut right);
            self.fed += n;
            // Let go of what has been through the inserts — but not after
            // every block: a drain moves everything still to come along with
            // it, and doing that a thousand times down a stretch cost more
            // than the whole insert chain.
            if self.fed - self.dry_at >= COMPACT {
                let done = self.fed - self.dry_at;
                self.dry_left.drain(..done);
                self.dry_right.drain(..done);
                self.dry_at = self.fed;
            }
        }
        if end.is_some_and(|end| self.fed >= end) {
            self.end = end;
            self.dry_left = Vec::new();
            self.dry_right = Vec::new();
        }
    }

    /// Adds this track's `from .. to` into a bus that starts at `from`, and
    /// says how far the track's own audio reaches so far.
    fn add_into(&self, bus: &mut [Vec<f32>; 2], from: usize, to: usize, amount: f32) -> usize {
        let reach = self.out_at + self.out_left.len();
        if amount == 0.0 {
            return reach;
        }
        let first = from.max(self.out_at);
        let last = to.min(reach);
        for at in first..last {
            let (l, r) = (self.out_left[at - self.out_at], self.out_right[at - self.out_at]);
            bus[0][at - from] += l * amount;
            bus[1][at - from] += r * amount;
        }
        reach
    }

    /// Everything before `to` has been mixed; let go of it.
    fn release(&mut self, to: usize) {
        let drop = to.saturating_sub(self.out_at).min(self.out_left.len());
        self.out_left.drain(..drop);
        self.out_right.drain(..drop);
        self.out_at += drop;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Everything that carries a state across a stretch boundary at once: a
    /// plate reverb and a ping-pong delay fed from two tracks, a master
    /// sidechain keyed off the drums, a track EQ, a track compressor, a
    /// chorus, a gain sweep, humanised timing, and a master with saturation,
    /// a bus compressor, a clipper and a limiter.
    const WORKS: &str = "tempo 124
meter 4/4
section a bars=1-8
section b bars=9-16
instrument lead synth
  osc saw voices=3 spread=14
  drift 8
  lfo pitch rate=5 depth=10
instrument pad synth
  osc square
instrument kit drums
pattern tune
  C4:q D4 E4 F4 | G4:q A4 B4 C5 |
pattern chords
  [C3 E3 G3]:w | [F3 A3 C4]:w |
pattern beat grid=1/8
  kick  X...x...
  hat   x.x.x.x.
  snare ....X...
track lead
  instrument lead
  reverb 0.4
  delay 0.3
  eq lowcut=80 high=+3
  comp threshold=-18 ratio=3
  sweep gain from=-6 to=0 bars=1-8
  humanize time=6ms vel=10
  play tune x8
track pad
  instrument pad
  layer beds
  pan -0.3
  reverb 0.5
  chorus mix=0.5 rate=0.6 depth=6ms
  sidechain drums depth=0.7 attack=4ms release=0.2
  play chords x8
track drums
  instrument kit
  layer drums
  play beat x16
master
  sidechain drums threshold=-24 ratio=6 attack=2ms release=150ms darken=2
  gain 1
  eq low=+2 highcut=17000
  width 1.2
  saturation 0.2
  comp threshold=-10 ratio=2
  clip -3
  limiter ceiling=-1 release=80ms
";

    /// Nothing that carries anything: no reverb, no delay, no master dynamics.
    const DRY: &str = "tempo 120
instrument lead synth
  osc saw voices=2 spread=8
instrument kit drums
pattern tune
  C4:q D4 E4 F4 |
pattern beat grid=1/8
  kick X...x...
  hat  x.x.x.x.
track lead
  instrument lead
  play tune x8
track drums
  instrument kit
  layer drums
  play beat x8
master
  reverb off
  delay off
  comp off
  limiter off
";

    /// A tb303: one voice running the length of the track, with slides,
    /// accents and knob sweeps under it, so every stretch boundary falls in
    /// the middle of a filter's decay and an accent capacitor's charge.
    const ACID: &str = "tempo 128
meter 4/4
instrument acid tb303
  wave saw
  cutoff 0.25
  resonance 0.8
  envmod 0.55
  decay 0.35
  accent 0.8
  drive 0.35
instrument kit drums
pattern line
  A1:s A1 A2! A1 r C2~ D2 A1   A1 G2! A1~ A2 r A1 E2~ G2 |
  A1:s A1 A2! A1 r C2~ D2 A1   A1 C3!~ A2 G2 E2! D2 C2~ A1 |
pattern beat grid=1/16
  kick    X...X...X...X...
  hat     x.x.x.x.x.x.x.x.
track acid
  instrument acid
  gain -4
  reverb 0.08
  delay 0.18
  sidechain drums depth=0.35 release=0.12
  sweep cutoff from=0.1 to=0.35 bars=1-8
  sweep resonance from=0.8 to=0.92 bars=5-12
  play line x8
track drums
  instrument kit
  layer drums
  play beat x16
master
  reverb size=0.5 decay=0.5 damping=0.5
  delay time=3/16 feedback=0.35 tone=2.5k
  limiter ceiling=-1
";

    fn timeline(text: &str) -> Timeline {
        crate::compile(text).expect("the test song compiles").0
    }

    /// Renders the song a stretch at a time, checking as it goes that the
    /// stretches arrive in order and with no gap, and gives back both what was
    /// given out and the finished render.
    fn streamed(timeline: &Timeline, options: &RenderOptions) -> ((Vec<f32>, Vec<f32>), Rendering) {
        let mut stream = Stream::start(timeline, RATE, HashMap::new(), options);
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for part in stream.by_ref() {
            assert_eq!(part.from, left.len(), "a stretch begins where the last one ended");
            assert!(!part.left.is_empty(), "a stretch carries samples");
            left.extend_from_slice(&part.left);
            right.extend_from_slice(&part.right);
        }
        ((left, right), stream.finish())
    }

    fn differs(a: &[f32], b: &[f32]) -> Option<(usize, f32, f32)> {
        (0..a.len().min(b.len())).find(|&i| a[i] != b[i]).map(|i| (i, a[i], b[i]))
    }

    /// The whole point. A streamed render of a song whose reverb, delay,
    /// sidechain, compressors and limiter all carry a state across every
    /// stretch boundary is the ordinary render, to the last bit.
    #[test]
    fn a_streamed_render_is_the_ordinary_renders_own_samples() {
        for song in [WORKS, DRY, ACID] {
            let timeline = timeline(song);
            let whole = crate::render::render_with(&timeline, RATE, HashMap::new(), &RenderOptions::default());
            let (given, streamed) = streamed(&timeline, &RenderOptions::default());
            assert!(whole.mix.left.len() > 8 * RATE as usize, "a song several seconds long");
            assert!(whole.mix.rms_db() > -40.0, "and heard: {:.1} dBFS", whole.mix.rms_db());
            assert_eq!(streamed.mix.left.len(), whole.mix.left.len(), "the same length");
            assert_eq!(differs(&streamed.mix.left, &whole.mix.left), None, "the left channel differs");
            assert_eq!(differs(&streamed.mix.right, &whole.mix.right), None, "the right channel differs");
            assert!((streamed.layers_peak_db - whole.layers_peak_db).abs() < 1e-4);
            // What was given out while it rendered is the render. Only the
            // end moves: the trim takes off the trailing near-silence and
            // fades the 50 ms before it, which is why `finish` says how many
            // frames the render kept.
            let (left, right) = given;
            let kept = whole.mix.left.len();
            let fade = (0.05 * RATE as f32) as usize;
            assert!(left.len() >= kept, "everything kept was given out");
            assert_eq!(differs(&left[..kept - fade], &whole.mix.left), None, "a stretch was not final");
            assert_eq!(differs(&right[..kept - fade], &whole.mix.right), None, "a stretch was not final");
            let faded = (kept - fade..kept).all(|i| whole.mix.left[i].abs() <= left[i].abs() + 1e-9);
            assert!(faded, "the last 50 ms are the fade over what was given out");
            assert!(left[kept..].iter().all(|s| s.abs() < 1e-3), "what the trim cut was near-silence");
        }
    }

    /// Stems: the layers a streamed render keeps are the layers an ordinary
    /// one keeps, and the mix is unchanged by the asking.
    #[test]
    fn the_layers_of_a_streamed_render_are_the_ordinary_renders_layers() {
        let timeline = timeline(WORKS);
        let options = RenderOptions { split: true, cache: None, stems_through_master: false };
        let whole = crate::render::render_with(&timeline, RATE, HashMap::new(), &options);
        let (_, streamed) = streamed(&timeline, &options);
        assert_eq!(differs(&streamed.mix.left, &whole.mix.left), None);
        assert_eq!(
            streamed.layers.iter().map(|l| l.layer.as_str()).collect::<Vec<_>>(),
            whole.layers.iter().map(|l| l.layer.as_str()).collect::<Vec<_>>()
        );
        for (got, want) in streamed.layers.iter().zip(&whole.layers) {
            assert_eq!(got.audio.left.len(), want.audio.left.len(), "layer '{}' is a different length", got.layer);
            assert_eq!(differs(&got.audio.left, &want.audio.left), None, "layer '{}' differs", got.layer);
            assert_eq!(differs(&got.audio.right, &want.audio.right), None, "layer '{}' differs", got.layer);
        }
    }


    /// The layers of a streamed render through the master's gain are the
    /// layers of an ordinary one through it, and both sum to the mix — the
    /// whole song, and a stretch of it, whose lead-in is dropped from the
    /// layers and from the mix alike.
    #[test]
    fn streamed_layers_through_the_master_sum_to_the_mix() {
        let song = timeline(WORKS);
        let part = crate::bars::cut(&song, crate::BarRange::parse("5-8").expect("a range"), RATE).expect("in range");
        for timeline in [&song, &part] {
            let options = RenderOptions { split: true, cache: None, stems_through_master: true };
            let whole = crate::render::render_with(timeline, RATE, HashMap::new(), &options);
            let (_, streamed) = streamed(timeline, &options);
            assert_eq!(streamed.master_curve_key, whole.master_curve_key, "the same curve");
            assert_eq!(differs(&streamed.mix.left, &whole.mix.left), None, "the mix differs");
            for (got, want) in streamed.layers.iter().zip(&whole.layers) {
                assert_eq!(got.audio.left.len(), want.audio.left.len(), "layer '{}' is a different length", got.layer);
                assert_eq!(differs(&got.audio.left, &want.audio.left), None, "layer '{}' differs", got.layer);
                assert_eq!(differs(&got.audio.right, &want.audio.right), None, "layer '{}' differs", got.layer);
            }
            let mut worst = 0.0f32;
            for i in 0..streamed.mix.left.len() {
                let l: f32 = streamed.layers.iter().map(|x| x.audio.left[i]).sum();
                let r: f32 = streamed.layers.iter().map(|x| x.audio.right[i]).sum();
                worst = worst.max((l - streamed.mix.left[i]).abs()).max((r - streamed.mix.right[i]).abs());
            }
            assert!(worst < 1e-5, "the streamed layers differ from the mix by up to {worst}");
        }
    }

    /// A streamed render fills the same cache as an ordinary one and reads the
    /// same cache back, so an editor can stream some renders and not others.
    #[test]
    fn a_streamed_render_and_an_ordinary_one_share_one_cache() {
        let timeline = timeline(WORKS);
        let dir = std::env::temp_dir().join(format!("mat-stream-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let options = RenderOptions { split: true, cache: Some(dir.clone()), stems_through_master: false };
        let plain = RenderOptions { split: true, cache: None, stems_through_master: false };
        let whole = crate::render::render_with(&timeline, RATE, HashMap::new(), &plain);

        // Streamed into an empty cache, then read back by an ordinary render.
        let (_, first) = streamed(&timeline, &options);
        assert!(first.layers.iter().all(|l| !l.cached), "nothing was there to read");
        let second = crate::render::render_with(&timeline, RATE, HashMap::new(), &options);
        assert!(second.layers.iter().all(|l| l.cached), "the streamed render filled it");
        assert_eq!(differs(&second.mix.left, &whole.mix.left), None, "a cached render is an uncached one");

        // And the other way round: an ordinary render's cache, streamed.
        let _ = std::fs::remove_dir_all(&dir);
        crate::render::render_with(&timeline, RATE, HashMap::new(), &options);
        let (_, third) = streamed(&timeline, &options);
        assert!(third.layers.iter().all(|l| l.cached), "the ordinary render filled it");
        assert_eq!(differs(&third.mix.left, &whole.mix.left), None, "read back, it is the same render");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Streaming part of a song: the same samples again, with the lead-in
    /// dropped and faded as `mat render --bars` drops and fades it.
    #[test]
    fn a_stretch_of_a_song_streams_as_it_renders() {
        let song = timeline(WORKS);
        let part = crate::bars::cut(&song, crate::BarRange::parse("5-8").expect("a range"), RATE).expect("in range");
        let whole = crate::render::render_with(&part, RATE, HashMap::new(), &RenderOptions::default());
        let (given, streamed) = streamed(&part, &RenderOptions::default());
        assert_eq!(streamed.mix.left.len(), whole.mix.left.len());
        assert_eq!(differs(&streamed.mix.left, &whole.mix.left), None);
        let to = whole.mix.left.len() - (0.05 * RATE as f32) as usize;
        assert_eq!(differs(&given.0[..to], &whole.mix.left), None);
        assert_eq!(given.0[0], 0.0, "the cut fades in");
    }
}
