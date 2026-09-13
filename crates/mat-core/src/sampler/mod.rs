//! Sample playback engine for EXS instruments (Logic Pro / GarageBand),
//! including Logic's consolidated sample files.

pub mod audio_file;
pub mod exs;
pub(crate) mod sinc;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rayon::prelude::*;

use crate::arrange::TimedNote;
use crate::dsp::{StereoClip, db_to_gain, pan_gains};
use crate::model::{SampleZone, SamplerDef};
use audio_file::AudioFile;
use exs::{EnableBy, ExsInstrument, Group, SampleRef, Zone};

/// Folders searched for sample files that are not where the instrument says.
fn sample_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/Library/Application Support/Logic/EXS Factory Samples"),
        PathBuf::from("/Library/Application Support/GarageBand/Instrument Library/Sampler/Sampler Files"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(Path::new(&home).join("Music/Audio Music Apps/Sampler Files"));
    }
    roots
}

fn sample_index() -> &'static HashMap<String, PathBuf> {
    static INDEX: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = HashMap::new();
        let mut stack = sample_roots();
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    index.entry(name.to_lowercase()).or_insert(path);
                }
            }
        }
        index
    })
}

pub struct Sampler {
    pub instrument: ExsInstrument,
    files: Vec<Option<AudioFile>>,
    /// Usable groups, ignoring articulation (chosen per note).
    usable_group: Vec<bool>,
    articulations: Vec<u8>,
    pub warnings: Vec<String>,
}

/// The zones one note triggers, plus when an exclusive group cuts it off.
pub struct NotePlan {
    pub zones: Vec<usize>,
    pub choke_at: Option<f64>,
}

type Region = Arc<(Vec<f32>, Vec<f32>)>;

impl Sampler {
    pub fn load(path: &Path) -> Result<Self, String> {
        let instrument = exs::read(path)?;
        let exs_dir = path.parent().unwrap_or(Path::new("."));
        let mut warnings = Vec::new();

        let files = instrument
            .samples
            .iter()
            .map(|s| {
                let candidates = [Path::new(&s.folder).join(&s.file_name), exs_dir.join(&s.file_name)];
                let found = candidates
                    .into_iter()
                    .find(|p| p.is_file())
                    .or_else(|| sample_index().get(&s.file_name.to_lowercase()).cloned());
                match found.map(|p| AudioFile::open(&p)) {
                    Some(Ok(f)) => Some(f),
                    Some(Err(e)) => {
                        warnings.push(e);
                        None
                    }
                    None => {
                        warnings.push(format!("sample file not found: {}", s.file_name));
                        None
                    }
                }
            })
            .collect();

        let groups = &instrument.groups;
        let mut articulations: Vec<u8> = groups.iter().filter(|g| g.enable_by == EnableBy::Articulation).map(|g| g.articulation).collect();
        articulations.sort_unstable();
        articulations.dedup();
        let has_plain = groups.iter().any(|g| matches!(g.enable_by, EnableBy::Always | EnableBy::RoundRobin));
        let usable_group = groups
            .iter()
            .map(|g| {
                !g.mute
                    && !g.release_trigger
                    && match g.enable_by {
                        EnableBy::Control => g.control_low == 0,
                        EnableBy::Note => !has_plain,
                        _ => true,
                    }
            })
            .collect();

        Ok(Self { instrument, files, usable_group, articulations, warnings })
    }

    /// Builds an instrument from regions of audio files (no `.exs` needed).
    pub fn from_zones(zones: &[SampleZone]) -> Result<Self, String> {
        let mut instrument = ExsInstrument::default();
        let mut files: Vec<Option<AudioFile>> = Vec::new();
        let mut paths: Vec<String> = Vec::new();
        for z in zones {
            let index = match paths.iter().position(|p| *p == z.path) {
                Some(i) => i,
                None => {
                    let file = AudioFile::open(Path::new(&z.path))?;
                    instrument.samples.push(SampleRef {
                        name: z.path.clone(),
                        frames: file.frames as u32,
                        sample_rate: file.sample_rate as u32,
                        channels: file.channels as u32,
                        compressed: false,
                        folder: String::new(),
                        file_name: z.path.clone(),
                    });
                    files.push(Some(file));
                    paths.push(z.path.clone());
                    paths.len() - 1
                }
            };
            let rate = files[index].as_ref().unwrap().sample_rate;
            let frames = files[index].as_ref().unwrap().frames as f64;
            let start = (z.start * rate).round().clamp(0.0, frames - 1.0);
            let end = z.length.map_or(frames - 1.0, |l| (start + l * rate).round().min(frames - 1.0));
            let (loop_on, loop_start, loop_end) = match z.loop_range {
                Some((a, b)) => (true, (start + a * rate).round(), (start + b * rate).round().min(end)),
                None => (false, 0.0, 0.0),
            };
            let tune = z.tune;
            instrument.zones.push(Zone {
                name: z.path.clone(),
                root: z.root.round() as u8,
                key_low: z.key_low,
                key_high: z.key_high,
                vel_low: z.vel_low,
                vel_high: z.vel_high,
                pitched: z.drum.is_none(),
                oneshot: z.drum.is_some(),
                reverse: false,
                fine_cents: (tune.fract() * 100.0) as i8,
                coarse: tune.trunc() as i8,
                pan: 0,
                volume_db: z.gain_db.round() as i8,
                start: start as u32,
                end: end as u32,
                loop_on,
                loop_start: loop_start as u32,
                loop_end: loop_end as u32,
                group: 0,
                sample: index as u32,
            });
        }
        instrument.groups.push(Group { vel_high: 127, ..Group::default() });
        Ok(Self { instrument, files, usable_group: vec![true], articulations: Vec::new(), warnings: Vec::new() })
    }

    fn group_ok(&self, group: u32, articulation: Option<u8>) -> bool {
        let Some(g) = self.instrument.groups.get(group as usize) else { return true };
        self.usable_group[group as usize] && (g.enable_by != EnableBy::Articulation || Some(g.articulation) == articulation)
    }

    /// The articulation a note plays: the song's choice, else the first one.
    /// General MIDI open hi-hat (46) picks the most open hi-hat articulation.
    fn articulation_for(&self, note: &TimedNote, def: &SamplerDef) -> Option<u8> {
        if matches!(note.pitch, crate::model::Pitch::Drum(crate::model::DrumKind::OpenHat)) {
            return self.articulations.last().copied();
        }
        def.articulation.filter(|a| self.articulations.contains(a)).or_else(|| self.articulations.first().copied())
    }

    /// Selects zones for every note: key and velocity ranges, round robin,
    /// and exclusive-group choke.
    pub fn plan(&self, notes: &[TimedNote], def: &SamplerDef) -> Vec<NotePlan> {
        let inst = &self.instrument;
        let mut round_robin: HashMap<u8, usize> = HashMap::new();
        let mut plans: Vec<NotePlan> = notes
            .iter()
            .map(|note| {
                let key = note.midi.round().clamp(0.0, 127.0) as u8;
                let vel = (note.velocity * 127.0).round().clamp(1.0, 127.0) as u8;
                let articulation = self.articulation_for(note, def);
                let mut zones: Vec<usize> = inst
                    .zones
                    .iter()
                    .enumerate()
                    .filter(|(_, z)| {
                        let group = inst.groups.get(z.group as usize);
                        key >= z.key_low
                            && key <= z.key_high
                            && vel >= z.vel_low
                            && vel <= z.vel_high
                            && group.is_none_or(|g| vel >= g.vel_low && vel <= g.vel_high)
                            && self.group_ok(z.group, articulation)
                            && self.files.get(z.sample as usize).is_some_and(|f| f.is_some())
                    })
                    .map(|(i, _)| i)
                    .collect();

                let rr_position = |zi: &usize| {
                    let z = &inst.zones[*zi];
                    inst.groups.get(z.group as usize).filter(|g| g.enable_by == EnableBy::RoundRobin).map(|g| g.round_robin_pos)
                };
                let mut positions: Vec<u32> = zones.iter().filter_map(rr_position).collect();
                positions.sort_unstable();
                positions.dedup();
                if !positions.is_empty() {
                    let counter = round_robin.entry(key).or_insert(0);
                    let chosen = positions[*counter % positions.len()];
                    *counter += 1;
                    zones.retain(|zi| rr_position(zi).is_none_or(|p| p == chosen));
                }
                NotePlan { zones, choke_at: None }
            })
            .collect();

        // Exclusive groups: a later note in the same group cuts earlier voices.
        let exclusive_of = |plan: &NotePlan| -> Vec<u8> {
            let mut groups: Vec<u8> = plan
                .zones
                .iter()
                .filter_map(|&zi| inst.groups.get(inst.zones[zi].group as usize))
                .map(|g| g.exclusive)
                .filter(|&e| e != 0)
                .collect();
            groups.dedup();
            groups
        };
        let exclusives: Vec<Vec<u8>> = plans.iter().map(exclusive_of).collect();
        for i in 0..plans.len() {
            if exclusives[i].is_empty() {
                continue;
            }
            plans[i].choke_at = (i + 1..notes.len())
                .find(|&j| notes[j].start > notes[i].start && exclusives[j].iter().any(|e| exclusives[i].contains(e)))
                .map(|j| notes[j].start - notes[i].start);
        }
        plans
    }

    fn speed(&self, zone: usize, note: &TimedNote, def: &SamplerDef, sample_rate: f32) -> (f64, &AudioFile) {
        let z = &self.instrument.zones[zone];
        let file = self.files[z.sample as usize].as_ref().unwrap();
        let mut semis = z.coarse as f64 + (z.fine_cents as f64 + self.instrument.tune_cents as f64) / 100.0 + def.tune as f64;
        if z.pitched {
            semis += note.midi as f64 - z.root as f64;
        }
        (file.sample_rate / sample_rate as f64 * 2f64.powf(semis / 12.0), file)
    }

    /// Renders all notes of a track.
    pub fn render(&self, notes: &[TimedNote], def: &SamplerDef, sample_rate: f32) -> Result<Vec<StereoClip>, String> {
        let plans = self.plan(notes, def);

        // Load each zone region once, as long as its longest use requires.
        let mut needed: HashMap<usize, f64> = HashMap::new();
        for (note, plan) in notes.iter().zip(&plans) {
            for &zi in &plan.zones {
                let (speed, _) = self.speed(zi, note, def, sample_rate);
                let seconds = note.duration + def.release as f64 * 4.0 + 0.05;
                let frames = seconds * sample_rate as f64 * speed;
                let entry = needed.entry(zi).or_insert(0.0);
                *entry = entry.max(frames);
            }
        }
        let regions: Mutex<HashMap<usize, Region>> = Mutex::new(HashMap::new());
        needed.par_iter().try_for_each(|(&zi, &frames)| -> Result<(), String> {
            let region = self.read_region(zi, frames)?;
            regions.lock().unwrap().insert(zi, region);
            Ok(())
        })?;
        let regions = regions.into_inner().unwrap();

        let clips = notes
            .par_iter()
            .zip(plans.par_iter())
            .flat_map_iter(|(note, plan)| {
                plan.zones.iter().map(|&zi| self.render_voice(zi, &regions[&zi], note, def, plan.choke_at, sample_rate)).collect::<Vec<_>>()
            })
            .collect();
        Ok(clips)
    }

    fn zone_bounds(&self, zone: usize) -> (i64, i64) {
        let z = &self.instrument.zones[zone];
        let file = self.files[z.sample as usize].as_ref().unwrap();
        let start = z.start as i64;
        let end = if z.end > z.start { z.end as i64 + 1 } else { file.frames as i64 };
        (start, end.min(file.frames as i64))
    }

    fn read_region(&self, zone: usize, needed_frames: f64) -> Result<Region, String> {
        let z = &self.instrument.zones[zone];
        let file = self.files[z.sample as usize].as_ref().unwrap();
        let (start, end) = self.zone_bounds(zone);
        let pad = (sinc::HALF_WIDTH * 4) as i64;
        let limit = if z.loop_on || z.reverse { end } else { (start + needed_frames.ceil() as i64).min(end) };
        let (l, r) = file.read_stereo(start - pad, limit + pad)?;
        Ok(Arc::new((l, r)))
    }

    fn render_voice(&self, zone: usize, region: &Region, note: &TimedNote, def: &SamplerDef, choke_at: Option<f64>, sample_rate: f32) -> StereoClip {
        let inst = &self.instrument;
        let z = &inst.zones[zone];
        let group = inst.groups.get(z.group as usize);
        let (speed, _) = self.speed(zone, note, def, sample_rate);
        let (start, end) = self.zone_bounds(zone);
        let pad = (sinc::HALF_WIDTH * 4) as f64;
        let (data_l, data_r) = (&region.0, &region.1);
        let region_len = data_l.len() as f64 - pad;
        let zone_len = (end - start) as f64;

        let cutoff = (1.0 / speed).min(1.0) as f32;
        let loop_range = (z.loop_on && !z.oneshot && z.loop_end > z.loop_start)
            .then(|| ((z.loop_start as i64 - start) as f64 + pad, (z.loop_end as i64 - start) as f64 + 1.0 + pad));

        let volume_db = inst.volume_db + z.volume_db as f32 + group.map_or(0.0, |g| g.volume_db as f32) + def.gain_db;
        let velocity_gain = db_to_gain(-(1.0 - note.velocity) * def.velocity_db);
        let pan = ((z.pan as f32 + group.map_or(0.0, |g| g.pan as f32)) / 50.0).clamp(-1.0, 1.0);
        let (pan_l, pan_r) = pan_gains(pan);
        let gain = db_to_gain(volume_db) * velocity_gain;

        let note_off = if z.oneshot { usize::MAX } else { (note.duration * sample_rate as f64) as usize };
        let choke = choke_at.map(|c| (c * sample_rate as f64) as usize);
        let attack = ((def.attack.max(0.0005)) * sample_rate) as usize;
        let release_coef = (-(100f32.ln()) / (def.release.max(0.005) * sample_rate)).exp();
        let choke_fade = (0.008 * sample_rate) as usize;

        let mut pos = if z.reverse { pad + zone_len - 1.0 } else { pad };
        let step = if z.reverse { -speed } else { speed };
        let mut env = 1.0f32;
        let mut left = Vec::new();
        let mut right = Vec::new();
        let mut i = 0usize;
        loop {
            if !z.reverse && (pos >= region_len.min(pad + zone_len)) {
                break;
            }
            if z.reverse && pos < pad {
                break;
            }
            if i >= note_off {
                env *= release_coef;
                if env < 1e-4 {
                    break;
                }
            }
            let mut g = env * gain;
            if i < attack {
                g *= i as f32 / attack as f32;
            }
            if let Some(c) = choke {
                if i >= c + choke_fade {
                    break;
                }
                if i >= c {
                    g *= 1.0 - (i - c) as f32 / choke_fade as f32;
                }
            }
            // Fade out over the last 10 ms of a region that is not looped.
            if loop_range.is_none() {
                let remaining = if z.reverse { pos - pad } else { pad + zone_len - pos };
                let fade = 0.01 * sample_rate as f64 * speed;
                if remaining < fade {
                    g *= (remaining / fade).max(0.0) as f32;
                }
            }
            let l = sinc::interpolate(data_l, pos, cutoff);
            let r = sinc::interpolate(data_r, pos, cutoff);
            left.push(l * g * pan_l);
            right.push(r * g * pan_r);

            pos += step;
            if let Some((ls, le)) = loop_range
                && pos >= le
            {
                pos = ls + (pos - le);
            }
            i += 1;
        }
        StereoClip { offset: (note.start * sample_rate as f64).round() as usize, left, right }
    }
}
