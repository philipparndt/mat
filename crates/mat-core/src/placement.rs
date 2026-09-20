//! Where each line of a song is heard: for an editor to draw beside the line.
//!
//! A `play` step sounds from where the track's cursor is to the end of its
//! repeats; a line of a pattern sounds wherever the pattern is played, from
//! its first note to the end of its last; a track's header sounds wherever
//! any of its steps do, an instrument's wherever a track playing it does, and
//! a section is its bars.
//!
//! **Loops are written out** by the parser, so a note made by a repeat group
//! is one of a line's notes once per repetition, each at the columns of the
//! token it came from, and a step inside a `repeat` block is a play once per
//! pass, at its own line. The `repeat` line itself is heard from its first
//! pass to the end of its last (`Track::repeats` says where they are).
//!
//! **Kept apart from `arrange`, on purpose.** The timeline `arrange` builds is
//! what the render cache hashes, and a source line in it would make adding a
//! comment above a track invalidate every cached layer. So the lines live
//! here, in a pass that walks the same steps with the same arithmetic and
//! feeds nothing that makes sound. Swing and humanize, which move notes by
//! milliseconds, are left out.
//!
//! **Lines are in files.** A song that includes other files has lines in each
//! of them, so every line here says its file: the index a `Span::file` names,
//! into `Song::sources`. A pattern's lines, notes and passes are in the file
//! its header is in.

use crate::model::{Song, TrackStep};

/// One line and the stretches of the song, in seconds, where it is heard.
#[derive(Debug, Clone, PartialEq)]
pub struct LineSpans {
    /// The file the line is in, an index into `Song::sources`.
    pub file: usize,
    /// 1-based, as the parser counts.
    pub line: usize,
    pub spans: Vec<(f64, f64)>,
    /// For a line of a pattern: where each pass of the pattern starts, sorted,
    /// in seconds. Empty for every other line.
    pub passes: Vec<f64>,
    /// For a line of a pattern: its notes, the same at every pass since a
    /// song has one tempo — for an editor to light the note under the
    /// playhead. Sorted by start; a chord's notes, sharing a token, are one.
    pub notes: Vec<NotePlace>,
}

/// A note of a pattern's line, in seconds from the start of a pass, and the
/// characters it is written in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NotePlace {
    pub start: f64,
    pub end: f64,
    /// 1-based character, as the parser's spans count.
    pub col: usize,
    pub len: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placements {
    /// The end of the last thing heard.
    pub seconds: f64,
    pub bar_seconds: f64,
    /// In file and line order, and only lines that are heard somewhere.
    pub lines: Vec<LineSpans>,
    /// Every track that is heard, in the song's order, with what it plays
    /// when: for an editor that shows a playing song as threads of a program,
    /// a track a thread and its plays the frames it passes through.
    pub tracks: Vec<TrackPlace>,
}

/// A track that is heard, and each of its steps that sounds.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackPlace {
    pub name: String,
    /// The file the header is in, and its line, 1-based.
    pub file: usize,
    pub line: usize,
    /// The stem it renders into: `layer`, or the track's own name.
    pub layer: String,
    pub instrument: Option<String>,
    /// The file and line, 1-based, of the instrument's header, when it is the song's.
    pub instrument_file: Option<usize>,
    pub instrument_line: Option<usize>,
    /// In the order they are heard.
    pub plays: Vec<PlayPlace>,
}

/// A `play` step of a track: where it is written, what it plays, and when.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayPlace {
    /// The file the step is in — its track's — and its line, 1-based.
    pub file: usize,
    pub line: usize,
    /// The pattern, or nil for a step of an audio track.
    pub pattern: Option<String>,
    /// The file and line, 1-based, of the pattern's header.
    pub pattern_file: Option<usize>,
    pub pattern_line: Option<usize>,
    pub start: f64,
    pub end: f64,
    /// How long one pass is: the pattern's length, or the audio stretch's.
    pub pass_seconds: f64,
    /// Semitones.
    pub transpose: f32,
}

pub fn placements(song: &Song) -> Placements {
    let bar = song.bar_length();
    // Keyed by (file, line).
    let mut by_line: std::collections::BTreeMap<(usize, usize), Vec<(f64, f64)>> = Default::default();
    let mut passes: std::collections::BTreeMap<(usize, usize), Vec<f64>> = Default::default();
    let mut tracks: Vec<TrackPlace> = Vec::new();
    let mut notes: std::collections::BTreeMap<(usize, usize), Vec<NotePlace>> = Default::default();
    let mut add = |line: (usize, usize), start: f64, end: f64| {
        if end > start {
            by_line.entry(line).or_default().push((start, end));
        }
    };

    for track in &song.tracks {
        if track.mute {
            continue;
        }
        let file = track.span.file;
        let mut cursor = 0.0;
        let mut heard: Vec<(f64, f64)> = Vec::new();
        let mut plays: Vec<PlayPlace> = Vec::new();
        // Where each step starts, and whether it sounds: for the `repeat` blocks.
        let mut starts: Vec<f64> = Vec::with_capacity(track.steps.len() + 1);
        let mut sounds: Vec<bool> = vec![false; track.steps.len()];
        for (index, step) in track.steps.iter().enumerate() {
            starts.push(cursor);
            match step {
                TrackStep::At { bar: b } => cursor = (b - 1.0) * bar,
                TrackStep::Rest { bars } => cursor += bars * bar,
                // A muted play is heard nowhere: it takes its time and marks nothing.
                TrackStep::PlayAudio { bars, repeat, muted: true, .. } => {
                    if let Some((from, to)) = bars {
                        cursor += (to - from + 1.0) * bar * *repeat as f64;
                    }
                }
                TrackStep::Play { pattern, repeat, muted: true, .. } => {
                    let Some(pat) = song.patterns.iter().find(|p| &p.name == pattern) else { continue };
                    cursor += pat.length * *repeat as f64;
                }
                TrackStep::PlayAudio { bars, repeat, line, .. } => {
                    let Some((source, _)) = &track.audio else { continue };
                    match bars {
                        // The whole file, bar 1 aligned: its length is the file's,
                        // which is not read here, so the step is marked for a bar.
                        None => {
                            let start = song.seconds(cursor) - source.offset;
                            add((file, *line), start.max(0.0), start.max(0.0) + song.seconds(bar));
                            sounds[index] = true;
                        }
                        Some((from, to)) => {
                            let length = (to - from + 1.0) * bar * *repeat as f64;
                            let span = (song.seconds(cursor), song.seconds(cursor + length));
                            add((file, *line), span.0, span.1);
                            heard.push(span);
                            sounds[index] = true;
                            plays.push(PlayPlace {
                                file,
                                line: *line,
                                pattern: None,
                                pattern_file: None,
                                pattern_line: None,
                                start: span.0,
                                end: span.1,
                                pass_seconds: song.seconds((to - from + 1.0) * bar),
                                transpose: 0.0,
                            });
                            cursor += length;
                        }
                    }
                }
                TrackStep::Play { pattern, span, repeat, transpose, .. } => {
                    let Some(pat) = song.patterns.iter().find(|p| &p.name == pattern) else { continue };
                    let length = pat.length * *repeat as f64;
                    let whole = (song.seconds(cursor), song.seconds(cursor + length));
                    add((span.file, span.line), whole.0, whole.1);
                    heard.push(whole);
                    sounds[index] = true;
                    let pattern_file = pat.span.file;
                    plays.push(PlayPlace {
                        file: span.file,
                        line: span.line,
                        pattern: Some(pattern.clone()),
                        pattern_file: Some(pattern_file),
                        pattern_line: Some(pat.span.line),
                        start: whole.0,
                        end: whole.1,
                        pass_seconds: song.seconds(pat.length),
                        transpose: *transpose,
                    });
                    // Each line of the pattern, at each repeat: its first note to
                    // the end of its last.
                    let mut lines: std::collections::BTreeMap<(usize, usize), (f64, f64)> = Default::default();
                    for event in &pat.events {
                        let entry = lines.entry((pattern_file, event.line)).or_insert((f64::MAX, f64::MIN));
                        entry.0 = entry.0.min(event.start);
                        entry.1 = entry.1.max(event.start + event.duration);
                    }
                    for event in &pat.events {
                        let placed = notes.entry((pattern_file, event.line)).or_default();
                        let note = NotePlace {
                            start: song.seconds(event.start),
                            end: song.seconds(event.start + event.duration),
                            col: event.col,
                            len: event.len,
                        };
                        if !placed.iter().any(|n| n.col == note.col && (n.start - note.start).abs() < 1e-9) {
                            placed.push(note);
                        }
                    }
                    for r in 0..*repeat {
                        let offset = cursor + pat.length * r as f64;
                        for (line, (first, last)) in &lines {
                            add(*line, song.seconds(offset + first), song.seconds(offset + last));
                            passes.entry(*line).or_default().push(song.seconds(offset));
                        }
                        add((pattern_file, pat.span.line), song.seconds(offset), song.seconds(offset + pat.length));
                    }
                    cursor += length;
                }
            }
        }
        starts.push(cursor);
        // A `repeat` line is heard from its first pass to the end of its last,
        // when anything in it sounds; its steps are placed at each pass above.
        for block in &track.repeats {
            if sounds[block.first..block.end].iter().any(|s| *s) {
                add((block.span.file, block.span.line), song.seconds(starts[block.first]), song.seconds(starts[block.end]));
            }
        }
        let instrument = track.instrument.as_ref().map(|(name, _)| name.clone());
        let instrument_at = instrument
            .as_ref()
            .and_then(|name| song.instruments.iter().find(|i| &i.name == name))
            .map(|i| (i.span.file, i.span.line));
        for (start, end) in &heard {
            add((file, track.span.line), *start, *end);
            if let Some(at) = instrument_at {
                add(at, *start, *end);
            }
        }
        if !plays.is_empty() {
            tracks.push(TrackPlace {
                name: track.name.clone(),
                file,
                line: track.span.line,
                layer: track.layer.clone().unwrap_or_else(|| track.name.clone()),
                instrument,
                instrument_file: instrument_at.map(|at| at.0),
                instrument_line: instrument_at.map(|at| at.1),
                plays,
            });
        }
    }

    let mut lines: Vec<LineSpans> = by_line
        .into_iter()
        .map(|((file, line), spans)| {
            let mut starts = passes.remove(&(file, line)).unwrap_or_default();
            starts.sort_by(f64::total_cmp);
            // Two tracks playing one pattern at once are one pass of its lines.
            starts.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
            let mut placed = if starts.is_empty() { Vec::new() } else { notes.remove(&(file, line)).unwrap_or_default() };
            placed.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.col.cmp(&b.col)));
            LineSpans { file, line, spans: merged(spans), passes: starts, notes: placed }
        })
        .collect();
    let seconds = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.1)).fold(0.0, f64::max);
    for section in &song.sections {
        let span = (song.seconds((section.from_bar - 1.0) * bar), song.seconds(section.to_bar * bar));
        lines.push(LineSpans { file: section.file, line: section.line, spans: vec![span], passes: Vec::new(), notes: Vec::new() });
    }
    lines.sort_by_key(|l| (l.file, l.line));
    Placements { seconds, bar_seconds: song.seconds(bar), lines, tracks }
}

/// Sorted, with overlapping and touching stretches joined.
fn merged(mut spans: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (start, end) in spans {
        match out.last_mut() {
            Some(last) if start <= last.1 + 1e-6 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // At 120 bpm in 4/4 a bar is 2 s.
    const SONG: &str = "tempo 120
section chorus bars=3-4
instrument lead synth
  osc saw
instrument kit drums
pattern verse
  C4:h D4 |
  E4:h F4 |
pattern beat grid=1/8
  kick X...X...
  snare ..X...X.
track melody
  instrument lead
  play verse
  rest 2
  play verse
track drums
  instrument kit
  at 3
  play beat x2
track ghost
  instrument kit
  mute
  play beat
";

    fn line_of(text: &str, needle: &str) -> usize {
        text.lines().position(|l| l == needle).expect("the line is in the song") + 1
    }

    fn spans(p: &Placements, line: usize) -> Vec<(f64, f64)> {
        p.lines.iter().find(|l| l.line == line).map(|l| l.spans.clone()).unwrap_or_default()
    }

    fn placements_of(text: &str) -> Placements {
        let (song, diags) = crate::parse(text);
        assert!(!crate::diag::has_errors(&diags), "{diags:?}");
        placements(&song.expect("parses"))
    }

    #[test]
    fn a_play_step_is_heard_from_the_cursor_for_its_repeats() {
        let p = placements_of(SONG);
        let first_play = SONG.lines().position(|l| l == "  play verse").unwrap() + 1;
        assert_eq!(spans(&p, first_play), vec![(0.0, 4.0)]);
        assert_eq!(spans(&p, line_of(SONG, "  play beat x2")), vec![(4.0, 8.0)], "at 3 is 4 s in, two one-bar repeats");
        assert_eq!(p.bar_seconds, 2.0);
        assert_eq!(p.seconds, 12.0, "the second verse ends at bar 7");
    }

    #[test]
    fn a_patterns_line_is_heard_wherever_the_pattern_plays() {
        let p = placements_of(SONG);
        // `pattern verse` is played at 0 s and, after two bars' rest, at 8 s.
        assert_eq!(spans(&p, line_of(SONG, "  C4:h D4 |")), vec![(0.0, 2.0), (8.0, 10.0)]);
        assert_eq!(spans(&p, line_of(SONG, "  E4:h F4 |")), vec![(2.0, 4.0), (10.0, 12.0)]);
        assert_eq!(spans(&p, line_of(SONG, "pattern verse")), vec![(0.0, 4.0), (8.0, 12.0)]);
        // A grid row from its first hit to the end of its last, per repeat:
        // the snare's first hit is the third eighth (0.5 s), its last the
        // seventh, ending at 1.75 s.
        assert_eq!(spans(&p, line_of(SONG, "  snare ..X...X.")), vec![(4.5, 5.75), (6.5, 7.75)]);
    }

    #[test]
    fn headers_are_heard_where_what_they_name_is() {
        let p = placements_of(SONG);
        assert_eq!(spans(&p, line_of(SONG, "track melody")), vec![(0.0, 4.0), (8.0, 12.0)]);
        assert_eq!(spans(&p, line_of(SONG, "instrument kit drums")), vec![(4.0, 8.0)], "the muted track adds nothing");
        assert_eq!(spans(&p, line_of(SONG, "section chorus bars=3-4")), vec![(4.0, 8.0)]);
        assert!(spans(&p, line_of(SONG, "track ghost")).is_empty());
        assert!(spans(&p, line_of(SONG, "  osc saw")).is_empty(), "a setting is not placed");
    }

    #[test]
    fn a_patterns_line_knows_its_passes_and_where_each_note_is_written() {
        let p = placements_of(SONG);
        let at = |needle: &str| p.lines.iter().find(|l| l.line == line_of(SONG, needle)).unwrap().clone();
        let verse = at("  C4:h D4 |");
        assert_eq!(verse.passes, vec![0.0, 8.0], "the verse is played at 0 s and at 8 s");
        // `C4:h` is a half note from character 3, `D4` the other half from 8.
        assert_eq!(
            verse.notes,
            vec![NotePlace { start: 0.0, end: 1.0, col: 3, len: 4 }, NotePlace { start: 1.0, end: 2.0, col: 8, len: 2 }]
        );
        // A grid row's cells: the snare hits the third and seventh eighths,
        // written at characters 11 and 15 of `  snare ..X...X.`.
        let snare = at("  snare ..X...X.");
        assert_eq!(snare.passes, vec![4.0, 6.0]);
        assert_eq!(snare.notes.iter().map(|n| (n.start, n.col, n.len)).collect::<Vec<_>>(), vec![(0.5, 11, 1), (1.5, 15, 1)]);
        assert!(at("track melody").notes.is_empty() && at("track melody").passes.is_empty());
    }

    #[test]
    fn each_heard_track_says_what_it_plays_when() {
        let p = placements_of(SONG);
        assert_eq!(p.tracks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["melody", "drums"], "the muted ghost is not heard");
        let melody = &p.tracks[0];
        assert_eq!(melody.line, line_of(SONG, "track melody"));
        assert_eq!(melody.layer, "melody");
        assert_eq!(melody.instrument.as_deref(), Some("lead"));
        assert_eq!(melody.instrument_line, Some(line_of(SONG, "instrument lead synth")));
        let first_play = SONG.lines().position(|l| l == "  play verse").unwrap() + 1;
        assert_eq!(
            melody.plays[0],
            PlayPlace {
                file: 0,
                line: first_play,
                pattern: Some("verse".into()),
                pattern_file: Some(0),
                pattern_line: Some(line_of(SONG, "pattern verse")),
                start: 0.0,
                end: 4.0,
                pass_seconds: 4.0,
                transpose: 0.0,
            }
        );
        assert_eq!((melody.plays[1].start, melody.plays[1].end), (8.0, 12.0));
        let drums = &p.tracks[1];
        assert_eq!((drums.plays[0].start, drums.plays[0].end, drums.plays[0].pass_seconds), (4.0, 8.0, 2.0), "two one-bar passes");
    }

    #[test]
    fn a_chord_is_one_note() {
        let song = "tempo 120\ninstrument lead synth\n  osc saw\npattern p\n  [C4 E4 G4]:w |\ntrack t\n  instrument lead\n  play p\n";
        let p = placements_of(song);
        let chord = p.lines.iter().find(|l| l.line == 5).unwrap();
        assert_eq!(chord.notes, vec![NotePlace { start: 0.0, end: 2.0, col: 3, len: 12 }]);
    }

    #[test]
    fn a_line_in_an_included_file_says_its_file() {
        let root = "tempo 120\ninclude \"parts.song\"\ntrack melody\n  instrument lead\n  play verse\n";
        let parts = "section intro bars=1-1\ninstrument lead synth\n  osc saw\npattern verse\n  C4:h D4 |\n";
        let loader = |path: &std::path::Path| {
            assert_eq!(path, std::path::Path::new("/songs/parts.song"));
            Ok(parts.to_string())
        };
        let parsed = crate::parse_with(root, std::path::Path::new("/songs/song.song"), &loader);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let p = placements(&parsed.song.expect("parses"));
        let at = |file: usize, line: usize| p.lines.iter().find(|l| l.file == file && l.line == line).cloned();
        assert_eq!(at(0, 5).expect("the play step").spans, vec![(0.0, 2.0)]);
        assert_eq!(at(0, 3).expect("the track's header").spans, vec![(0.0, 2.0)]);
        let notes = at(1, 5).expect("the verse's line, in the included file");
        assert_eq!((notes.passes.clone(), notes.notes.len()), (vec![0.0], 2));
        assert!(at(0, 5).unwrap().notes.is_empty(), "line 5 of the song is not line 5 of the included file");
        assert!(at(1, 4).is_some() && at(1, 2).is_some(), "the pattern's and the instrument's headers");
        assert_eq!(at(1, 1).expect("the section").spans, vec![(0.0, 2.0)]);
        assert_eq!(p.lines.iter().map(|l| (l.file, l.line)).collect::<Vec<_>>(), [(0, 3), (0, 5), (1, 1), (1, 2), (1, 4), (1, 5)], "in file and line order");
        let track = &p.tracks[0];
        assert_eq!((track.file, track.line, track.instrument_file, track.instrument_line), (0, 3, Some(1), Some(2)));
        let play = &track.plays[0];
        assert_eq!((play.file, play.line, play.pattern_file, play.pattern_line), (0, 5, Some(1), Some(4)));
    }

    #[test]
    fn stretches_that_touch_are_one() {
        assert_eq!(merged(vec![(2.0, 4.0), (0.0, 2.0), (5.0, 6.0)]), vec![(0.0, 4.0), (5.0, 6.0)]);
    }

    // At 120 bpm a quarter is 0.5 s.
    const LOOPS: &str = "tempo 120
instrument lead synth
  osc saw
pattern riff
  (C4:e D4)x2 E4:h |
pattern hit
  C5:w |
track melody
  instrument lead
  rest 1
  repeat 2 {
    play riff
    repeat 2 {
      play hit
    }
  }
  play riff
";

    #[test]
    fn a_note_of_a_repeat_group_is_placed_at_its_token_on_every_pass() {
        let p = placements_of(LOOPS);
        let riff = p.lines.iter().find(|l| l.line == line_of(LOOPS, "  (C4:e D4)x2 E4:h |")).unwrap();
        // `C4:e` is written at character 4 and `D4` at 9: each twice in a pass, in start order.
        assert_eq!(
            riff.notes.iter().map(|n| (n.start, n.end, n.col, n.len)).collect::<Vec<_>>(),
            [(0.0, 0.25, 4, 4), (0.25, 0.5, 9, 2), (0.5, 0.75, 4, 4), (0.75, 1.0, 9, 2), (1.0, 2.0, 15, 4)]
        );
        assert_eq!(riff.passes, [2.0, 8.0, 14.0], "pattern passes, as ever: two in the repeat and the last play");
    }

    #[test]
    fn a_repeat_line_is_heard_across_its_passes_and_its_steps_at_each() {
        let p = placements_of(LOOPS);
        // bar 2: riff, hit, hit; bar 5: riff, hit, hit; bar 8: riff.
        assert_eq!(spans(&p, line_of(LOOPS, "  repeat 2 {")), [(2.0, 14.0)], "one span over both passes");
        assert_eq!(spans(&p, line_of(LOOPS, "    repeat 2 {")), [(4.0, 8.0), (10.0, 14.0)], "the inner block, in each outer pass");
        assert_eq!(spans(&p, line_of(LOOPS, "    play riff")), [(2.0, 4.0), (8.0, 10.0)]);
        assert_eq!(spans(&p, line_of(LOOPS, "      play hit")), [(4.0, 8.0), (10.0, 14.0)], "touching passes are one stretch");
        assert!(spans(&p, line_of(LOOPS, "    }")).is_empty() && spans(&p, line_of(LOOPS, "  }")).is_empty());
        let plays: Vec<(usize, f64, f64)> = p.tracks[0].plays.iter().map(|play| (play.line, play.start, play.end)).collect();
        let (riff, hit, last) = (line_of(LOOPS, "    play riff"), line_of(LOOPS, "      play hit"), line_of(LOOPS, "  play riff"));
        assert_eq!(
            plays,
            [(riff, 2.0, 4.0), (hit, 4.0, 6.0), (hit, 6.0, 8.0), (riff, 8.0, 10.0), (hit, 10.0, 12.0), (hit, 12.0, 14.0), (last, 14.0, 16.0)],
            "a play of a repeat block once per pass, each at its own line"
        );
    }

    #[test]
    fn a_repeat_of_rests_alone_is_not_heard() {
        let text = "tempo 120\ninstrument lead synth\npattern p\n  C4:w\ntrack t\n  instrument lead\n  repeat 2 {\n    rest 1\n  }\n  play p\n";
        let p = placements_of(text);
        assert!(spans(&p, 7).is_empty());
        assert_eq!(spans(&p, 10), [(4.0, 6.0)]);
    }
}
