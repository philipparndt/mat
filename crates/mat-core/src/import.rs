//! Converts MIDI notes into `.song` patterns and a track that plays them.
//!
//! Notes are quantized to a 1/16 grid. The part is cut into chunks of a few
//! bars; identical chunks share one pattern and repeats become `play xN`.
//! Monophonic chunks are written as note lines, polyphonic ones and drums as
//! grids (one row per pitch or drum).

use std::collections::HashMap;
use std::fmt::Write;

use crate::midi::MidiNote;
use crate::model::DrumKind;

pub struct ImportOptions {
    pub name: String,
    pub tempo: f64,
    /// Beats per bar in quarter notes (4/4 = 4.0).
    pub bar_quarters: f64,
    /// Time in seconds of bar 1 in the MIDI file.
    pub offset: f64,
    pub bars_per_pattern: usize,
    pub drums: bool,
    /// Write every note at the default velocity.
    pub ignore_velocity: bool,
    /// Extend every note up to the next note (monophonic parts played legato).
    pub legato: bool,
    /// Below 1.0, chunks whose notes match at least this share are grouped
    /// and replaced by their consensus (notes present in most repeats).
    /// This removes random transcription errors.
    pub similarity: f64,
}

const STEPS_PER_QUARTER: f64 = 4.0;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Note {
    step: usize,
    len: usize,
    key: u8,
    vel: u8,
}

pub fn to_song_text(notes: &[MidiNote], opt: &ImportOptions) -> String {
    let step_seconds = 60.0 / opt.tempo / STEPS_PER_QUARTER;
    let steps_per_bar = (opt.bar_quarters * STEPS_PER_QUARTER).round() as usize;
    let chunk_steps = steps_per_bar * opt.bars_per_pattern.max(1);

    let mut quantized: Vec<Note> = notes
        .iter()
        .filter_map(|n| {
            let start = ((n.start - opt.offset) / step_seconds).round();
            if start < 0.0 {
                return None;
            }
            let end = ((n.end - opt.offset) / step_seconds).round().max(start + 1.0);
            let vel = if opt.ignore_velocity { 100 } else { n.velocity };
            Some(Note { step: start as usize, len: (end - start) as usize, key: n.key, vel })
        })
        .collect();
    quantized.sort_by_key(|n| (n.step, n.key));
    quantized.dedup_by(|a, b| a.step == b.step && a.key == b.key);
    if opt.legato {
        for i in 0..quantized.len() {
            if let Some(next) = quantized[i + 1..].iter().find(|m| m.step > quantized[i].step).map(|m| m.step) {
                quantized[i].len = next - quantized[i].step;
            }
        }
    }

    let total_steps = quantized.iter().map(|n| n.step + if opt.drums { 1 } else { n.len }).max().unwrap_or(0);
    let chunks = total_steps.div_ceil(chunk_steps).max(1);

    let chunk_notes: Vec<Vec<Note>> = (0..chunks)
        .map(|c| {
            let from = c * chunk_steps;
            let to = from + chunk_steps;
            quantized
                .iter()
                .filter(|n| n.step >= from && n.step < to)
                .map(|n| Note { step: n.step - from, len: n.len.min(to - n.step), ..n.clone() })
                .collect()
        })
        .collect();

    // Group similar chunks and replace each by its group's consensus.
    let mut group_of: Vec<Option<usize>> = vec![None; chunks];
    let mut groups: Vec<(Vec<usize>, Vec<Note>)> = Vec::new();
    for (c, notes) in chunk_notes.iter().enumerate() {
        if notes.is_empty() {
            continue;
        }
        let best = groups
            .iter()
            .enumerate()
            .map(|(g, (_, consensus))| (g, similarity(consensus, notes)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, score)| *score >= opt.similarity);
        match best {
            Some((g, _)) => {
                groups[g].0.push(c);
                groups[g].1 = consensus(groups[g].0.iter().map(|&m| &chunk_notes[m]));
            }
            None => groups.push((vec![c], notes.clone())),
        }
        group_of[c] = Some(groups.iter().position(|(members, _)| members.contains(&c)).unwrap());
    }

    // Dropped notes leave holes; close them again for legato parts.
    if opt.legato {
        for (_, notes) in &mut groups {
            for i in 0..notes.len() {
                if let Some(next) = notes[i + 1..].iter().find(|m| m.step > notes[i].step).map(|m| m.step) {
                    notes[i].len = next - notes[i].step;
                }
            }
        }
    }
    let bodies: Vec<Option<String>> = group_of
        .iter()
        .map(|g| {
            g.map(|g| {
                let chunk = &groups[g].1;
                if opt.drums {
                    grid(chunk, chunk_steps, steps_per_bar, true)
                } else if is_monophonic(chunk) {
                    note_line(chunk, chunk_steps, steps_per_bar)
                } else {
                    grid(chunk, chunk_steps, steps_per_bar, false)
                }
            })
        })
        .collect();

    let mut out = String::new();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    for body in bodies.iter().flatten() {
        if !names.contains_key(body) {
            let name = format!("{}_{}", opt.name, (b'a' + (order.len() % 26) as u8) as char).to_string()
                + &if order.len() >= 26 { (order.len() / 26).to_string() } else { String::new() };
            names.insert(body.clone(), name.clone());
            order.push((name, body.clone()));
        }
    }
    for (name, body) in &order {
        let header = if body.starts_with("grid") { format!("pattern {name} grid=1/16 bars={}", opt.bars_per_pattern) } else { format!("pattern {name} bars={}", opt.bars_per_pattern) };
        let body = body.strip_prefix("grid\n").unwrap_or(body);
        let _ = writeln!(out, "{header}\n{body}");
    }

    let _ = writeln!(out, "track {}", opt.name);
    let _ = writeln!(out, "  instrument {}", if opt.drums { "kit" } else { "TODO" });
    let mut i = 0;
    while i < bodies.len() {
        let mut j = i + 1;
        while j < bodies.len() && bodies[j] == bodies[i] {
            j += 1;
        }
        let count = j - i;
        match &bodies[i] {
            None => {
                let _ = writeln!(out, "  rest {}", count * opt.bars_per_pattern);
            }
            Some(body) => {
                let repeat = if count > 1 { format!(" x{count}") } else { String::new() };
                let _ = writeln!(out, "  play {}{repeat}", names[body]);
            }
        }
        i = j;
    }
    out
}

/// Share of notes (by start step and key) the two chunks have in common.
fn similarity(a: &[Note], b: &[Note]) -> f64 {
    let set = |notes: &[Note]| notes.iter().map(|n| (n.step, n.key)).collect::<std::collections::HashSet<_>>();
    let (sa, sb) = (set(a), set(b));
    let union = sa.union(&sb).count();
    if union == 0 { 1.0 } else { sa.intersection(&sb).count() as f64 / union as f64 }
}

/// Notes that appear (same step and key) in more than half of the chunks,
/// with the median length and velocity of their occurrences.
fn consensus<'a>(chunks: impl Iterator<Item = &'a Vec<Note>>) -> Vec<Note> {
    let chunks: Vec<&Vec<Note>> = chunks.collect();
    if chunks.len() == 1 {
        return chunks[0].clone();
    }
    let mut occurrences: std::collections::HashMap<(usize, u8), Vec<&Note>> = std::collections::HashMap::new();
    for chunk in &chunks {
        for n in chunk.iter() {
            occurrences.entry((n.step, n.key)).or_default().push(n);
        }
    }
    let mut notes: Vec<Note> = occurrences
        .into_iter()
        .filter(|(_, found)| found.len() * 2 > chunks.len())
        .map(|((step, key), found)| {
            let mut lens: Vec<usize> = found.iter().map(|n| n.len).collect();
            let mut vels: Vec<u8> = found.iter().map(|n| n.vel).collect();
            lens.sort_unstable();
            vels.sort_unstable();
            Note { step, len: lens[lens.len() / 2], key, vel: vels[vels.len() / 2] }
        })
        .collect();
    notes.sort_by_key(|n| (n.step, n.key));
    // Keep lines playable: a note ends where the next one on the same key starts.
    for i in 0..notes.len() {
        if let Some(next) = notes[i + 1..].iter().find(|m| m.key == notes[i].key).map(|m| m.step) {
            notes[i].len = notes[i].len.min(next - notes[i].step).max(1);
        }
    }
    notes
}

fn is_monophonic(notes: &[Note]) -> bool {
    notes.windows(2).all(|w| w[0].step + w[0].len <= w[1].step)
}

fn note_name(key: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
    format!("{}{}", NAMES[key as usize % 12], key as i32 / 12 - 1)
}

fn duration(steps: usize) -> String {
    const UNITS: [(usize, &str); 8] = [(16, "w"), (12, "h."), (8, "h"), (6, "q."), (4, "q"), (3, "e."), (2, "e"), (1, "s")];
    let mut left = steps;
    let mut parts = Vec::new();
    while left > 0 {
        let (n, name) = UNITS.iter().find(|(n, _)| *n <= left).unwrap();
        parts.push(*name);
        left -= n;
    }
    parts.join("+")
}

/// Note lines with rests and a bar check after every bar.
fn note_line(notes: &[Note], total: usize, steps_per_bar: usize) -> String {
    let mut out = String::new();
    let mut line = String::from(" ");
    let mut pos = 0;
    let mut last_duration = String::new();
    let emit = |line: &mut String, token: String, steps: usize, last: &mut String| {
        let d = duration(steps);
        let text = if *last == d { token } else { format!("{token}:{d}") };
        *last = d;
        line.push(' ');
        line.push_str(&text);
    };
    let advance_to = |target: usize, pos: &mut usize, line: &mut String, out: &mut String, last: &mut String| {
        while *pos < target {
            let bar_end = (*pos / steps_per_bar + 1) * steps_per_bar;
            let rest = target.min(bar_end) - *pos;
            emit(line, "r".into(), rest, last);
            *pos += rest;
            if *pos % steps_per_bar == 0 {
                line.push_str(" |");
                out.push_str(line);
                out.push('\n');
                *line = String::from(" ");
            }
        }
    };
    for n in notes {
        advance_to(n.step, &mut pos, &mut line, &mut out, &mut last_duration);
        // A note crossing a bar line is written again in the next bar (a tie).
        let mut remaining = n.len.min(total - pos);
        while remaining > 0 {
            let bar_end = (pos / steps_per_bar + 1) * steps_per_bar;
            let len = remaining.min(bar_end - pos);
            let vel = if (n.vel as i32 - 100).abs() > 12 { format!("@{}", n.vel) } else { String::new() };
            let d = duration(len);
            let token = if last_duration == d { format!("{}{vel}", note_name(n.key)) } else { format!("{}:{d}{vel}", note_name(n.key)) };
            last_duration = d;
            line.push(' ');
            line.push_str(&token);
            pos += len;
            remaining -= len;
            if pos % steps_per_bar == 0 {
                line.push_str(" |");
                out.push_str(&line);
                out.push('\n');
                line = String::from(" ");
            }
        }
    }
    advance_to(total, &mut pos, &mut line, &mut out, &mut last_duration);
    out
}

/// One row per pitch (highest first) or per drum; `=` holds notes.
fn grid(notes: &[Note], total: usize, steps_per_bar: usize, drums: bool) -> String {
    let mut keys: Vec<u8> = notes.iter().map(|n| n.key).collect();
    keys.sort_unstable_by(|a, b| b.cmp(a));
    keys.dedup();
    let label = |key: u8| -> String {
        if drums {
            DrumKind::ALL.iter().find(|d| d.gm_note() == key).map_or_else(|| note_name(key), |d| d.name().to_string())
        } else {
            note_name(key)
        }
    };
    let width = keys.iter().map(|k| label(*k).len()).max().unwrap_or(4);
    let mut out = String::from("grid\n");
    for key in keys {
        let mut cells = vec!['.'; total];
        for n in notes.iter().filter(|n| n.key == key) {
            cells[n.step] = match n.vel {
                0..=50 => 'o',
                51..=115 => 'x',
                _ => 'X',
            };
            if !drums {
                for c in cells.iter_mut().take((n.step + n.len).min(total)).skip(n.step + 1) {
                    *c = '=';
                }
            }
        }
        let bars: Vec<String> = cells.chunks(steps_per_bar).map(|c| c.iter().collect()).collect();
        let _ = writeln!(out, "  {:<width$} {}", label(key), bars.join(" | "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_bars_become_one_pattern() {
        let tempo = 120.0;
        let step = 60.0 / tempo / 4.0;
        let mut notes = Vec::new();
        for bar in 0..4 {
            for beat in 0..4 {
                let start = (bar * 16 + beat * 4) as f64 * step;
                notes.push(MidiNote { start, end: start + 2.0 * step, key: 38, velocity: 100, channel: 0 });
            }
        }
        let opt = ImportOptions { name: "bass".into(), tempo, bar_quarters: 4.0, offset: 0.0, bars_per_pattern: 1, drums: false, ignore_velocity: false, legato: false, similarity: 1.0 };
        let text = to_song_text(&notes, &opt);
        assert!(text.contains("pattern bass_a bars=1\n  D2:e r D2 r D2 r D2 r |"), "{text}");
        assert!(text.contains("play bass_a x4"), "{text}");
        let (song, diags) = crate::parse(&text.replace("TODO", "lead").replace("track bass", "instrument lead synth\ntrack bass"));
        assert!(song.is_some(), "{diags:?}");
    }
}
