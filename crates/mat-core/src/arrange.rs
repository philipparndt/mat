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
    /// Where this note's randomness starts: drift, unison spread, LFO phase,
    /// a drum's noise. A function of the track's seed and of the note itself —
    /// its time, its pitch, and which of several identical ones it is — so a
    /// note sounds the same however the rest of the song is edited. It used to
    /// be the track's position in the file and the note's in the track, and
    /// inserting either changed every take after it.
    #[serde(default)]
    pub seed: u64,
    /// Which of its track's [`TimelineRegion`]s this note was placed by, for an
    /// editor that draws notes inside the regions they came from. Not part of
    /// what a note sounds like, so a layer's cache key leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<usize>,
}

/// One `play` of a track: a pattern, or a stretch of the track's audio, put on
/// the timeline — what a DAW draws as a region. One per `play` line, however
/// many times it repeats, so `play brass x4` is one region of four passes.
///
/// Where it was written is kept so an editor can go from the drawing to the
/// source; nothing here changes the sound, and a layer's cache key leaves it
/// out, so moving a line does not render the layer again.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineRegion {
    /// `pattern` or `audio`.
    pub kind: &'static str,
    /// The pattern's name; `audio` for a stretch of the track's audio.
    pub name: String,
    /// Seconds.
    pub start: f64,
    pub end: f64,
    /// The length of one pass, in seconds.
    pub pass: f64,
    pub repeat: u32,
    /// Semitones.
    pub transpose: f32,
    /// The file the `play` line is in, as the song's own sources name it, and
    /// its line, 1-based.
    pub file: String,
    pub line: usize,
    /// Where the pattern is defined; absent for audio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_line: Option<usize>,
    /// The `play` line says `mute`: the region is where it would be, and
    /// nothing in it sounds.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
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
    /// Rendered (e.g. as a scratch source) but not mixed; used by stem renders.
    #[serde(default)]
    pub silent: bool,
    /// The track's take: its name, and the `seed` it or the song says. Every
    /// note's seed starts from it.
    #[serde(default)]
    pub seed: u64,
    pub instrument_name: String,
    pub instrument: InstrumentKind,
    pub gain_db: f32,
    pub pan: f32,
    pub reverb: f32,
    pub delay: f32,
    pub eq: Option<EqSettings>,
    pub comp: Option<CompSettings>,
    pub chorus: Option<ChorusSettings>,
    pub phaser: Option<PhaserSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distortion: Option<DistortionSettings>,
    pub duck: Option<Duck>,
    pub sweeps: Vec<Sweep>,
    pub notes: Vec<TimedNote>,
    pub clips: Vec<AudioClip>,
    /// Each `play`, in the order they were written. See [`TimelineRegion`].
    #[serde(default)]
    pub regions: Vec<TimelineRegion>,
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
    /// Set when this is only part of a song — `mat render --bars 33-40`. It
    /// says which bars, and how much of the front of the render is the lead-in
    /// that is dropped again. See [`crate::bars`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<crate::bars::Window>,
}

/// A source file's name as an editor can find it: relative to the song's own
/// directory when it is under it, as read otherwise, and empty for a song
/// parsed from text alone.
fn source_name(song: &Song, file: usize) -> String {
    let Some(path) = song.sources.get(file) else { return String::new() };
    let base = song.sources.first().and_then(|song| song.parent());
    base.and_then(|base| path.strip_prefix(base).ok()).unwrap_or(path).to_string_lossy().into_owned()
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
        let mut regions = Vec::new();
        let mut cursor: Whole = 0.0;
        for step in &track.steps {
            match step {
                TrackStep::At { bar: b } => cursor = (b - 1.0) * bar,
                TrackStep::Rest { bars } => cursor += bars * bar,
                TrackStep::PlayAudio { bars, repeat, line, muted } => {
                    let Some((source, _)) = &track.audio else { continue };
                    // The whole file is as long as the file, which is not
                    // known until it is read: a region of no length there.
                    let pass = match bars {
                        Some((from, to)) => song.seconds((to - from + 1.0) * bar),
                        None => 0.0,
                    };
                    let start = song.seconds(cursor);
                    regions.push(TimelineRegion {
                        kind: "audio",
                        name: "audio".to_string(),
                        start,
                        end: start + pass * *repeat as f64,
                        pass,
                        repeat: *repeat,
                        transpose: 0.0,
                        file: source_name(song, track.span.file),
                        line: *line,
                        pattern_file: None,
                        pattern_line: None,
                        muted: *muted,
                    });
                    for _ in 0..*repeat {
                        match bars {
                            // Muted, the whole file takes no time, as it takes
                            // none played; a stretch of bars takes its bars.
                            None if *muted => {}
                            Some((from, to)) if *muted => cursor += (to - from + 1.0) * bar,
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
                TrackStep::Play { pattern, span, repeat, transpose, velocity, muted } => {
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
                    let start = song.seconds(cursor);
                    let pass = song.seconds(pat.length);
                    let region = regions.len();
                    regions.push(TimelineRegion {
                        kind: "pattern",
                        name: pattern.clone(),
                        start,
                        end: start + pass * *repeat as f64,
                        pass,
                        repeat: *repeat,
                        transpose: *transpose,
                        file: source_name(song, span.file),
                        line: span.line,
                        pattern_file: Some(source_name(song, pat.span.file)),
                        pattern_line: Some(pat.span.line),
                        muted: *muted,
                    });
                    // Muted: the pattern is still looked up and checked, so a
                    // block switched off does not hide a mistake until it is
                    // switched on again. It plays nothing — no notes, so no
                    // sidechain trigger either, unlike a muted track.
                    if *muted {
                        cursor += pat.length * *repeat as f64;
                        continue;
                    }
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
                                seed: 0,
                                region: Some(region),
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
        let track_seed = crate::hash::track_seed(&track.name, track.seed.unwrap_or(song.seed));
        if let Some(h) = track.humanize {
            // Per note, from the note: humanize used to be one generator run down
            // the track, so a note added at bar 2 moved every note after it.
            let mut identities = crate::hash::NoteIdentities::new(track_seed ^ crate::hash::HUMANIZE);
            for n in &mut notes {
                let mut rng = crate::dsp::Rng::new(identities.next(n.start, n.midi));
                n.start = (n.start + (rng.bipolar() * h.time) as f64).max(0.0);
                n.velocity = (n.velocity + rng.bipolar() * h.velocity).clamp(0.05, 1.0);
            }
        }
        notes.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut identities = crate::hash::NoteIdentities::new(track_seed);
        for n in &mut notes {
            n.seed = identities.next(n.start, n.midi);
        }
        all_notes.push((track.name.as_str(), notes.clone()));

        let Some((instrument_name, kind)) = instrument else { continue };
        for s in &track.sweeps {
            const SYNTH_PARAMS: [&str; 4] = ["cutoff", "res", "decay", "release"];
            let (valid, names): (bool, &[&str]) = match &kind {
                _ if s.param == "gain" => (true, &[]),
                InstrumentKind::Tb303(_) => (Tb303Param::from_name(&s.param).is_some(), &Tb303Param::NAMES),
                InstrumentKind::Synth(_) => (SYNTH_PARAMS.contains(&s.param.as_str()), &SYNTH_PARAMS),
                _ => (false, &[]),
            };
            if !valid {
                let d = if names.is_empty() {
                    Diagnostic::error(s.span, "only 'gain' can be swept on this track (synth: cutoff, res, decay, release; tb303: its knobs)")
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
            silent: false,
            seed: track_seed,
            instrument_name,
            instrument: kind,
            gain_db: track.gain_db,
            pan: track.pan,
            reverb: track.reverb,
            delay: track.delay,
            eq: track.eq.clone(),
            comp: track.comp.clone(),
            chorus: track.chorus.clone(),
            phaser: track.phaser.clone(),
            distortion: track.distortion.clone(),
            duck: None,
            sweeps: track
                .sweeps
                .iter()
                .map(|s| Sweep { param: s.param.clone(), from: s.from, to: s.to, start: song.seconds((s.from_bar - 1.0) * bar), end: song.seconds(s.to_bar * bar) })
                .collect(),
            notes,
            clips,
            regions,
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
        window: None,
    })
}

fn check_compatible(pat: &Pattern, kind: &InstrumentKind) -> Result<(), &'static str> {
    let has_drums = pat.events.iter().any(|e| matches!(e.pitch, Pitch::Drum(_)));
    let has_notes = pat.events.iter().any(|e| matches!(e.pitch, Pitch::Note(_)));
    match kind {
        InstrumentKind::Synth(_) | InstrumentKind::Fm(_) | InstrumentKind::Tb303(_) if has_drums => Err("contains drum hits"),
        InstrumentKind::Drums(_) | InstrumentKind::Scratch(_) if has_notes => Err("contains pitched notes"),
        InstrumentKind::Scratch(_) if pat.events.iter().any(|e| matches!(e.pitch, Pitch::Drum(d) if !d.is_scratch())) => Err("contains drum hits (scratch moves are baby, fwd, back, scribble, chirp, transform)"),
        _ => Ok(()),
    }
}

fn kind_label(kind: &InstrumentKind) -> &'static str {
    match kind {
        InstrumentKind::Synth(_) => "a synth",
        InstrumentKind::Fm(_) => "an fm synth",
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

/// Where `samples:` points when `MAT_ASSETS` does not say.
///
/// The library is the checkout's `assets/samples`, and a `mat` can be run from
/// three places: the checkout's own `target/<profile>`, where it sits three
/// directories under the checkout; an install (`cargo install --path
/// crates/mat-cli`, or a copy into `~/.cargo/bin`), which is nowhere near it;
/// and anywhere else. The first is found beside the binary, the second through
/// the checkout it was built from, which the build knows — and only then the
/// working directory, which is the song's folder when an editor runs it.
///
/// Reported 2026-09-16 as "Mix view loses the drums" in an editor: an installed
/// `mat` found no library, warned `cannot open assets/samples/...` for every
/// voice of the kit, and wrote a render whose drums peaked at -23 dBFS instead
/// of -0.3 — a successful render, as far as anyone watching the exit code knew.
///
/// A release (the Homebrew formula) is a fourth place: `bin/mat` with the
/// library in `share/mat/samples` under the same prefix. That is looked for
/// through the binary's real path, since `/opt/homebrew/bin/mat` is a link
/// into the Cellar, and before the checkout it was built from — which exists
/// on the machine that cut the release, and is the development copy there.
fn sample_library() -> std::path::PathBuf {
    let exe = std::env::current_exe().ok();
    let beside_binary = exe.as_ref().and_then(|e| e.parent().and_then(Path::parent).and_then(Path::parent).map(|r| r.join("assets/samples")));
    let installed = exe.and_then(|e| e.canonicalize().ok()).and_then(|e| e.parent().and_then(Path::parent).map(|p| p.join("share/mat/samples")));
    let built_from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/samples");
    let found = beside_binary
        .into_iter()
        .chain(installed)
        .chain(std::iter::once(built_from))
        .find(|p| p.is_dir())
        .unwrap_or_else(|| Path::new("assets/samples").to_path_buf());
    // One library, one spelling: the checkout's path is written with `..` in
    // it, and a sample's path is part of its layer's cache key. A library that
    // is not there keeps the path it was looked for at, so it is still warned
    // about by that name.
    found.canonicalize().unwrap_or(found)
}

/// Resolves an instrument path: relative paths are taken relative to the song
/// file, `logic:` / `garageband:` expand to the installed sampler instrument
/// libraries and `gm` is the General MIDI sound bank of macOS.
pub fn resolve_load_path(load: &str, song_dir: &Path) -> String {
    const LOGIC: &str = "/Library/Application Support/Logic/Sampler Instruments";
    const GARAGEBAND: &str = "/Library/Application Support/GarageBand/Instrument Library/Sampler/Sampler Instruments";
    const GM: &str = "/System/Library/Components/CoreAudio.component/Contents/Resources/gs_instruments.dls";
    const SURGE: &str = if cfg!(target_os = "macos") { "/Library/Application Support/Surge XT" } else { "/usr/share/surge-xt" };
    let assets = std::env::var_os("MAT_ASSETS").map(std::path::PathBuf::from).unwrap_or_else(sample_library);
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

#[cfg(test)]
mod tests {
    /// A test binary sits in `target/<profile>/deps`, so three directories up
    /// is `target`, not the checkout: it is placed like an installed `mat`, and
    /// the library can only be found through the checkout it was built from.
    /// `MAT_ASSETS` is process-wide and is not changed here; the resolved path
    /// is only checked when nobody has set it.
    #[test]
    fn a_binary_away_from_the_checkout_finds_the_checkouts_samples() {
        let checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        let exe = std::env::current_exe().unwrap();
        let beside = exe.parent().and_then(std::path::Path::parent).and_then(std::path::Path::parent).unwrap().join("assets/samples");
        assert!(!beside.is_dir(), "{} exists, so this test would not reach the built-from checkout", beside.display());

        let library = super::sample_library();
        assert_eq!(library, checkout.join("assets/samples"));
        assert!(!library.components().any(|c| c == std::path::Component::ParentDir), "{} is not written plainly", library.display());

        if std::env::var_os("MAT_ASSETS").is_none() {
            let resolved = super::resolve_load_path("samples:sonic-pi/bd_tek.wav", std::path::Path::new("/nonexistent/song"));
            assert!(std::path::Path::new(&resolved).is_file(), "{resolved} is not the checkout's sample");
        }
    }

    /// Every `play` is one region however often it repeats, where the cursor
    /// put it, naming the line it is on and the pattern's; every note of it
    /// says so and lies inside it.
    #[test]
    fn each_play_is_one_region_holding_its_notes() {
        let song = "tempo 120
instrument b synth
pattern p
  C4 D4 E4 F4 |
pattern q
  G4:h A4:h |
track one
  instrument b
  at 3
  play p x4
  rest 1
  play q transpose=12
";
        let timeline = crate::compile(song).expect("the song compiles").0;
        let track = &timeline.tracks[0];
        let regions: Vec<_> = track.regions.iter().map(|r| (r.name.as_str(), r.start, r.end, r.repeat, r.line, r.pattern_line)).collect();
        // A bar is two seconds at 120: bar 3 is 4 s, four bars on is 12 s, a
        // bar's rest, and q from 14 s.
        assert_eq!(regions, vec![("p", 4.0, 12.0, 4, 10, Some(3)), ("q", 14.0, 16.0, 1, 12, Some(5))]);
        assert_eq!(track.regions[1].transpose, 12.0);
        assert_eq!(track.notes.len(), 18);
        for note in &track.notes {
            let region = &track.regions[note.region.expect("a pattern's note has a region")];
            assert!(note.start >= region.start && note.start < region.end, "{note:?} is outside {region:?}");
        }
    }

    /// A muted `play` is a region where it would have been, with nothing in
    /// it, and what follows it is where it was: taking the line out instead
    /// would have moved `q` four bars earlier.
    #[test]
    fn a_muted_play_keeps_its_place_and_plays_nothing() {
        let song = |mute: &str| {
            format!(
                "tempo 120
instrument b synth
pattern p
  C4 D4 E4 F4 |
pattern q
  G4:h A4:h |
track one
  instrument b
  play p x4{mute}
  play q
"
            )
        };
        let heard = crate::compile(&song("")).expect("the song compiles").0;
        let muted = crate::compile(&song(" mute")).expect("the song compiles").0;
        let regions = |t: &crate::arrange::Timeline| t.tracks[0].regions.iter().map(|r| (r.name.clone(), r.start, r.end, r.muted)).collect::<Vec<_>>();
        assert_eq!(regions(&heard), vec![("p".to_string(), 0.0, 8.0, false), ("q".to_string(), 8.0, 10.0, false)]);
        assert_eq!(regions(&muted), vec![("p".to_string(), 0.0, 8.0, true), ("q".to_string(), 8.0, 10.0, false)]);
        assert_eq!(heard.tracks[0].notes.len(), 18);
        let notes = &muted.tracks[0].notes;
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|n| n.region == Some(1) && n.start >= 8.0));
        // Only a muted region says so: every other export is what it was.
        let written = serde_json::to_string(&muted.tracks[0].regions).expect("regions serialize");
        assert_eq!(written.matches("\"muted\":true").count(), 1);
        assert!(!written.contains("\"muted\":false"));
    }

    /// A block switched off is still checked: a pattern that is not there is
    /// an error now, not when the block is switched on again.
    #[test]
    fn a_muted_play_of_an_unknown_pattern_is_still_an_error() {
        let song = "tempo 120
instrument b synth
pattern p
  C4 |
track one
  instrument b
  play nothing mute
";
        assert!(crate::compile(song).is_err());
    }

    /// Loops are written out by the parser, so a song with them is the song
    /// without them to everything after it: the same timeline, the same samples.
    #[test]
    fn a_song_with_loops_arranges_and_renders_as_it_does_written_out() {
        let looped = "tempo 140
swing 0.56
instrument acid tb303
instrument lead synth
  osc saw voices=2 spread=10
  drift 6
instrument kit drums
pattern bass
  (A1:s A1 A2! C2~)x2 ((E1:s G1)x4 |)x1
pattern lead
  (C5:e [E5 G5])x2 (D5:q)x2 |
pattern beat grid=1/16
  kick (X...x...)x2
  hat  (x.)x8
track acid
  instrument acid
  repeat 2 {
    play bass
  }
track lead
  instrument lead
  humanize time=5ms vel=8
  rest 1
  repeat 2 {
    repeat 2 {
      play lead transpose=2
    }
    rest 1
  }
track drums
  instrument kit
  repeat 3 {
    play beat
  }
";
        let written = "tempo 140
swing 0.56
instrument acid tb303
instrument lead synth
  osc saw voices=2 spread=10
  drift 6
instrument kit drums
pattern bass
  A1:s A1 A2! C2~ A1:s A1 A2! C2~ E1:s G1 E1:s G1 E1:s G1 E1:s G1 |
pattern lead
  C5:e [E5 G5] C5:e [E5 G5] D5:q D5:q |
pattern beat grid=1/16
  kick X...x...X...x...
  hat  x.x.x.x.x.x.x.x.
track acid
  instrument acid
  play bass
  play bass
track lead
  instrument lead
  humanize time=5ms vel=8
  rest 1
  play lead transpose=2
  play lead transpose=2
  rest 1
  play lead transpose=2
  play lead transpose=2
  rest 1
track drums
  instrument kit
  play beat
  play beat
  play beat
";
        let arranged = |text: &str| crate::compile(text).expect("the song compiles").0;
        let (a, b) = (arranged(looped), arranged(written));
        // The same timeline, but for where each `play` is written: one loop
        // line is several written ones.
        let placed = |t: &super::Timeline| {
            let mut value = serde_json::to_value(t).unwrap();
            for track in value["tracks"].as_array_mut().unwrap() {
                crate::render_cache::without_placements(track);
            }
            value
        };
        assert_eq!(placed(&a), placed(&b), "the same timeline");
        let (left, _) = crate::render(&a, 22_050, Default::default());
        let (right, _) = crate::render(&b, 22_050, Default::default());
        assert!(left.left.len() > 22_050 && left.rms_db() > -40.0, "a song several bars long, and heard");
        assert!(left.left == right.left && left.right == right.right, "the same samples");
    }
}
