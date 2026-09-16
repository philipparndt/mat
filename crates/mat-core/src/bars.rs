//! Rendering part of a song: a range of bars, and a timeline cut to it.
//!
//! **Why it is here.** A four-minute song takes several seconds to render, and
//! an editor playing the bars somebody is working on should not wait for the
//! other three hundred. `mat render --bars 33-40` parses and arranges the
//! whole song — which is the cheap half — and then throws everything outside
//! those bars away before a single voice is synthesised, so the render costs
//! what eight bars cost and not what the song costs.
//!
//! **What a stretch carries in.** Everything that *starts* inside it sounds as
//! it does in the whole song: the same notes, with the same takes (a note's
//! randomness is a function of the note, not of its place in the file — see
//! `hash::NoteIdentities`), the same track and master settings, the same
//! sidechain ducking, and the value a sweep that began earlier has reached.
//! [`LEAD_IN_BARS`] bars before the stretch are rendered as well and then
//! dropped, so a note that begins just before it is already ringing at the
//! start.
//!
//! **What it does not.** Whatever was sounding when the lead-in began: a note
//! that started before it, a reverb or delay tail, a tb303's filter and slide
//! state, a scratch track's record of another track. An audio clip is cut to
//! the stretch, with mat's usual 5 ms fade at a cut. The first 5 ms of the
//! stretch fade in, because it begins in the middle of the music and a file
//! that starts at a sample far from zero clicks. A stretch is a stretch of the
//! song, not the song played from bar 33.

use serde::Serialize;

use crate::arrange::{TimedSection, Timeline};
use crate::model::InstrumentKind;

/// How many bars are rendered before the range asked for and then dropped, so
/// that notes which begin just before it are heard ringing at its start. Two
/// bars is long enough for a held chord and short enough to stay cheap.
pub const LEAD_IN_BARS: u32 = 2;

/// Times are compared in seconds, and a bar line is the same number on both
/// sides of the comparison.
const EPSILON: f64 = 1e-9;

/// A range of bars as a song numbers them: 1-based and inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarRange {
    pub from: u32,
    pub to: u32,
}

impl BarRange {
    /// Reads `<from>-<to>`, or a single bar. The errors say what to write
    /// instead, because this is typed on a command line.
    pub fn parse(text: &str) -> Result<BarRange, String> {
        let text = text.trim();
        let written = "bars are written as <from>-<to>, both included, or as one bar: --bars 9";
        let (from, to) = match text.split_once('-') {
            Some((from, to)) => (from.trim(), to.trim()),
            None => (text, text),
        };
        let number = |part: &str| -> Result<u32, String> {
            part.parse::<u32>().map_err(|_| match part.is_empty() {
                true => format!("'{text}' is missing a bar number; {written}"),
                false => format!("'{part}' is not a bar number; {written}"),
            })
        };
        let (from, to) = (number(from)?, number(to)?);
        if from == 0 {
            return Err(format!("'{text}': bars are numbered from 1; the first eight are --bars 1-8"));
        }
        if to < from {
            return Err(format!("'{text}' runs backwards; write --bars {to}-{from}"));
        }
        Ok(BarRange { from, to })
    }

    /// How many bars the range holds.
    pub fn count(&self) -> u32 {
        self.to - self.from + 1
    }
}

impl std::fmt::Display for BarRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.from == self.to {
            true => write!(f, "{}", self.from),
            false => write!(f, "{}-{}", self.from, self.to),
        }
    }
}

/// What part of a song a timeline holds, when it holds only part of one.
///
/// **Times in the timeline are counted from the lead-in**, which is where the
/// render starts. The audio a render gives back has the lead-in dropped from
/// its front, so *its* times are counted from the first bar of the stretch.
/// [`Window::lead_in_seconds`] is the difference, and the only conversion
/// between the two.
#[derive(Debug, Clone, Serialize)]
pub struct Window {
    /// The bars rendered, as the whole song numbers them: 1-based, inclusive.
    pub bars: [u32; 2],
    /// Bars rendered before them and then dropped again.
    pub lead_in_bars: u32,
    /// The lead-in in seconds — a whole number of samples at the rate it was
    /// cut for, so the stretch lands on the samples the whole render puts
    /// there and not a sample beside them.
    pub lead_in_seconds: f64,
    /// The stretch itself in seconds: its bars at the song's tempo.
    pub seconds: f64,
}

impl Window {
    /// The lead-in in samples: what a render drops from the front.
    pub fn lead_in_frames(&self, sample_rate: u32) -> usize {
        (self.lead_in_seconds * sample_rate as f64).round() as usize
    }
}

/// How many bars the song is: its last sound, rounded up to a whole bar.
pub fn song_bars(timeline: &Timeline) -> u32 {
    if timeline.bar_seconds <= 0.0 {
        return 1;
    }
    (timeline.end / timeline.bar_seconds - 1e-6).ceil().max(1.0) as u32
}

/// Cuts the timeline down to `range`, with a lead-in in front of it.
///
/// The result is an ordinary timeline that renders in the ordinary way; its
/// only difference is [`Timeline::window`], which says which bars it is and
/// how much of the front of the render is the lead-in. `sample_rate` is the
/// rate it will be rendered at: the cut moves everything by a whole number of
/// samples, so a note lands where the whole render puts it.
pub fn cut(timeline: &Timeline, range: BarRange, sample_rate: u32) -> Result<Timeline, String> {
    let total = song_bars(timeline);
    if range.from > total || range.to > total {
        let last = BarRange { from: (total + 1).saturating_sub(range.count()).max(1), to: total };
        return Err(format!(
            "--bars {range} is outside the song: it is {total} bar{} long\nhint: its last bars are --bars {last}",
            if total == 1 { "" } else { "s" }
        ));
    }
    let bar = timeline.bar_seconds;
    let rate = sample_rate as f64;
    let on_a_sample = |seconds: f64| (seconds * rate).round() / rate;
    let lead_in_bars = LEAD_IN_BARS.min(range.from - 1);
    // Where the render starts, where the stretch starts, and where it ends.
    let start = on_a_sample((range.from - 1 - lead_in_bars) as f64 * bar);
    let stretch = on_a_sample((range.from - 1) as f64 * bar);
    let end = range.to as f64 * bar;

    let mut cut = timeline.clone();
    for track in &mut cut.tracks {
        // A note that starts inside the render is rendered whole, tail and
        // all; one that started before it is gone, which is what the lead-in
        // is there to make rare.
        track.notes.retain(|n| n.start >= start - EPSILON && n.start < end - EPSILON);
        for note in &mut track.notes {
            note.start -= start;
        }
        // An audio clip is a file read: one that runs past the stretch is cut
        // to it, rather than reading four minutes of a file to hear eight
        // bars. One that began earlier keeps its place in the file —
        // `render::render_audio` winds a clip with a negative `at` forward.
        track.clips.retain(|c| c.at < end - EPSILON && c.length.is_none_or(|l| c.at + l > start + EPSILON));
        for clip in &mut track.clips {
            clip.at -= start;
            let room = end - start - clip.at;
            clip.length = Some(clip.length.map_or(room, |l| l.min(room)));
        }
        // Every sweep is kept, moved: one that finished before the stretch
        // still holds its last value there, which is what the song sounds like
        // at that bar.
        for sweep in &mut track.sweeps {
            sweep.start -= start;
            sweep.end -= start;
        }
        if let Some(duck) = &mut track.duck {
            duck.times.retain(|t| *t >= start - EPSILON && *t < end - EPSILON);
            for time in &mut duck.times {
                *time -= start;
            }
        }
        // A scratch track that cuts its record out of another track: where it
        // cuts from moves with everything else (`arrange` has already turned
        // it into seconds). It hears only what is inside the render.
        if let InstrumentKind::Scratch(def) = &mut track.instrument
            && def.source_track.is_some()
        {
            def.start -= start;
        }
    }
    cut.end = cut
        .tracks
        .iter()
        .flat_map(|t| t.notes.iter().map(|n| n.start + n.duration).chain(t.clips.iter().map(|c| c.at + c.length.unwrap_or(0.0))))
        .fold(0.0, f64::max);
    // The sections the stretch is in, clipped to it. Their bar numbers are
    // still the song's, so an editor can say which section it is hearing.
    cut.sections = timeline
        .sections
        .iter()
        .filter(|s| s.end > stretch + EPSILON && s.start < end - EPSILON)
        .map(|s| TimedSection { start: (s.start - start).max(stretch - start), end: (s.end - start).min(end - start), ..s.clone() })
        .collect();
    cut.window = Some(Window {
        bars: [range.from, range.to],
        lead_in_bars,
        lead_in_seconds: stretch - start,
        seconds: range.count() as f64 * bar,
    });
    Ok(cut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Sixteen bars, and nothing in the master that carries a state from one
    /// sample to the next: no reverb, no delay, no compressor, no limiter. A
    /// stretch of this is the whole render's own samples, and the test below
    /// says so to the last bit.
    const PLAIN: &str = "tempo 120
meter 4/4
section first bars=1-8
section second bars=9-16
instrument lead synth
  osc saw voices=3 spread=12
  drift 8
instrument kit drums
pattern tune
  C4:q D4 E4 F4 | G4:q A4 B4 C5 |
pattern beat grid=1/8
  kick X...x...
  hat  x.x.x.x.
track lead
  instrument lead
  play tune x8
track drums
  instrument kit
  play beat x16
master
  reverb off
  delay off
  comp off
  limiter off
";

    const RATE: u32 = 48_000;

    fn timeline(text: &str) -> Timeline {
        crate::compile(text).expect("the test song compiles").0
    }

    fn range(text: &str) -> BarRange {
        BarRange::parse(text).expect("a bar range")
    }

    #[test]
    fn a_bar_range_is_from_to_or_one_bar() {
        assert_eq!(range("33-40"), BarRange { from: 33, to: 40 });
        assert_eq!(range("9"), BarRange { from: 9, to: 9 });
        assert_eq!(range(" 1 - 8 "), BarRange { from: 1, to: 8 });
        assert_eq!(range("5-5").count(), 1);
        assert_eq!(range("33-40").count(), 8);
        assert_eq!(range("9").to_string(), "9");
        assert_eq!(range("33-40").to_string(), "33-40");
    }

    #[test]
    fn a_bar_range_that_cannot_be_read_says_what_to_write() {
        let says = |text: &str| BarRange::parse(text).expect_err("not a range");
        assert!(says("0-8").contains("numbered from 1"), "{}", says("0-8"));
        assert!(says("0").contains("numbered from 1"), "{}", says("0"));
        assert!(says("9-3").contains("write --bars 3-9"), "{}", says("9-3"));
        assert!(says("eight").contains("<from>-<to>"), "{}", says("eight"));
        assert!(says("5-").contains("missing a bar number"), "{}", says("5-"));
        assert!(says("-5").contains("missing a bar number"), "{}", says("-5"));
        assert!(says("1-2-3").contains("not a bar number"), "{}", says("1-2-3"));
    }

    #[test]
    fn a_range_outside_the_song_is_an_error_that_says_how_long_it_is() {
        let t = timeline(PLAIN);
        assert_eq!(song_bars(&t), 16);
        let said = cut(&t, range("17-24"), RATE).expect_err("outside the song");
        assert!(said.contains("16 bars long"), "{said}");
        assert!(said.contains("--bars 9-16"), "{said}");
        assert!(cut(&t, range("13-16"), RATE).is_ok(), "the last bars are inside it");
        assert!(cut(&t, range("16"), RATE).is_ok(), "and so is the last one");
    }

    /// The bars of the stretch, and the lead-in in front of them.
    #[test]
    fn a_cut_timeline_holds_its_bars_and_a_lead_in() {
        let t = timeline(PLAIN);
        let part = cut(&t, range("5-8"), RATE).expect("in range");
        let window = part.window.as_ref().expect("a window");
        assert_eq!(window.bars, [5, 8]);
        assert_eq!(window.lead_in_bars, 2);
        assert!((window.lead_in_seconds - 4.0).abs() < 1e-12, "two bars at 120 bpm");
        assert!((window.seconds - 8.0).abs() < 1e-12, "four bars at 120 bpm");
        assert_eq!(window.lead_in_frames(RATE), 4 * RATE as usize);
        // Bars 3 to 8, counted from the lead-in.
        let starts: Vec<f64> = part.tracks[0].notes.iter().map(|n| n.start).collect();
        assert!(starts.iter().all(|s| *s >= 0.0 && *s < 12.0), "{starts:?}");
        assert_eq!(starts.len(), 6 * 4, "six bars of four notes");
        // The first bar of the song has no lead-in to render.
        let first = cut(&t, range("1-4"), RATE).expect("in range");
        let window = first.window.as_ref().expect("a window");
        assert_eq!((window.lead_in_bars, window.lead_in_seconds), (0, 0.0));
    }

    /// The sections the stretch is in, clipped to it and counted from the
    /// lead-in, with the song's own bar numbers.
    #[test]
    fn the_sections_of_a_stretch_are_the_ones_it_is_in() {
        let t = timeline(PLAIN);
        let part = cut(&t, range("7-10"), RATE).expect("in range");
        let names: Vec<&str> = part.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["first", "second"], "bars 7-10 cross both");
        let lead_in = part.window.as_ref().expect("a window").lead_in_seconds;
        assert!((part.sections[0].start - lead_in).abs() < 1e-9, "clipped to the stretch");
        assert_eq!(part.sections[0].from_bar, 1.0, "the song's own bar numbers");
        assert!((part.sections[1].end - (lead_in + 8.0)).abs() < 1e-9, "clipped to the stretch");
        let one = cut(&t, range("2-3"), RATE).expect("in range");
        assert_eq!(one.sections.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["first"]);
    }

    /// The whole point: a stretch of a song with no state running through the
    /// master is the very samples the whole render puts at those bars. The
    /// first 5 ms are the fade over the cut, and the comparison starts after
    /// them.
    #[test]
    fn a_stretch_is_the_whole_renders_own_samples_at_those_bars() {
        let song = timeline(PLAIN);
        let part = cut(&song, range("5-8"), RATE).expect("in range");
        let window = part.window.clone().expect("a window");
        let (whole, _) = crate::render(&song, RATE, HashMap::new());
        let (part, _) = crate::render(&part, RATE, HashMap::new());
        let at = 4 * 2 * RATE as usize; // bar 5, of a two-second bar
        let after_the_fade = RATE as usize / 100; // the 5 ms fade, and room around it
        let length = (window.seconds * RATE as f64) as usize;
        let trailing = RATE as usize / 20; // the 50 ms fade at the end of a render
        assert!(part.left.len() > length + trailing, "the stretch runs past its last bar");
        assert!(whole.left.len() > at + length, "and the whole song is longer still");
        let differs = (after_the_fade..length).find(|&i| part.left[i] != whole.left[at + i] || part.right[i] != whole.right[at + i]);
        assert_eq!(differs, None, "the stretch is not the song's own samples there");
        assert!(part.rms_db() > -40.0, "and it is heard: {:.1} dBFS", part.rms_db());
    }

    /// With reverb and delay on, the two cannot be the same samples, and this
    /// says how close they are instead.
    ///
    /// The stretch starts with an empty reverb and an empty delay line where
    /// the whole song arrives at bar 5 with four bars of tails in them; the
    /// plate's 0.9 Hz modulation starts with the render as well, so from bar 5
    /// on its wet signal is a different — and equally correct — realisation of
    /// the same reverb, which does not converge on the whole render's however
    /// long the stretch runs. Measured on this song, with a heavy send: the
    /// difference is 8.7 dB under the music and no 100 ms of it is more than
    /// 3.7 dB from the whole render's level. So: the same music, at the same
    /// loudness, in a reverb of the same size — not the same samples.
    #[test]
    fn a_stretch_of_a_song_with_tails_is_close_to_the_whole_render() {
        let with_tails = PLAIN.replace("  reverb off\n  delay off\n", "").replace("  play tune x8", "  reverb 0.35\n  delay 0.25\n  play tune x8");
        let song = timeline(&with_tails);
        let part = cut(&song, range("5-8"), RATE).expect("in range");
        let window = part.window.clone().expect("a window");
        let (whole, _) = crate::render(&song, RATE, HashMap::new());
        let (part, _) = crate::render(&part, RATE, HashMap::new());
        let at = 4 * 2 * RATE as usize;
        let length = (window.seconds * RATE as f64) as usize;
        let db = |sum: f64, over: usize| 10.0 * (sum / over as f64).max(1e-18).log10();
        let level = |take: &dyn Fn(usize) -> f64, from: usize, to: usize| db((from..to).map(|i| take(i) * take(i)).sum(), to - from);
        let (there, here) = (|i: usize| whole.left[at + i] as f64, |i: usize| part.left[i] as f64);

        let signal = level(&there, 0, length);
        let difference = db((0..length).map(|i| (there(i) - here(i)).powi(2)).sum(), length);
        assert!(signal > -40.0, "the bars are heard: {signal:.1} dBFS");
        assert!(difference < signal - 6.0, "what the stretch does not carry in is only {:.1} dB under the music", signal - difference);
        assert!((level(&here, 0, length) - signal).abs() < 1.0, "the stretch is as loud as the song is there");
        let block = RATE as usize / 10;
        let worst = (0..length / block)
            .map(|b| (level(&there, b * block, (b + 1) * block) - level(&here, b * block, (b + 1) * block)).abs())
            .fold(0.0, f64::max);
        assert!(worst < 5.0, "one 100 ms of the stretch is {worst:.1} dB away from the whole render's level there");
    }

    /// An audio clip that runs through the stretch is cut to it and keeps its
    /// place in the file.
    #[test]
    fn an_audio_clip_is_cut_to_the_stretch() {
        let mut t = timeline(PLAIN);
        t.tracks[0].clips = vec![crate::arrange::AudioClip { at: 0.0, source_start: 0.0, length: None }];
        let part = cut(&t, range("5-8"), RATE).expect("in range");
        let clip = &part.tracks[0].clips[0];
        // Four bars back: two before the lead-in, two of lead-in. The render
        // winds the part before its own start off the front of the file.
        assert_eq!(clip.at, -4.0);
        assert_eq!(clip.length, Some(16.0), "to the end of the stretch, and no further");
        assert_eq!(clip.source_start, 0.0, "read from where it was");
    }
}
