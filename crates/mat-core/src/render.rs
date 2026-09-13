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
use crate::model::{DrumKind, InstrumentKind, Pitch};
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

/// Renders the timeline. `stems` holds pre-rendered dry audio for tracks the
/// built-in engine cannot play (Audio Unit instruments), keyed by track index.
pub fn render(timeline: &Timeline, sample_rate: u32, mut stems: HashMap<usize, StereoClip>) -> (Audio, RenderReport) {
    let sr = sample_rate as f32;
    let mut skipped = Vec::new();

    // Plugins run on the calling thread, one at a time: many expect a single host thread.
    let mut plugin_results: Vec<(usize, Result<Vec<StereoClip>, String>)> = Vec::new();
    for (ti, track) in timeline.tracks.iter().enumerate() {
        if let InstrumentKind::Clap(def) = &track.instrument {
            plugin_results.push((ti, render_clap(def, &track.notes, sr).map(|c| vec![c])));
        }
    }

    let results: Vec<(usize, Result<Vec<StereoClip>, String>)> = timeline
        .tracks
        .par_iter()
        .enumerate()
        .filter_map(|(ti, track)| render_track(track, ti, sr).map(|r| (ti, r)))
        .collect::<Vec<_>>()
        .into_iter()
        .chain(plugin_results)
        .collect();
    let mut warnings = Vec::new();
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

    // Audio tracks are read one at a time while mixing, to keep memory low.
    let mut audio_tracks = Vec::new();
    let mut audio_end = 0usize;
    for (ti, track) in timeline.tracks.iter().enumerate() {
        if let InstrumentKind::Audio(source) = &track.instrument {
            match AudioFile::open(std::path::Path::new(&source.path)) {
                Ok(file) => {
                    let duration = file.frames as f64 / file.sample_rate;
                    for clip in &track.clips {
                        let end = clip.at + clip.length.unwrap_or(duration - clip.source_start).min(duration - clip.source_start);
                        audio_end = audio_end.max((end.max(0.0) * sr as f64) as usize);
                    }
                    audio_tracks.push((ti, file));
                }
                Err(e) => warnings.push(format!("track '{}': {e}", track.name)),
            }
        }
    }

    let master = &timeline.master;
    let content_end = rendered
        .iter()
        .flat_map(|(_, clips)| clips.iter().map(|c| c.offset + c.left.len()))
        .max()
        .unwrap_or(0)
        .max(audio_end);
    let tail_seconds = if master.reverb.enabled { 1.5 + 6.0 * master.reverb.decay as f64 } else { 0.5 }
        + if master.delay.enabled { delay_tail(timeline.delay_seconds, master.delay.feedback) } else { 0.0 };
    let len = content_end + (tail_seconds * sr as f64) as usize;

    let mut mix = [vec![0.0f32; len], vec![0.0f32; len]];
    // With a master sidechain, the key tracks are collected separately.
    let key_source = master.sidechain.as_ref().map(|s| s.source.clone());
    let mut key_mix = [vec![0.0f32; if key_source.is_some() { len } else { 0 }], vec![0.0f32; if key_source.is_some() { len } else { 0 }]];
    let mut reverb_bus = [vec![0.0f32; len], vec![0.0f32; len]];
    let mut delay_bus = [vec![0.0f32; len], vec![0.0f32; len]];

    let mut mix_track = |track: &TimelineTrack, clips: Vec<StereoClip>| {
        if track.silent {
            return;
        }
        let Some(start) = clips.iter().map(|c| c.offset).min() else { return };
        let chorus_tail = if track.chorus.is_some() { (0.05 * sr) as usize } else { 0 };
        let end = (clips.iter().map(|c| c.offset + c.left.len()).max().unwrap_or(start) + chorus_tail).min(len);
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
        let is_key = key_source.as_ref().is_some_and(|k| &track.name == k || &track.layer == k);
        let target = if is_key { &mut key_mix } else { &mut mix };
        for (i, (l, r)) in left.iter().zip(&right).enumerate() {
            let idx = start + i;
            let (l, r) = (l * gl, r * gr);
            target[0][idx] += l;
            target[1][idx] += r;
            reverb_bus[0][idx] += l * track.reverb;
            reverb_bus[1][idx] += r * track.reverb;
            delay_bus[0][idx] += l * track.delay;
            delay_bus[1][idx] += r * track.delay;
        }
    };
    for (ti, clips) in rendered {
        mix_track(&timeline.tracks[ti], clips);
    }
    for (ti, file) in &audio_tracks {
        let track = &timeline.tracks[*ti];
        match render_audio(file, &track.clips, sr) {
            Ok(clips) => mix_track(track, clips),
            Err(e) => warnings.push(format!("track '{}': {e}", track.name)),
        }
    }

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
        crate::dsp::dynamics::keyed_compress(sc, &key_mix[0], &key_mix[1], &mut left, &mut right, sr);
        for ch in 0..2 {
            let dst = if ch == 0 { &mut left } else { &mut right };
            for (d, k) in dst.iter_mut().zip(&key_mix[ch]) {
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
    if master.saturation > 0.0 {
        crate::dsp::dynamics::saturate(master.saturation, &mut left, &mut right);
    }
    if let Some(comp) = &master.comp {
        crate::dsp::dynamics::compress(comp, &mut left, &mut right, sr);
    }
    if master.limiter.enabled {
        limiter::limit(&mut left, &mut right, master.limiter.ceiling_db, master.limiter.release_ms, sr);
    }
    trim_tail(&mut left, &mut right, sr);

    (Audio { sample_rate, left, right }, RenderReport { skipped_tracks: skipped, warnings })
}

fn render_track(track: &TimelineTrack, track_index: usize, sr: f32) -> Option<Result<Vec<StereoClip>, String>> {
    let seed_base = (track_index as u64 + 1) << 32;
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
                    synth::render_note_from(def, note, from, &track.sweeps, sr, seed_base + i as u64)
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
                    drums::render_hit(note, kit.voice(kind), choke, sr, seed_base + i as u64)
                })
                .collect(),
        )),
        InstrumentKind::Tb303(def) => Some(Ok(vec![tb303::render(def, &track.notes, &track.sweeps, sr)])),
        InstrumentKind::AudioUnit(_) | InstrumentKind::Audio(_) | InstrumentKind::Clap(_) => None,
    }
}

fn render_clap(def: &crate::model::ClapDef, notes: &[crate::arrange::TimedNote], sr: f32) -> Result<StereoClip, String> {
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
    let mut clip = instance.render(notes, &params, end + 4.0, sr)?;
    let gain = db_to_gain(def.gain_db);
    clip.left.iter_mut().chain(clip.right.iter_mut()).for_each(|s| *s *= gain);
    Ok(clip)
}

/// Reads the clips of an audio track, resampling if the file rate differs.
fn render_audio(file: &AudioFile, clips: &[AudioClip], sr: f32) -> Result<Vec<StereoClip>, String> {
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

/// Removes trailing near-silence and applies a short fade-out.
fn trim_tail(left: &mut Vec<f32>, right: &mut Vec<f32>, sr: f32) {
    let threshold = 10f32.powf(-70.0 / 20.0);
    let last = (0..left.len()).rev().find(|&i| left[i].abs() > threshold || right[i].abs() > threshold);
    let end = last.map_or(0, |i| (i + (0.1 * sr) as usize).min(left.len()));
    left.truncate(end);
    right.truncate(end);
    let fade = ((0.05 * sr) as usize).min(end);
    for k in 0..fade {
        let g = k as f32 / fade as f32;
        left[end - 1 - k] *= g;
        right[end - 1 - k] *= g;
    }
}
