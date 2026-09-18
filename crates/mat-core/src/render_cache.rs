//! Layers kept between renders, so an edit costs the layer it touched.
//!
//! **Why it is needed.** A four-minute song of ten layers took nine to eleven
//! seconds to render, and an editor renders on every save: the voices of every
//! track, then each layer's inserts, delay, reverb and master EQ, then the
//! master's dynamics over the sum. An edit to one pattern changes one or two
//! layers of those ten. Everything else was recomputed to the same numbers.
//!
//! **What a layer is keyed by.** Everything its samples are a function of, and
//! nothing else: its tracks as the arranger resolved them (instrument, notes
//! with their seeds, gain, pan, sends, inserts, ducking triggers, sweeps), the
//! tracks its scratch tracks cut their records from, the tracks keying the
//! master sidechain, the master's delay, reverb, sidechain, gain, EQ and width,
//! the sample rate, and the size and modification time of every file those
//! name — a sample, an `.exs`, a patch, an audio track. Not the layer's
//! position in the song, not the other layers, and not the song's length:
//! a layer is computed to its own end (see `render::render_with`). A layer with
//! an Audio Unit track is not cached; its audio comes from another process.
//! When the render is of part of a song (`mat render --bars 33-40`), the bars
//! are keyed too, so a stretch and the whole song are never the same layer.
//!
//! **Why notes carry seeds.** A key is only worth anything if an edit elsewhere
//! leaves it alone, and a note's randomness used to come from its track's
//! position in the file. See `hash::NoteIdentities`.
//!
//! **The files.** `<key>.layer`: a small header and the two channels as
//! little-endian `f32`, left then right. Written to a temporary name and
//! renamed, so a render that is killed half-way leaves no half of a layer
//! under a key. A file is touched whenever a render uses it, and a file no
//! render has used for `KEPT_FOR` is deleted by the next render — long enough
//! for undo to find the layer it takes you back to.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::arrange::Timeline;
use crate::hash::Fnv;
use crate::model::InstrumentKind;

const MAGIC: &[u8; 8] = b"MATLAYR1";

/// How long a cached layer outlives the last render that used it.
pub const KEPT_FOR: Duration = Duration::from_secs(30 * 60);

/// Bumped whenever what a layer's samples are a function of changes shape in
/// a way the key would not see — a new effect stage, a changed default.
const FORMAT: &str = "mat-layer-2";

pub struct LayerCache {
    dir: PathBuf,
}

impl LayerCache {
    pub fn open(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        LayerCache { dir }
    }

    pub fn path(&self, key: u64) -> PathBuf {
        self.dir.join(format!("{key:016x}.layer"))
    }

    pub fn has(&self, key: u64) -> bool {
        self.path(key).is_file()
    }

    pub fn store(&self, key: u64, left: &[f32], right: &[f32]) -> Result<(), String> {
        let path = self.path(key);
        let temporary = self.dir.join(format!("{key:016x}.{}.tmp", std::process::id()));
        let write = || -> std::io::Result<()> {
            let mut out = BufWriter::with_capacity(1 << 20, File::create(&temporary)?);
            out.write_all(MAGIC)?;
            out.write_all(&(left.len() as u64).to_le_bytes())?;
            for channel in [left, right] {
                for sample in channel {
                    out.write_all(&sample.to_le_bytes())?;
                }
            }
            out.flush()?;
            Ok(())
        };
        write().map_err(|e| e.to_string())?;
        fs::rename(&temporary, &path).map_err(|e| e.to_string())
    }

    pub fn load(&self, key: u64) -> Result<(Vec<f32>, Vec<f32>), String> {
        let mut file = File::open(self.path(key)).map_err(|e| e.to_string())?;
        let mut header = [0u8; 16];
        file.read_exact(&mut header).map_err(|e| e.to_string())?;
        if &header[..8] != MAGIC {
            return Err("not a cached layer".into());
        }
        let frames = u64::from_le_bytes(header[8..16].try_into().expect("eight bytes")) as usize;
        let mut bytes = Vec::with_capacity(frames * 8);
        file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() != frames * 8 {
            return Err(format!("expected {} frames, found {}", frames, bytes.len() / 8));
        }
        let samples = |from: usize| -> Vec<f32> {
            bytes[from..from + frames * 4].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
        };
        Ok((samples(0), samples(frames * 4)))
    }

    /// Marks these layers as used now, and deletes what no render has used
    /// for `KEPT_FOR`.
    pub fn keep(&self, used: impl IntoIterator<Item = u64>) {
        let now = SystemTime::now();
        for key in used {
            if let Ok(file) = File::options().append(true).open(self.path(key)) {
                let _ = file.set_modified(now);
            }
        }
        self.evict(now);
    }

    fn evict(&self, now: SystemTime) {
        let Ok(entries) = fs::read_dir(&self.dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_ours = path.extension().is_some_and(|e| e == "layer" || e == "tmp" || e == "wav" || e == "flac" || e == "m4a");
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|when| now.duration_since(when).ok())
                .is_some_and(|age| age > KEPT_FOR);
            if is_ours && old {
                let _ = fs::remove_file(path);
            }
        }
    }

    /// Where a finished stem of a layer is kept: the layer's key, the frames
    /// it was cut to and how it was written. A stem that is already there is
    /// linked into a render's output rather than written again.
    pub fn stem_path(&self, key: u64, frames: usize, variant: &str, extension: &str) -> PathBuf {
        self.dir.join(format!("{key:016x}-{frames}-{variant}.{extension}"))
    }

    /// Marks a stem as used now.
    pub fn touch(&self, path: &Path) {
        if let Ok(file) = File::options().append(true).open(path) {
            let _ = file.set_modified(SystemTime::now());
        }
    }
}

/// A serialised track without its regions and its notes' region indices.
pub(crate) fn without_placements(track: &mut serde_json::Value) {
    let Some(track) = track.as_object_mut() else { return };
    track.remove("regions");
    if let Some(notes) = track.get_mut("notes").and_then(|n| n.as_array_mut()) {
        for note in notes {
            if let Some(note) = note.as_object_mut() {
                note.remove("region");
            }
        }
    }
}

/// What a layer is keyed by, or nil when it cannot be cached.
pub fn layer_key(timeline: &Timeline, members: &[usize], sample_rate: u32) -> Option<u64> {
    let tracks = &timeline.tracks;
    if members.iter().any(|&ti| matches!(tracks[ti].instrument, InstrumentKind::AudioUnit(_))) {
        return None;
    }
    let mut hash = Fnv::default().str(FORMAT).str(env!("CARGO_PKG_VERSION")).u64(sample_rate as u64);
    // Which bars, when this is only part of a song. The notes in the layer say
    // it too — they are the ones inside the stretch, moved to it — but a layer
    // with nothing in those bars has the same empty note list whatever was
    // asked for, and a cached layer is trimmed by the range it was rendered
    // for, not by the one in the file. So the range is keyed, and a partial
    // layer is never read back for a whole render or for other bars.
    if let Some(window) = &timeline.window {
        hash = hash.str("bars").u64(window.bars[0] as u64).u64(window.bars[1] as u64).u64(window.lead_in_bars as u64);
    }
    let mut files: Vec<String> = Vec::new();

    let add_track = |hash: Fnv, ti: usize, files: &mut Vec<String>| -> Option<Fnv> {
        let mut value = serde_json::to_value(&tracks[ti]).ok()?;
        // Where the track's `play` lines are written is not what it sounds
        // like: keyed, moving a line would render the layer again, and a
        // region's file would be taken for a file the layer reads.
        without_placements(&mut value);
        collect_files(&value, files);
        Some(hash.str(&value.to_string()))
    };

    for &ti in members {
        hash = add_track(hash, ti, &mut files)?;
        if let InstrumentKind::Scratch(def) = &tracks[ti].instrument
            && let Some(source) = &def.source_track
        {
            for (si, other) in tracks.iter().enumerate() {
                let wanted = if source == "mix" { !matches!(other.instrument, InstrumentKind::Scratch(_)) } else { &other.name == source };
                if wanted {
                    hash = add_track(hash.str("source"), si, &mut files)?;
                }
            }
        }
    }

    let master = &timeline.master;
    let linear = serde_json::json!({
        "delay": master.delay,
        "delay_seconds": timeline.delay_seconds,
        "reverb": master.reverb,
        "sidechain": master.sidechain,
        "gain_db": master.gain_db,
        "eq": master.eq,
        "width": master.width,
    });
    hash = hash.str(&linear.to_string());
    if let Some(sidechain) = &master.sidechain {
        for (ti, track) in tracks.iter().enumerate() {
            if track.name == sidechain.source || track.layer == sidechain.source {
                if matches!(track.instrument, InstrumentKind::AudioUnit(_)) {
                    return None;
                }
                hash = add_track(hash.str("key"), ti, &mut files)?;
            }
        }
    }

    files.sort();
    files.dedup();
    for path in files {
        let stamp = fs::metadata(&path).ok().map(|m| {
            let modified = m.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as u64);
            (m.len(), modified)
        });
        let (len, modified) = stamp.unwrap_or((0, 0));
        hash = hash.str(&path).u64(len).u64(modified);
    }
    Some(hash.finish())
}

/// Every string in a value that names an existing file: the samples, patches
/// and audio a track reads. Found by looking rather than by listing the
/// fields, so an instrument kind added later is covered without anybody
/// remembering this.
fn collect_files(value: &serde_json::Value, files: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            if text.contains('/') && Path::new(text).is_file() {
                files.push(text.clone());
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_files(v, files)),
        serde_json::Value::Object(map) => map.values().for_each(|v| collect_files(v, files)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{RenderOptions, render_with};
    use std::collections::HashMap;

    const SONG: &str = "tempo 120
instrument a synth
  osc saw voices=3 spread=12
  drift 8
instrument b synth
  osc square
instrument kit drums
pattern p
  C4:q D4 E4 F4 |
pattern q
  G4:h A4:h |
pattern beat grid=1/8
  kick X...X...
track one
  instrument a
  reverb 0.3
  delay 0.2
  play p x2
track two
  instrument b
  pan 0.4
  play q x2
track drums
  instrument kit
  play beat x2
master
  sidechain drums threshold=-20 ratio=4
  gain 2
";

    fn timeline(text: &str) -> Timeline {
        crate::compile(text).expect("the test song compiles").0
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mat-cache-test-{}-{}", std::process::id(), Fnv::default().str(&format!("{:?}", std::thread::current().id())).finish()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn keys(t: &Timeline) -> HashMap<String, Option<u64>> {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (ti, track) in t.tracks.iter().enumerate() {
            match groups.iter_mut().find(|(l, _)| l == &track.layer) {
                Some((_, m)) => m.push(ti),
                None => groups.push((track.layer.clone(), vec![ti])),
            }
        }
        groups.into_iter().map(|(l, m)| (l.clone(), layer_key(t, &m, 48_000))).collect()
    }

    /// An edit to one track changes that layer's key and no other.
    #[test]
    fn an_edit_changes_only_its_own_layers_key() {
        let before = keys(&timeline(SONG));
        let after = keys(&timeline(&SONG.replace("  G4:h A4:h |", "  G4:h B4:h |")));
        assert_ne!(before["two"], after["two"]);
        assert_eq!(before["one"], after["one"]);
        assert_eq!(before["drums"], after["drums"]);
    }

    /// Where a `play` is written is not how it sounds: comments that move
    /// every line down leave every layer's key alone, though the regions the
    /// export gives say the new lines.
    #[test]
    fn moving_the_lines_leaves_every_key_alone() {
        let before = keys(&timeline(SONG));
        let moved = timeline(&format!("# a comment\n# and another\n{SONG}"));
        assert_eq!(before, keys(&moved));
        assert!(moved.tracks.iter().all(|t| !t.regions.is_empty()));
    }

    /// A track inserted before the others changes nobody's take.
    #[test]
    fn a_track_added_above_leaves_the_others_keys_alone() {
        let before = keys(&timeline(SONG));
        let added = SONG.replace("track one\n", "track zero\n  instrument b\n  play q\ntrack one\n");
        let after = keys(&timeline(&added));
        assert_eq!(before["one"], after["one"]);
        assert_eq!(before["two"], after["two"]);
    }

    /// Changing what keys the sidechain changes every layer it ducks.
    #[test]
    fn the_sidechains_key_is_part_of_every_layer() {
        let before = keys(&timeline(SONG));
        let after = keys(&timeline(&SONG.replace("  kick X...X...", "  kick X.X.X...")));
        assert_ne!(before["one"], after["one"]);
    }

    /// A layer of a song that includes a file is keyed by what the file
    /// says: an edit there changes the layers it is heard in, and a comment
    /// or a moved line changes nothing.
    #[test]
    fn an_edit_in_an_included_file_changes_the_layers_it_is_heard_in() {
        let root = "tempo 120\ninclude \"parts.song\"\ntrack one\n  instrument a\n  play p x2\ntrack two\n  instrument b\n  play q x2\n";
        let parts = "instrument a synth\n  osc saw\ninstrument b synth\n  osc square\npattern p\n  C4:q D4 E4 F4 |\npattern q\n  G4:h A4:h |\n";
        let keys_of = |parts: &str| {
            let parts = parts.to_string();
            let loader = move |_: &Path| Ok(parts.clone());
            let parsed = crate::parse_with(root, Path::new("/songs/song.song"), &loader);
            assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
            keys(&crate::arrange(&parsed.song.expect("parses")).expect("arranges"))
        };
        let before = keys_of(parts);
        let edited = keys_of(&parts.replace("G4:h A4:h |", "G4:h B4:h |"));
        assert_ne!(before["two"], edited["two"], "the edited pattern's layer");
        assert_eq!(before["one"], edited["one"]);
        let sound = keys_of(&parts.replace("  osc saw", "  osc sine"));
        assert_ne!(before["one"], sound["one"], "the edited instrument's layer");
        assert_eq!(before["two"], sound["two"]);
        let moved = keys_of(&format!("# the parts\n\n{}pattern p # moved\n  C4:q D4 E4 F4 |\n", parts.replace("pattern p\n  C4:q D4 E4 F4 |\n", "")));
        assert_eq!(before, moved, "comments and moved lines are not heard");
    }

    /// A stretch of a song and the whole song share a cache directory and must
    /// never share a layer in it: whichever is rendered first, the other is
    /// the samples it would have been on its own.
    #[test]
    fn a_stretch_and_the_whole_song_are_different_layers_in_the_cache() {
        let long = SONG.replace("play p x2", "play p x8").replace("play q x2", "play q x8").replace("play beat x2", "play beat x8");
        let whole = timeline(&long);
        let part = crate::bars::cut(&whole, crate::bars::BarRange { from: 5, to: 8 }, 48_000).expect("in range");
        let alone = |t: &Timeline| render_with(t, 48_000, HashMap::new(), &RenderOptions { split: true, cache: None, stems_through_master: false }).mix;
        let (whole_alone, part_alone) = (alone(&whole), alone(&part));
        assert_ne!(whole_alone.left.len(), part_alone.left.len(), "the two renders are different lengths");

        for order in [[&part, &whole], [&whole, &part]] {
            let dir = scratch();
            let options = RenderOptions { split: true, cache: Some(dir.clone()), stems_through_master: false };
            let first = render_with(order[0], 48_000, HashMap::new(), &options);
            let second = render_with(order[1], 48_000, HashMap::new(), &options);
            for (rendered, on_its_own) in [(&first, order[0]), (&second, order[1])] {
                let expected = if std::ptr::eq(on_its_own, &whole) { &whole_alone } else { &part_alone };
                assert_eq!(&rendered.mix.left, &expected.left, "a render beside the other one in the cache is its own samples");
                assert_eq!(&rendered.mix.right, &expected.right);
            }
            // And each is still cached for itself.
            let again = render_with(order[0], 48_000, HashMap::new(), &options);
            assert!(again.layers.iter().all(|l| l.cached), "the first render's layers are still there");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    /// A render that reads its layers back is the same numbers as one that
    /// made them.
    #[test]
    fn a_cached_render_is_the_same_as_a_fresh_one() {
        let dir = scratch();
        let t = timeline(SONG);
        let options = RenderOptions { split: true, cache: Some(dir.clone()), stems_through_master: false };
        let fresh = render_with(&t, 48_000, HashMap::new(), &options);
        assert!(fresh.layers.iter().all(|l| !l.cached));
        let again = render_with(&t, 48_000, HashMap::new(), &options);
        assert!(again.layers.iter().all(|l| l.cached), "every layer came from the cache");
        assert_eq!(fresh.mix.left, again.mix.left);
        assert_eq!(fresh.mix.right, again.mix.right);

        let uncached = render_with(&t, 48_000, HashMap::new(), &RenderOptions { split: true, cache: None, stems_through_master: false });
        assert_eq!(fresh.mix.left, uncached.mix.left, "the cache changes nothing about the sound");

        let edited = timeline(&SONG.replace("  G4:h A4:h |", "  G4:h B4:h |"));
        let partial = render_with(&edited, 48_000, HashMap::new(), &options);
        let rendered: Vec<&str> = partial.layers.iter().filter(|l| !l.cached).map(|l| l.layer.as_str()).collect();
        assert_eq!(rendered, ["two"], "only the edited layer is rendered");
        let full = render_with(&edited, 48_000, HashMap::new(), &RenderOptions { split: true, cache: None, stems_through_master: false });
        assert_eq!(partial.mix.left, full.mix.left, "a partial render is the same as a full one");
        let _ = fs::remove_dir_all(&dir);
    }
}
