//! A song and everything it reads, in one zip: `mat pack`.
//!
//! **What goes in.** The song, every file it includes, and every file those
//! name that is the song's to hand on: its samples, its audio tracks, its own
//! `.exs` instruments and the samples they load, its patches, and the samples
//! it takes from mat's library (`samples:`). Nothing else from the folders
//! they are in — a pack is what the song reads, not a copy of a directory.
//!
//! **One folder, and the paths point into it.** The files keep the places
//! they have under the song's folder; a file from anywhere else goes into
//! `external/`, and a library sample into `mat-samples/`. A path in the text
//! is written again only where it has to be, as a path from the file it is in,
//! so what is unchanged reads as it did. The zip is one folder named after the
//! song, and the song in it renders from there.
//!
//! **What stays out.** Instruments that come with software installed where the
//! song is played — Logic's and GarageBand's libraries, the macOS General MIDI
//! bank, Surge's patches, Audio Unit and CLAP plugins — are left as they are
//! written and listed in the pack's `README.txt`: Apple's sample content is not
//! the song's to hand on, and a plugin is not a file.
//!
//! **A preset whose samples are packed is written out.** `instrument drums
//! preset house-kit` names its samples in mat's own `presets.song`, not in the
//! song, so there is no path in the song to point into the pack. In the pack
//! the line becomes `instrument drums samples` with the preset's lines under
//! it, their samples pointing into `mat-samples/`. A preset that reads nothing
//! the pack carries — a synth, or Logic's piano — stays one line.
//!
//! **Why the token is found by what it names.** A path is a word on a line,
//! and which words are paths is the parser's knowledge. So the parser says
//! which files the song reads, and a word is a path when it names one of them
//! from the file it is in. A file read that no word names is an error rather
//! than a pack that quietly still points out of itself.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::arrange::resolve_load_path;
use crate::lexer::lex_file;
use crate::model::{InstrumentKind, Song};
use crate::parser::{Parsed, normalize};

/// The prefixes of instruments that come with software installed elsewhere.
const INSTALLED: &[&str] = &["logic:", "garageband:", "surge:", "surge-3rdparty:"];
/// Where a library sample goes in the pack.
pub const LIBRARY_FOLDER: &str = "mat-samples";
/// Where a file from outside the song's folder goes in the pack.
pub const EXTERNAL_FOLDER: &str = "external";

/// What a pack holds, worked out before anything is written.
#[derive(Debug)]
pub struct Plan {
    /// The folder the zip holds everything in: the song's name.
    pub folder: String,
    /// The song's own file in the pack.
    pub song: String,
    /// Every file, by its place in the pack.
    pub files: BTreeMap<String, Entry>,
    /// What the song needs installed where it is played, said in the README.
    pub needs: BTreeSet<String>,
    /// How many paths in the text were written again.
    pub rewritten: usize,
    /// The presets written out into the song, as `preset (instrument)`.
    pub written_out: BTreeSet<String>,
}

#[derive(Debug)]
pub enum Entry {
    /// A song file, as it is to be written.
    Text(String),
    /// A file copied as it is.
    Copy(PathBuf),
}

/// What `pack` wrote.
#[derive(Debug)]
pub struct Packed {
    pub files: usize,
    pub bytes: u64,
    pub rewritten: usize,
    pub needs: Vec<String>,
}

/// Plans the pack of the song at `path`, parsed as `parsed`. An error lists
/// every file that is missing or cannot be placed.
pub fn plan(path: &Path, parsed: &Parsed) -> Result<Plan, Vec<String>> {
    let Some(song) = &parsed.song else { return Err(vec!["the song has errors".into()]) };
    let root = normalize(path);
    let root_dir = root.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = root.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "song".into());
    let mut placer = Placer { root_dir: root_dir.clone(), places: HashMap::new(), taken: BTreeSet::new() };
    let mut problems = Vec::new();

    // Every file the song reads, as the renderer would find it.
    let reads = reads(song, &root_dir);
    // The song's files first, so a sample never takes a name one of them needs.
    for source in &parsed.sources {
        placer.place(&source.path, None);
    }

    let mut files = BTreeMap::new();
    let mut needs = BTreeSet::new();
    let mut named = BTreeSet::new();
    let mut rewritten = 0;
    let mut written_out = BTreeSet::new();
    for (index, source) in parsed.sources.iter().enumerate() {
        let dir = source.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let here = placer.places[&source.path].clone();
        let here_dir = parent(&here);
        let mut edits: Vec<(usize, usize, usize, String)> = Vec::new();
        let mut inserts: HashMap<usize, Vec<String>> = HashMap::new();
        // The `preset` words taken out of header lines: edits, and not paths.
        let mut headers = 0;
        // Lines without a token are not lexed at all, so a line's place in the
        // file is its tokens' line, never its index among the lexed ones.
        for line in lex_file(&source.text, index, &mut Vec::new()).iter().filter(|l| !l.tokens.is_empty()) {
            let line_index = line.tokens[0].span.line - 1;
            if let Some(preset) = preset_of(line) {
                let tokens = &line.tokens;
                // The paths the preset reads, found as the song's own are.
                let mut packs = false;
                for preset_line in &preset.lines {
                    for token in &preset_line.tokens {
                        let target = normalize(Path::new(&resolve_load_path(&token.text, &root_dir)));
                        if !reads.contains(&target) {
                            continue;
                        }
                        named.insert(target.clone());
                        if token.text.starts_with("samples:") {
                            packs = true;
                        } else if token.text == "gm" || INSTALLED.iter().any(|p| token.text.starts_with(p)) {
                            let missing = if target.exists() { "" } else { ", not installed here either" };
                            needs.insert(format!("{} (preset {}{missing})", token.text, preset.name));
                        }
                    }
                }
                if packs && tokens[0].text == "instrument" {
                    // `instrument drums preset house-kit …` → `instrument drums samples …`,
                    // and the preset's lines under it with their samples in the pack.
                    let (kind_at, name) = (&tokens[2], &tokens[3]);
                    edits.push((line_index, kind_at.span.col - 1, name.span.col - 1 + name.span.len - (kind_at.span.col - 1), preset.kind.clone()));
                    headers += 1;
                    let mut lines = Vec::new();
                    for preset_line in &preset.lines {
                        let Some(text) = crate::presets::source_line(preset_line.tokens[0].span.line) else { continue };
                        let mut line_edits = Vec::new();
                        for token in &preset_line.tokens {
                            let Some(rest) = token.text.strip_prefix("samples:") else { continue };
                            let target = normalize(Path::new(&resolve_load_path(&token.text, &root_dir)));
                            if !target.is_file() {
                                problems.push(format!("preset {}: {} is not there ({})", preset.name, token.text, target.display()));
                                continue;
                            }
                            let place = placer.place(&target, Some(format!("{LIBRARY_FOLDER}/{}", rest.trim_start_matches('/'))));
                            line_edits.push((0, token.span.col - 1, token.span.len, format!("\"{}\"", relative(&here_dir, &place))));
                        }
                        rewritten += line_edits.len();
                        lines.push(apply(text, &line_edits));
                    }
                    inserts.insert(line_index, lines);
                    written_out.insert(format!("{} (instrument {})", preset.name, tokens[1].text));
                }
                continue;
            }
            for token in &line.tokens {
                let text = token.text.as_str();
                if text.is_empty() {
                    continue;
                }
                let target = normalize(Path::new(&resolve_load_path(text, &dir)));
                let is_include = parsed.sources.iter().skip(1).any(|s| s.path == target);
                if !is_include && !reads.contains(&target) {
                    continue;
                }
                named.insert(target.clone());
                if text == "gm" || INSTALLED.iter().any(|p| text.starts_with(p)) {
                    needs.insert(format!("{text}{}", if target.exists() { "" } else { " (not installed here either)" }));
                    continue;
                }
                if !target.is_file() {
                    problems.push(format!("{}:{}: {text} is not there ({})", source.path.display(), line_index + 1, target.display()));
                    continue;
                }
                let library = text.strip_prefix("samples:").map(|rest| format!("{LIBRARY_FOLDER}/{}", rest.trim_start_matches('/')));
                let place = placer.place(&target, library);
                // A word that already names its file from here is left as written.
                let still = !text.starts_with("samples:")
                    && !Path::new(text).is_absolute()
                    && normalize_relative(&format!("{here_dir}/{text}")) == place;
                if still {
                    continue;
                }
                let new = relative(&here_dir, &place);
                let quoted = token.span.len > token.text.chars().count();
                let written = if quoted || new.contains(char::is_whitespace) { format!("\"{new}\"") } else { new };
                edits.push((line_index, token.span.col - 1, token.span.len, written));
            }
        }
        rewritten += edits.len() - headers;
        files.insert(here, Entry::Text(with_inserts(&apply(&source.text, &edits), &inserts)));
    }

    for target in &reads {
        if !named.contains(target) {
            problems.push(format!("{} is read by the song, and no path in it could be found to point into the pack", target.display()));
        }
    }

    // The files: each copied, and an `.exs` of the song's own with the samples
    // it loads beside it, which is where the sampler looks when they are not
    // where the instrument says.
    let placed: Vec<(PathBuf, String)> = placer.places.iter().map(|(p, a)| (p.clone(), a.clone())).collect();
    for (from, place) in placed {
        if parsed.sources.iter().any(|s| s.path == from) {
            continue;
        }
        if from.extension().is_some_and(|e| e.eq_ignore_ascii_case("exs")) {
            match crate::sampler::sample_files(&from) {
                Ok(samples) => {
                    for (name, found) in samples {
                        match found {
                            Some(sample) if crate::sampler::is_installed_sample(&sample) => {
                                needs.insert(format!("{name}, a sample of {}, from Logic's or GarageBand's library", file_name(&from)));
                            }
                            Some(sample) => {
                                let beside = format!("{}{name}", prefix(&parent(&place)));
                                files.entry(beside).or_insert(Entry::Copy(sample));
                            }
                            None => problems.push(format!("{} names {name}, which is not found", from.display())),
                        }
                    }
                }
                Err(error) => problems.push(format!("{}: {error}", from.display())),
            }
        }
        files.insert(place, Entry::Copy(from));
    }

    for instrument in &song.instruments {
        match &instrument.kind {
            InstrumentKind::AudioUnit(au) => {
                needs.insert(format!("the Audio Unit {} (instrument {})", au.component.join(":"), instrument.name));
            }
            InstrumentKind::Clap(clap) => {
                needs.insert(format!("the CLAP plugin {} (instrument {})", clap.plugin, instrument.name));
            }
            _ => {}
        }
    }

    if !problems.is_empty() {
        return Err(problems);
    }
    let song_place = placer.places[&root].clone();
    Ok(Plan { folder: stem, song: song_place, files, needs, rewritten, written_out })
}

/// Writes the pack of the song at `path` to `output`.
pub fn pack(path: &Path, parsed: &Parsed, output: &Path) -> Result<Packed, Vec<String>> {
    let plan = plan(path, parsed)?;
    write(&plan, output).map_err(|e| vec![format!("writing {}: {e}", output.display())])
}

/// Writes a plan as a zip: to a temporary name and renamed, so a pack that
/// fails half-way leaves no half of a zip under the name asked for.
pub fn write(plan: &Plan, output: &Path) -> std::io::Result<Packed> {
    let partial = output.with_extension("zip.partial");
    let result = write_zip(plan, &partial);
    match result {
        Ok(packed) => {
            std::fs::rename(&partial, output)?;
            Ok(packed)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            Err(error)
        }
    }
}

fn write_zip(plan: &Plan, output: &Path) -> std::io::Result<Packed> {
    use zip::write::SimpleFileOptions;
    let file = std::fs::File::create(output)?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let deflated = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
    // Compressed already: deflating them again costs time and saves nothing.
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
    let mut bytes = 0;
    let folder = &plan.folder;
    zip.start_file(format!("{folder}/README.txt"), deflated)?;
    zip.write_all(readme(plan).as_bytes())?;
    for (place, entry) in &plan.files {
        let name = format!("{folder}/{place}");
        match entry {
            Entry::Text(text) => {
                zip.start_file(name, deflated)?;
                zip.write_all(text.as_bytes())?;
                bytes += text.len() as u64;
            }
            Entry::Copy(from) => {
                let packed = from.extension().and_then(|e| e.to_str()).map(str::to_lowercase);
                let options = if matches!(packed.as_deref(), Some("m4a" | "mp3" | "flac" | "ogg" | "aac" | "zip")) { stored } else { deflated };
                zip.start_file(name, options)?;
                bytes += std::io::copy(&mut std::fs::File::open(from)?, &mut zip)?;
            }
        }
    }
    zip.finish()?.flush()?;
    Ok(Packed { files: plan.files.len(), bytes, rewritten: plan.rewritten, needs: plan.needs.iter().cloned().collect() })
}

/// What the pack is, for whoever opens it.
pub fn readme(plan: &Plan) -> String {
    let mut text = format!(
        "{}, packed by mat {}.\n\nRender it with: mat render {}\n\n\
         Every file the song reads is in this folder, and its paths point here:\n\
         files from outside the song's own folder are in {EXTERNAL_FOLDER}/, and the\n\
         samples it takes from mat's library are in {LIBRARY_FOLDER}/.\n",
        plan.folder,
        env!("CARGO_PKG_VERSION"),
        plan.song
    );
    if !plan.written_out.is_empty() {
        text.push_str("\nThese presets are written out in the song, since the pack carries their samples:\n\n");
        for preset in &plan.written_out {
            text.push_str(&format!("  - {preset}\n"));
        }
    }
    if plan.needs.is_empty() {
        text.push_str("\nIt needs nothing installed but mat.\n");
    } else {
        text.push_str("\nIt also needs these, installed where it is played, which are not in the pack:\n\n");
        for need in &plan.needs {
            text.push_str(&format!("  - {need}\n"));
        }
    }
    text
}

/// The preset a header line names: `instrument x preset y` or `master preset y`.
fn preset_of(line: &crate::lexer::Line) -> Option<&'static crate::presets::Preset> {
    let t = &line.tokens;
    if line.indented {
        return None;
    }
    let name = match t.first().map(|t| t.text.as_str()) {
        Some("instrument") if t.len() >= 4 && t[2].text == "preset" => &t[3].text,
        Some("master") if t.len() >= 3 && t[1].text == "preset" => &t[2].text,
        _ => return None,
    };
    crate::presets::library().get(name)
}

/// The text with lines put in after the lines of `inserts`, 0-based, each
/// ending as the line it follows does.
fn with_inserts(text: &str, inserts: &HashMap<usize, Vec<String>>) -> String {
    if inserts.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.split_inclusive('\n').enumerate() {
        out.push_str(line);
        let Some(lines) = inserts.get(&index) else { continue };
        let body = line.trim_end_matches(['\n', '\r']);
        let ending = if body.len() == line.len() { "\n" } else { &line[body.len()..] };
        if body.len() == line.len() {
            out.push('\n');
        }
        for inserted in lines {
            out.push_str(inserted);
            out.push_str(ending);
        }
    }
    out
}

/// Every file the song reads, resolved as the renderer resolves it.
fn reads(song: &Song, root_dir: &Path) -> BTreeSet<PathBuf> {
    let mut paths: Vec<&str> = Vec::new();
    for instrument in &song.instruments {
        match &instrument.kind {
            InstrumentKind::Sampler(def) => paths.push(&def.load),
            InstrumentKind::Samples(def) => paths.extend(def.zones.iter().map(|z| z.path.as_str())),
            InstrumentKind::Scratch(def) if def.source_track.is_none() => paths.push(&def.path),
            InstrumentKind::AudioUnit(def) => paths.extend(def.load.as_deref()),
            InstrumentKind::Clap(def) => paths.extend(def.patch.as_deref()),
            InstrumentKind::Audio(def) => paths.push(&def.path),
            _ => {}
        }
    }
    for track in &song.tracks {
        if let Some((audio, _)) = &track.audio {
            paths.push(&audio.path);
        }
    }
    paths.into_iter().filter(|p| !p.is_empty()).map(|p| normalize(Path::new(&resolve_load_path(p, root_dir)))).collect()
}

/// Gives each file its place in the pack, once.
struct Placer {
    root_dir: PathBuf,
    places: HashMap<PathBuf, String>,
    taken: BTreeSet<String>,
}

impl Placer {
    fn place(&mut self, path: &Path, library: Option<String>) -> String {
        if let Some(place) = self.places.get(path) {
            return place.clone();
        }
        let wanted = library.unwrap_or_else(|| match path.strip_prefix(&self.root_dir) {
            Ok(inside) => slashed(inside),
            Err(_) => format!("{EXTERNAL_FOLDER}/{}", file_name(path)),
        });
        let mut place = wanted.clone();
        let mut count = 2;
        while self.taken.contains(&place) {
            place = numbered(&wanted, count);
            count += 1;
        }
        self.taken.insert(place.clone());
        self.places.insert(path.to_path_buf(), place.clone());
        place
    }
}

/// `external/kick.wav` as `external/kick-2.wav`.
fn numbered(place: &str, count: usize) -> String {
    let (dir, name) = place.rsplit_once('/').map_or(("", place), |(d, n)| (d, n));
    let named = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem}-{count}.{extension}"),
        _ => format!("{name}-{count}"),
    };
    if dir.is_empty() { named } else { format!("{dir}/{named}") }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn slashed(path: &Path) -> String {
    path.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

fn parent(place: &str) -> String {
    place.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default()
}

fn prefix(dir: &str) -> String {
    if dir.is_empty() { String::new() } else { format!("{dir}/") }
}

/// A path in the pack with its `.` and `..` taken out.
fn normalize_relative(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// `to` as a path from the folder `from`, both places in the pack.
fn relative(from: &str, to: &str) -> String {
    let from: Vec<&str> = from.split('/').filter(|p| !p.is_empty()).collect();
    let to: Vec<&str> = to.split('/').filter(|p| !p.is_empty()).collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts = vec![".."; from.len() - shared];
    parts.extend(&to[shared..]);
    parts.join("/")
}

/// The text with each edit made: `(line, column, length, text)`, columns and
/// lengths in characters, as the lexer counts them. Line endings are kept.
fn apply(text: &str, edits: &[(usize, usize, usize, String)]) -> String {
    if edits.is_empty() {
        return text.to_string();
    }
    let mut by_line: HashMap<usize, Vec<&(usize, usize, usize, String)>> = HashMap::new();
    for edit in edits {
        by_line.entry(edit.0).or_default().push(edit);
    }
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let Some(line_edits) = by_line.get_mut(&index) else {
            out.push_str(line);
            continue;
        };
        let body = line.trim_end_matches(['\n', '\r']);
        let ending = &line[body.len()..];
        let mut chars: Vec<char> = body.chars().collect();
        // From the right, so an edit does not move the ones before it.
        line_edits.sort_by_key(|edit| std::cmp::Reverse(edit.1));
        for (_, col, len, new) in line_edits.iter() {
            let start = (*col).min(chars.len());
            let end = (col + len).min(chars.len());
            chars.splice(start..end, new.chars());
        }
        out.extend(chars);
        out.push_str(ending);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// A folder of files under the system's temporary directory, gone after.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("mat-pack-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a scratch folder");
            Scratch(dir)
        }

        fn write(&self, path: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wav(scratch: &Scratch, path: &str) -> PathBuf {
        let path = scratch.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let spec = hound::WavSpec { channels: 1, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..441 {
            writer.write_sample(((i as f32 / 10.0).sin() * 8000.0) as i16).unwrap();
        }
        writer.finalize().unwrap();
        path
    }

    fn unzip(zip: &Path, into: &Path) -> Vec<String> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).unwrap();
        let mut names = Vec::new();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).unwrap();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            let path = into.join(entry.name());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            names.push(entry.name().to_string());
        }
        names.sort();
        names
    }

    /// A song whose kit is in another folder, whose own samples are beside
    /// it, one of them named by an absolute path, and one from mat's library:
    /// packed and unpacked somewhere else, it reads only files in the pack.
    #[test]
    fn a_song_packed_reads_only_what_is_in_the_pack() {
        let scratch = Scratch::new("reads");
        let kick = wav(&scratch, "kits/drums/kick.wav");
        wav(&scratch, "songs/one/loops/hat.wav");
        let absolute = wav(&scratch, "elsewhere/snare.wav");
        scratch.write("kits/drums/kit.song", b"instrument kit samples\n  kick \"kick.wav\"\n  clap \"samples:sonic-pi/drum_snare_hard.wav\"\n");
        let song = scratch.write(
            "songs/one/one.song",
            format!(
                "# One, with a blank line and a comment before anything is rewritten\n\ntitle \"One\"\ntempo 120\ninclude \"../../kits/drums/kit.song\"\r\n\
                 instrument own samples   # its own\n  hat \"loops/hat.wav\"\n  snare \"{}\"\n\
                 instrument piano sampler\n  load \"logic:Pianos/Steinway.exs\"\n\
                 pattern beat grid=1/16\n  kick x...\n  hat ..x.\n  snare ....\n\
                 track drums\n  instrument kit\n  play beat\ntrack own\n  instrument own\n  play beat\n",
                absolute.display()
            )
            .as_bytes(),
        );
        let parsed = crate::parse_file(&song).unwrap();
        let plan = plan(&song, &parsed).expect("the song packs");
        let places: Vec<&str> = plan.files.keys().map(String::as_str).collect();
        assert_eq!(
            places,
            [
                "external/kick.wav",
                "external/kit.song",
                "external/snare.wav",
                "loops/hat.wav",
                "mat-samples/sonic-pi/drum_snare_hard.wav",
                "one.song",
            ]
        );
        let Entry::Text(root) = &plan.files["one.song"] else { panic!("the song is text") };
        assert!(root.contains("include \"external/kit.song\"\r\n"), "the include points into the pack, its line ending kept: {root}");
        assert!(root.contains("  hat \"loops/hat.wav\"\n"), "a path that already points into the pack is left as written");
        assert!(root.contains("  snare \"external/snare.wav\"\n"));
        assert!(root.contains("instrument own samples   # its own\n"), "the rest of the line is kept");
        assert!(root.contains("load \"logic:Pianos/Steinway.exs\""), "an installed instrument is left as it is written");
        let Entry::Text(kit) = &plan.files["external/kit.song"] else { panic!("the kit is text") };
        assert!(kit.contains("  kick \"kick.wav\"\n"), "the kit's sample is beside it in the pack too");
        assert!(kit.contains("  clap \"../mat-samples/sonic-pi/drum_snare_hard.wav\"\n"), "{kit}");
        assert_eq!(plan.rewritten, 3);
        // Said, whether or not this machine has it.
        assert_eq!(plan.needs.len(), 1);
        assert!(plan.needs.iter().all(|n| n.starts_with("logic:Pianos/Steinway.exs")), "{:?}", plan.needs);
        assert!(matches!(&plan.files["external/kick.wav"], Entry::Copy(from) if *from == kick));

        // Unpacked elsewhere, every file the song reads is inside the pack.
        let zip = scratch.0.join("one.zip");
        let packed = write(&plan, &zip).unwrap();
        assert_eq!(packed.files, 6);
        let away = scratch.0.join("away");
        let names = unzip(&zip, &away);
        assert!(names.contains(&"one/README.txt".to_string()) && names.contains(&"one/one.song".to_string()), "{names:?}");
        let unpacked = crate::parse_file(&away.join("one/one.song")).unwrap();
        let song = unpacked.song.as_ref().expect("the unpacked song parses");
        let folder = normalize(&away.join("one"));
        for read in reads(song, &folder) {
            if read.starts_with("/Library") {
                continue;
            }
            assert!(read.starts_with(&folder) && read.is_file(), "{} is outside the pack", read.display());
        }
        let readme = std::fs::read_to_string(away.join("one/README.txt")).unwrap();
        assert!(readme.contains("mat render one.song") && readme.contains("logic:Pianos/Steinway.exs"), "{readme}");
    }

    /// A pack that would still point out of itself is not made: a sample
    /// that is not there is said, with where it is named.
    #[test]
    fn a_missing_sample_is_said_and_nothing_is_packed() {
        let scratch = Scratch::new("missing");
        let song = scratch.write(
            "a.song",
            b"tempo 120\ninstrument kit samples\n  kick \"gone.wav\"\npattern p grid=1/4\n  kick x...\ntrack t\n  instrument kit\n  play p\n",
        );
        let parsed = crate::parse_file(&song).unwrap();
        let problems = pack(&song, &parsed, &scratch.0.join("a.zip")).unwrap_err();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains(":3: gone.wav is not there"), "{problems:?}");
        assert!(!scratch.0.join("a.zip").exists());
    }

    /// A preset that reads library samples is written out, its samples
    /// pointing into the pack; one that reads Logic's is a line and a need;
    /// a synth preset is a line.
    #[test]
    fn a_preset_whose_samples_are_packed_is_written_out() {
        let scratch = Scratch::new("presets");
        let song = scratch.write(
            "p.song",
            b"tempo 120\ninstrument drums preset house-kit   # the kit\n  gain -2\ninstrument piano preset steinway\ninstrument bass preset acid\n\
              pattern beat grid=1/4\n  kick x...\npattern low\n  C2:w |\n\
              track drums\n  instrument drums\n  play beat\ntrack piano\n  instrument piano\n  play low\ntrack bass\n  instrument bass\n  play low\n",
        );
        let parsed = crate::parse_file(&song).unwrap();
        let plan = plan(&song, &parsed).expect("packs");
        let Entry::Text(text) = &plan.files["p.song"] else { panic!() };
        assert!(text.contains("instrument drums samples   # the kit\n  kick    \"mat-samples/sonic-pi/bd_haus.wav\""), "{text}");
        assert!(text.contains("  velocity 14\n  gain -2\n"), "the preset's lines, then the song's own: {text}");
        assert!(text.contains("instrument piano preset steinway\n") && text.contains("instrument bass preset acid\n"), "{text}");
        assert!(plan.files.contains_key("mat-samples/sonic-pi/drum_tom_lo_hard.wav"));
        assert!(plan.needs.iter().any(|n| n.starts_with("logic:01 Acoustic Pianos/Steinway Grand Piano 2.exs (preset steinway")), "{:?}", plan.needs);
        assert_eq!(plan.written_out.iter().collect::<Vec<_>>(), ["house-kit (instrument drums)"]);

        // The pack's song parses to the same instrument the preset made.
        let packed = crate::parse_with(text, &song, &|p| std::fs::read_to_string(p)).song.expect("the written-out song parses");
        let original = parsed.song.as_ref().unwrap();
        let zones = |s: &Song| match &s.instruments[0].kind {
            InstrumentKind::Samples(def) => (def.zones.len(), def.settings.gain_db),
            _ => panic!("drums are samples"),
        };
        assert_eq!(zones(&packed), zones(original));
    }

    /// Two files of one name from two folders do not overwrite each other.
    #[test]
    fn two_files_of_one_name_from_outside_both_go_in() {
        let scratch = Scratch::new("names");
        let a = wav(&scratch, "x/kick.wav");
        let b = wav(&scratch, "y/kick.wav");
        let song = scratch.write(
            "s/a.song",
            format!(
                "tempo 120\ninstrument kit samples\n  kick \"{}\"\n  snare \"{}\"\npattern p grid=1/4\n  kick x...\n  snare .x..\ntrack t\n  instrument kit\n  play p\n",
                a.display(),
                b.display()
            )
            .as_bytes(),
        );
        let parsed = crate::parse_file(&song).unwrap();
        let plan = plan(&song, &parsed).expect("packs");
        let Entry::Text(text) = &plan.files["a.song"] else { panic!() };
        assert!(text.contains("kick \"external/kick.wav\"") && text.contains("snare \"external/kick-2.wav\""), "{text}");
    }

    #[test]
    fn paths_between_places_in_the_pack() {
        assert_eq!(relative("", "external/kit.song"), "external/kit.song");
        assert_eq!(relative("external", "mat-samples/a/b.wav"), "../mat-samples/a/b.wav");
        assert_eq!(relative("parts/verse", "parts/kick.wav"), "../kick.wav");
        assert_eq!(normalize_relative("parts/../loops/./hat.wav"), "loops/hat.wav");
        assert_eq!(numbered("external/kick.wav", 2), "external/kick-2.wav");
        assert_eq!(numbered("README", 3), "README-3");
    }
}
