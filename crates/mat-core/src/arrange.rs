//! Turns a parsed [`Song`] into a flat timeline of notes in seconds.
//! The timeline is what renderers consume, and it can be exported as JSON
//! for external hosts (e.g. the macOS Audio Unit renderer).

use std::path::Path;

use serde::Serialize;

use crate::diag::{Diagnostic, Span, did_you_mean};
use crate::model::*;

#[derive(Debug, Clone, Serialize)]
pub struct TimedNote {
    pub start: f64,
    pub duration: f64,
    pub pitch: Pitch,
    /// MIDI note number, also for drums (General MIDI mapping).
    pub midi: f32,
    pub velocity: f32,
    pub accent: bool,
    pub slide: bool,
}

/// A resolved parameter sweep in seconds.
#[derive(Debug, Clone, Serialize)]
pub struct Sweep {
    pub param: String,
    pub from: f32,
    pub to: f32,
    pub start: f64,
    pub end: f64,
}

/// A region of an audio file placed on the timeline (all values in seconds).
#[derive(Debug, Clone, Serialize)]
pub struct AudioClip {
    pub at: f64,
    pub source_start: f64,
    /// `None` plays to the end of the file.
    pub length: Option<f64>,
}

/// Resolved sidechain ducking: trigger times of the source track.
#[derive(Debug, Clone, Serialize)]
pub struct Duck {
    pub depth: f32,
    pub attack: f32,
    pub release: f32,
    pub times: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimedSection {
    pub name: String,
    pub from_bar: f64,
    pub to_bar: f64,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineTrack {
    pub name: String,
    pub layer: String,
    pub instrument_name: String,
    pub instrument: InstrumentKind,
    pub gain_db: f32,
    pub pan: f32,
    pub reverb: f32,
    pub delay: f32,
    pub eq: Option<EqSettings>,
    pub comp: Option<CompSettings>,
    pub chorus: Option<ChorusSettings>,
    pub duck: Option<Duck>,
    pub sweeps: Vec<Sweep>,
    pub notes: Vec<TimedNote>,
    pub clips: Vec<AudioClip>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Timeline {
    pub title: Option<String>,
    pub tempo: f64,
    pub meter: (u32, u32),
    /// Time of the last note-off, in seconds.
    pub end: f64,
    /// Delay time in seconds (resolved from the note value).
    pub delay_seconds: f64,
    pub master: Master,
    pub tracks: Vec<TimelineTrack>,
    pub sections: Vec<TimedSection>,
    /// Length of one bar in seconds.
    pub bar_seconds: f64,
}

pub fn arrange(song: &Song) -> Result<Timeline, Vec<Diagnostic>> {
    let mut diags = Vec::new();
    let bar = song.bar_length();
    let mut tracks = Vec::new();
    // Notes of every track (muted ones too), for sidechain triggers.
    let mut all_notes: Vec<(&str, Vec<TimedNote>)> = Vec::new();
    let mut pending_ducks: Vec<(usize, &SidechainSettings)> = Vec::new();

    for track in &song.tracks {
        let instrument = match (&track.instrument, &track.audio) {
            (Some((inst_name, inst_span)), _) => {
                let found = song.instruments.iter().find(|i| &i.name == inst_name);
                if found.is_none() {
                    let mut d = Diagnostic::error(*inst_span, format!("unknown instrument '{inst_name}'"));
                    if let Some(h) = did_you_mean(inst_name, song.instruments.iter().map(|i| i.name.as_str())) {
                        d = d.with_hint(h);
                    }
                    diags.push(d);
                }
                found.map(|i| (i.name.clone(), i.kind.clone()))
            }
            (None, Some((source, _))) => Some(("audio".to_string(), InstrumentKind::Audio(source.clone()))),
            (None, None) => None,
        };

        let mut notes = Vec::new();
        let mut clips = Vec::new();
        let mut cursor: Whole = 0.0;
        for step in &track.steps {
            match step {
                TrackStep::At { bar: b } => cursor = (b - 1.0) * bar,
                TrackStep::Rest { bars } => cursor += bars * bar,
                TrackStep::PlayAudio { bars, repeat } => {
                    let Some((source, _)) = &track.audio else { continue };
                    for _ in 0..*repeat {
                        match bars {
                            None => {
                                clips.push(AudioClip { at: song.seconds(cursor) - source.offset, source_start: 0.0, length: None });
                            }
                            Some((from, to)) => {
                                let count = to - from + 1.0;
                                clips.push(AudioClip {
                                    at: song.seconds(cursor),
                                    source_start: source.offset + song.seconds((from - 1.0) * bar),
                                    length: Some(song.seconds(count * bar)),
                                });
                                cursor += count * bar;
                            }
                        }
                    }
                }
                TrackStep::Play { pattern, span, repeat, transpose, velocity } => {
                    if track.audio.is_some() {
                        diags.push(Diagnostic::error(*span, "audio tracks play: all, or bars=<from>-<to>").with_hint("put the 'audio' line before 'play' lines"));
                        continue;
                    }
                    let Some(pat) = song.patterns.iter().find(|p| &p.name == pattern) else {
                        let mut d = Diagnostic::error(*span, format!("unknown pattern '{pattern}'"));
                        if let Some(h) = did_you_mean(pattern, song.patterns.iter().map(|p| p.name.as_str())) {
                            d = d.with_hint(h);
                        }
                        diags.push(d);
                        continue;
                    };
                    if let Some((inst_name, kind)) = &instrument
                        && let Err(msg) = check_compatible(pat, kind)
                    {
                        diags.push(Diagnostic::error(*span, format!("pattern '{pattern}' {msg} but instrument '{inst_name}' is {}", kind_label(kind))));
                        continue;
                    }
                    let drum_map = match &instrument {
                        Some((_, InstrumentKind::Sampler(def))) => def.drum_map.as_slice(),
                        Some((_, InstrumentKind::Samples(def))) => def.settings.drum_map.as_slice(),
                        _ => &[],
                    };
                    for _ in 0..*repeat {
                        for ev in &pat.events {
                            let midi = match ev.pitch {
                                Pitch::Note(n) => n + transpose,
                                Pitch::Drum(d) => drum_map.iter().find(|(k, _)| *k == d).map_or(d.gm_note(), |(_, n)| *n) as f32,
                            };
                            let pitch = match ev.pitch {
                                Pitch::Note(_) => Pitch::Note(midi),
                                drum => drum,
                            };
                            notes.push(TimedNote {
                                start: song.seconds(cursor + ev.start),
                                duration: song.seconds(ev.duration),
                                pitch,
                                midi,
                                velocity: (ev.velocity * velocity).clamp(0.0, 1.0),
                                accent: ev.accent,
                                slide: ev.slide,
                            });
                        }
                        cursor += pat.length;
                    }
                }
            }
        }
        // Swing: offbeat steps of the grid move late. Humanize: small random
        // timing and velocity changes, deterministic per track.
        let swing = track.swing.unwrap_or(song.swing);
        let grid = song.seconds(track.swing_grid.unwrap_or(song.swing_grid));
        if swing > 0.5 && grid > 0.0 {
            let delay = (2.0 * swing as f64 - 1.0) * grid;
            for n in &mut notes {
                let steps = n.start / grid;
                if (steps - steps.round()).abs() < 1e-4 && (steps.round() as i64) % 2 == 1 {
                    n.start += delay;
                    n.duration = (n.duration - delay).max(grid * 0.5);
                }
            }
        }
        if let Some(h) = track.humanize {
            let mut rng = crate::dsp::Rng::new(0xC0FFEE ^ tracks.len() as u64 ^ (track.name.len() as u64) << 8);
            for n in &mut notes {
                n.start = (n.start + (rng.bipolar() * h.time) as f64).max(0.0);
                n.velocity = (n.velocity + rng.bipolar() * h.velocity).clamp(0.05, 1.0);
            }
        }
        notes.sort_by(|a, b| a.start.total_cmp(&b.start));
        all_notes.push((track.name.as_str(), notes.clone()));

        let Some((instrument_name, kind)) = instrument else { continue };
        for s in &track.sweeps {
            const SYNTH_PARAMS: [&str; 2] = ["cutoff", "res"];
            let (valid, names): (bool, &[&str]) = match &kind {
                _ if s.param == "gain" => (true, &[]),
                InstrumentKind::Tb303(_) => (Tb303Param::from_name(&s.param).is_some(), &Tb303Param::NAMES),
                InstrumentKind::Synth(_) => (SYNTH_PARAMS.contains(&s.param.as_str()), &SYNTH_PARAMS),
                _ => (false, &[]),
            };
            if !valid {
                let d = if names.is_empty() {
                    Diagnostic::error(s.span, "only 'gain' can be swept on this track (synth: cutoff, res; tb303: its knobs)")
                } else {
                    Diagnostic::error(s.span, format!("{} has no sweepable parameter '{}'", kind_label(&kind), s.param))
                        .with_hint(did_you_mean(&s.param, names.iter().copied()).unwrap_or_else(|| format!("sweepable: {}", names.join(", "))))
                };
                diags.push(d);
            }
        }
        if track.mute {
            continue;
        }
        if let Some(sc) = &track.sidechain {
            pending_ducks.push((tracks.len(), sc));
        }
        tracks.push(TimelineTrack {
            name: track.name.clone(),
            layer: track.layer.clone().unwrap_or_else(|| track.name.clone()),
            instrument_name,
            instrument: kind,
            gain_db: track.gain_db,
            pan: track.pan,
            reverb: track.reverb,
            delay: track.delay,
            eq: track.eq.clone(),
            comp: track.comp.clone(),
            chorus: track.chorus.clone(),
            duck: None,
            sweeps: track
                .sweeps
                .iter()
                .map(|s| Sweep { param: s.param.clone(), from: s.from, to: s.to, start: song.seconds((s.from_bar - 1.0) * bar), end: song.seconds(s.to_bar * bar) })
                .collect(),
            notes,
            clips,
        });
    }

    for (index, sc) in pending_ducks {
        let Some((_, source)) = all_notes.iter().find(|(name, _)| *name == sc.source) else {
            let mut d = Diagnostic::error(sc.span, format!("unknown track '{}'", sc.source));
            if let Some(h) = did_you_mean(&sc.source, all_notes.iter().map(|(n, _)| *n)) {
                d = d.with_hint(h);
            }
            diags.push(d);
            continue;
        };
        let has_kick = source.iter().any(|n| n.pitch == Pitch::Drum(DrumKind::Kick));
        let drum = sc.drum.or(has_kick.then_some(DrumKind::Kick));
        let mut times: Vec<f64> = source
            .iter()
            .filter(|n| drum.is_none_or(|d| n.pitch == Pitch::Drum(d)))
            .map(|n| n.start)
            .collect();
        times.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        tracks[index].duck = Some(Duck { depth: sc.depth, attack: sc.attack, release: sc.release, times });
    }

    // Scratch sources: bars → seconds, and the source track must exist.
    for i in 0..tracks.len() {
        if let InstrumentKind::Scratch(def) = &mut tracks[i].instrument
            && let Some(src) = def.source_track.clone()
        {
            let from_bar = def.start;
            def.start = song.seconds((from_bar - 1.0) * bar);
            def.length = def.length.map(|bars| song.seconds(bars * bar));
            if src != "mix" && !song.tracks.iter().any(|t| t.name == src) {
                diags.push(Diagnostic::error(Span::default(), format!("scratch source: no track named '{src}'")));
            }
        }
    }
    if let Some(sc) = &song.master.sidechain
        && !tracks.iter().any(|t| t.name == sc.source || t.layer == sc.source)
    {
        diags.push(Diagnostic::error(Span::default(), format!("master sidechain: no track or layer named '{}'", sc.source)));
    }
    if !diags.is_empty() {
        return Err(diags);
    }
    let end = tracks
        .iter()
        .flat_map(|t| t.notes.iter().map(|n| n.start + n.duration).chain(t.clips.iter().map(|c| c.at + c.length.unwrap_or(0.0))))
        .fold(0.0, f64::max);
    Ok(Timeline {
        title: song.title.clone(),
        tempo: song.tempo,
        meter: song.meter,
        end,
        delay_seconds: song.seconds(song.master.delay.time),
        master: song.master.clone(),
        tracks,
        sections: song
            .sections
            .iter()
            .map(|s| TimedSection { name: s.name.clone(), from_bar: s.from_bar, to_bar: s.to_bar, start: song.seconds((s.from_bar - 1.0) * bar), end: song.seconds(s.to_bar * bar) })
            .collect(),
        bar_seconds: song.seconds(bar),
    })
}

fn check_compatible(pat: &Pattern, kind: &InstrumentKind) -> Result<(), &'static str> {
    let has_drums = pat.events.iter().any(|e| matches!(e.pitch, Pitch::Drum(_)));
    let has_notes = pat.events.iter().any(|e| matches!(e.pitch, Pitch::Note(_)));
    match kind {
        InstrumentKind::Synth(_) | InstrumentKind::Tb303(_) if has_drums => Err("contains drum hits"),
        InstrumentKind::Drums(_) | InstrumentKind::Scratch(_) if has_notes => Err("contains pitched notes"),
        InstrumentKind::Scratch(_) if pat.events.iter().any(|e| matches!(e.pitch, Pitch::Drum(d) if !d.is_scratch())) => Err("contains drum hits (scratch moves are baby, fwd, back, scribble, chirp, transform)"),
        _ => Ok(()),
    }
}

fn kind_label(kind: &InstrumentKind) -> &'static str {
    match kind {
        InstrumentKind::Synth(_) => "a synth",
        InstrumentKind::Drums(_) => "a drum kit",
        InstrumentKind::Sampler(_) => "a sampler",
        InstrumentKind::Samples(_) => "a samples instrument",
        InstrumentKind::Scratch(_) => "a scratch instrument",
        InstrumentKind::AudioUnit(_) => "an Audio Unit",
        InstrumentKind::Audio(_) => "an audio file",
        InstrumentKind::Clap(_) => "a CLAP plugin",
        InstrumentKind::Tb303(_) => "a tb303",
    }
}

/// Resolves an instrument path: relative paths are taken relative to the song
/// file, `logic:` / `garageband:` expand to the installed sampler instrument
/// libraries and `gm` is the General MIDI sound bank of macOS.
pub fn resolve_load_path(load: &str, song_dir: &Path) -> String {
    const LOGIC: &str = "/Library/Application Support/Logic/Sampler Instruments";
    const GARAGEBAND: &str = "/Library/Application Support/GarageBand/Instrument Library/Sampler/Sampler Instruments";
    const GM: &str = "/System/Library/Components/CoreAudio.component/Contents/Resources/gs_instruments.dls";
    const SURGE: &str = if cfg!(target_os = "macos") { "/Library/Application Support/Surge XT" } else { "/usr/share/surge-xt" };
    let assets = std::env::var_os("MAT_ASSETS").map(std::path::PathBuf::from).unwrap_or_else(|| {
        // <repo>/assets/samples, found relative to the binary in the development layout.
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().and_then(Path::parent).and_then(Path::parent).map(|r| r.join("assets/samples")))
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| Path::new("assets/samples").to_path_buf())
    });
    let resolved = if let Some(rest) = load.strip_prefix("samples:") {
        assets.join(rest)
    } else if load == "gm" {
        Path::new(GM).to_path_buf()
    } else if let Some(rest) = load.strip_prefix("logic:") {
        Path::new(LOGIC).join(rest)
    } else if let Some(rest) = load.strip_prefix("surge:") {
        Path::new(SURGE).join("patches_factory").join(rest)
    } else if let Some(rest) = load.strip_prefix("surge-3rdparty:") {
        Path::new(SURGE).join("patches_3rdparty").join(rest)
    } else if let Some(rest) = load.strip_prefix("garageband:") {
        Path::new(GARAGEBAND).join(rest)
    } else {
        song_dir.join(load)
    };
    resolved.to_string_lossy().into_owned()
}

/// Resolves the instrument paths of all tracks, see [`resolve_load_path`].
pub fn resolve_paths(timeline: &mut Timeline, song_dir: &Path) {
    for track in &mut timeline.tracks {
        match &mut track.instrument {
            InstrumentKind::AudioUnit(au) => au.load = au.load.as_deref().map(|l| resolve_load_path(l, song_dir)),
            InstrumentKind::Sampler(s) => s.load = resolve_load_path(&s.load, song_dir),
            InstrumentKind::Samples(s) => {
                for z in &mut s.zones {
                    z.path = resolve_load_path(&z.path, song_dir);
                }
            }
            InstrumentKind::Audio(a) => a.path = resolve_load_path(&a.path, song_dir),
            InstrumentKind::Scratch(s) if s.source_track.is_none() => s.path = resolve_load_path(&s.path, song_dir),
            InstrumentKind::Clap(c) => c.patch = c.patch.as_deref().map(|p| resolve_load_path(p, song_dir)),
            _ => {}
        }
    }
}
