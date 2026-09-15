//! Where each line of a song is heard: for an editor to draw beside the line.
//!
//! A `play` step sounds from where the track's cursor is to the end of its
//! repeats; a line of a pattern sounds wherever the pattern is played, from
//! its first note to the end of its last; a track's header sounds wherever
//! any of its steps do, an instrument's wherever a track playing it does, and
//! a section is its bars.
//!
//! **Kept apart from `arrange`, on purpose.** The timeline `arrange` builds is
//! what the render cache hashes, and a source line in it would make adding a
//! comment above a track invalidate every cached layer. So the lines live
//! here, in a pass that walks the same steps with the same arithmetic and
//! feeds nothing that makes sound. Swing and humanize, which move notes by
//! milliseconds, are left out.

use crate::model::{Song, TrackStep};

/// One line and the stretches of the song, in seconds, where it is heard.
#[derive(Debug, Clone, PartialEq)]
pub struct LineSpans {
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
    /// In line order, and only lines that are heard somewhere.
    pub lines: Vec<LineSpans>,
}

pub fn placements(song: &Song) -> Placements {
    let bar = song.bar_length();
    let mut by_line: std::collections::BTreeMap<usize, Vec<(f64, f64)>> = Default::default();
    let mut passes: std::collections::BTreeMap<usize, Vec<f64>> = Default::default();
    let mut notes: std::collections::BTreeMap<usize, Vec<NotePlace>> = Default::default();
    let mut add = |line: usize, start: f64, end: f64| {
        if end > start {
            by_line.entry(line).or_default().push((start, end));
        }
    };

    for track in &song.tracks {
        if track.mute {
            continue;
        }
        let mut cursor = 0.0;
        let mut heard: Vec<(f64, f64)> = Vec::new();
        for step in &track.steps {
            match step {
                TrackStep::At { bar: b } => cursor = (b - 1.0) * bar,
                TrackStep::Rest { bars } => cursor += bars * bar,
                TrackStep::PlayAudio { bars, repeat, line } => {
                    let Some((source, _)) = &track.audio else { continue };
                    match bars {
                        // The whole file, bar 1 aligned: its length is the file's,
                        // which is not read here, so the step is marked for a bar.
                        None => {
                            let start = song.seconds(cursor) - source.offset;
                            add(*line, start.max(0.0), start.max(0.0) + song.seconds(bar));
                        }
                        Some((from, to)) => {
                            let length = (to - from + 1.0) * bar * *repeat as f64;
                            let span = (song.seconds(cursor), song.seconds(cursor + length));
                            add(*line, span.0, span.1);
                            heard.push(span);
                            cursor += length;
                        }
                    }
                }
                TrackStep::Play { pattern, span, repeat, .. } => {
                    let Some(pat) = song.patterns.iter().find(|p| &p.name == pattern) else { continue };
                    let length = pat.length * *repeat as f64;
                    let whole = (song.seconds(cursor), song.seconds(cursor + length));
                    add(span.line, whole.0, whole.1);
                    heard.push(whole);
                    // Each line of the pattern, at each repeat: its first note to
                    // the end of its last.
                    let mut lines: std::collections::BTreeMap<usize, (f64, f64)> = Default::default();
                    for event in &pat.events {
                        let entry = lines.entry(event.line).or_insert((f64::MAX, f64::MIN));
                        entry.0 = entry.0.min(event.start);
                        entry.1 = entry.1.max(event.start + event.duration);
                    }
                    for event in &pat.events {
                        let placed = notes.entry(event.line).or_default();
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
                        add(pat.span.line, song.seconds(offset), song.seconds(offset + pat.length));
                    }
                    cursor += length;
                }
            }
        }
        for (start, end) in &heard {
            add(track.span.line, *start, *end);
            if let Some((name, _)) = &track.instrument
                && let Some(instrument) = song.instruments.iter().find(|i| &i.name == name)
            {
                add(instrument.span.line, *start, *end);
            }
        }
    }

    let mut lines: Vec<LineSpans> = by_line
        .into_iter()
        .map(|(line, spans)| {
            let mut starts = passes.remove(&line).unwrap_or_default();
            starts.sort_by(f64::total_cmp);
            // Two tracks playing one pattern at once are one pass of its lines.
            starts.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
            let mut placed = if starts.is_empty() { Vec::new() } else { notes.remove(&line).unwrap_or_default() };
            placed.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.col.cmp(&b.col)));
            LineSpans { line, spans: merged(spans), passes: starts, notes: placed }
        })
        .collect();
    let seconds = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.1)).fold(0.0, f64::max);
    for section in &song.sections {
        let span = (song.seconds((section.from_bar - 1.0) * bar), song.seconds(section.to_bar * bar));
        lines.push(LineSpans { line: section.line, spans: vec![span], passes: Vec::new(), notes: Vec::new() });
    }
    lines.sort_by_key(|l| l.line);
    Placements { seconds, bar_seconds: song.seconds(bar), lines }
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
    fn a_chord_is_one_note() {
        let song = "tempo 120\ninstrument lead synth\n  osc saw\npattern p\n  [C4 E4 G4]:w |\ntrack t\n  instrument lead\n  play p\n";
        let p = placements_of(song);
        let chord = p.lines.iter().find(|l| l.line == 5).unwrap();
        assert_eq!(chord.notes, vec![NotePlace { start: 0.0, end: 2.0, col: 3, len: 12 }]);
    }

    #[test]
    fn stretches_that_touch_are_one() {
        assert_eq!(merged(vec![(2.0, 4.0), (0.0, 2.0), (5.0, 6.0)]), vec![(0.0, 4.0), (5.0, 6.0)]);
    }
}
