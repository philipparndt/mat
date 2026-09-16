//! Language server for `.song` files.
//!
//! Diagnostics as you type (the parser's and the arranger's, the same ones
//! `mat check` prints), completion of the keywords and settings a block takes
//! and of the names a song has defined, hover on keywords and on names,
//! go-to-definition for instruments, patterns and tracks, and the blocks as
//! document symbols.
//!
//! The analysis is pure functions over the text — `completions`, `hover`,
//! `definition`, `symbols`, `diagnostics` — so it is tested without a
//! transport; `Server` keeps the open documents and the songs they are in,
//! and `run_stdio` wraps it in the protocol.
//!
//! **A song is several files.** A song that includes others is analysed as
//! one, through a loader that reads an open document's text before the disk.
//! Its diagnostics are published to the file each is in, a `mat/timeline` is
//! sent for every one of its files, and a name is defined wherever in the song
//! it is. An included file opened on its own is analysed through a song that
//! includes it: an open one, or else one found in the workspace.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _, PublishDiagnostics};
use lsp_types::request::{Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, Request as _};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionResponse, CompletionTextEdit, Diagnostic, DiagnosticSeverity,
    DocumentSymbol, DocumentSymbolResponse, GotoDefinitionResponse, Hover, HoverContents, HoverProviderCapability, InitializeParams,
    Location, MarkupContent, MarkupKind, OneOf, Position, PublishDiagnosticsParams, Range, ServerCapabilities, SymbolKind,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Url,
};
use mat_core::diag::{Severity, Span};
use mat_core::lexer::{Line, lex_file};
use mat_core::model::Song;
use mat_core::parser::{identity, normalize};

pub mod docs;

// MARK: - What the text says

/// A file of a song: its path, its text, and its lines of tokens, one entry
/// per source line.
#[derive(Clone)]
pub struct SourceFile {
    /// Empty for a document analysed without one.
    pub path: PathBuf,
    pub text: String,
    pub lines: Vec<Line>,
}

impl SourceFile {
    fn of(path: PathBuf, text: &str, file: usize) -> SourceFile {
        SourceFile { path, text: text.to_string(), lines: line_slots(text, file) }
    }
}

/// One entry per source line, blank and comment lines included as lines with
/// no tokens: the lexer leaves those out, and everything here is asked by the
/// line the cursor is on.
fn line_slots(text: &str, file: usize) -> Vec<Line> {
    let mut lex_diags = Vec::new();
    let count = text.lines().count();
    let mut lines: Vec<Line> = (0..count).map(|_| Line { indented: false, tokens: Vec::new() }).collect();
    for line in lex_file(text, file, &mut lex_diags) {
        if let Some(first) = line.tokens.first() {
            let index = first.span.line.saturating_sub(1);
            if index < count {
                lines[index] = line;
            }
        }
    }
    lines
}

/// A song as the server sees it, looked at from one of its files: every
/// file's text and lines, and the song when it parses — or the last one that
/// did, so names keep completing while a line is half typed.
#[derive(Clone)]
pub struct Analysis {
    /// The file being looked at, an index into `files`: the one whose text,
    /// lines, completions and symbols the functions here answer for.
    pub file: usize,
    /// The song first, then each file it includes, as `Span::file` counts.
    pub files: Vec<SourceFile>,
    pub song: Option<Song>,
    /// Every file's, each span saying which.
    pub diagnostics: Vec<mat_core::Diagnostic>,
    /// Whether `song` is this text's, rather than the last one that parsed.
    pub parsed: bool,
}

impl Analysis {
    /// A document with no path: an `include` in it is a diagnostic.
    pub fn of(text: &str, previous: Option<Song>) -> Analysis {
        let (song, diagnostics) = mat_core::parse(text);
        Self::arranged(vec![SourceFile::of(PathBuf::new(), text, 0)], song, diagnostics, previous)
    }

    /// The song at `path`, with the files it includes read by `loader`.
    pub fn of_song(text: &str, path: &Path, loader: mat_core::parser::Loader, previous: Option<Song>) -> Analysis {
        let parsed = mat_core::parse_with(text, path, loader);
        let files = parsed.sources.iter().enumerate().map(|(i, s)| SourceFile::of(s.path.clone(), &s.text, i)).collect();
        Self::arranged(files, parsed.song, parsed.diagnostics, previous)
    }

    fn arranged(files: Vec<SourceFile>, song: Option<Song>, mut diagnostics: Vec<mat_core::Diagnostic>, previous: Option<Song>) -> Analysis {
        if let Some(song) = &song
            && let Err(errors) = mat_core::arrange(song)
        {
            diagnostics.extend(errors);
        }
        let parsed = song.is_some();
        Analysis { file: 0, files, song: song.or(previous), diagnostics, parsed }
    }

    /// The same analysis, looked at from another of its files.
    pub fn at(mut self, file: usize) -> Analysis {
        self.file = file.min(self.files.len().saturating_sub(1));
        self
    }

    /// The text of the file being looked at.
    pub fn text(&self) -> &str {
        &self.files[self.file].text
    }

    /// The lines of the file being looked at.
    pub fn lines(&self) -> &[Line] {
        &self.files[self.file].lines
    }

    fn line_text(&self, line: usize) -> &str {
        self.text().lines().nth(line).unwrap_or("")
    }

    fn instrument_names(&self) -> Vec<&str> {
        self.song.as_ref().map(|s| s.instruments.iter().map(|i| i.name.as_str()).collect()).unwrap_or_default()
    }

    fn pattern_names(&self) -> Vec<&str> {
        self.song.as_ref().map(|s| s.patterns.iter().map(|p| p.name.as_str()).collect()).unwrap_or_default()
    }

    fn track_names(&self) -> Vec<&str> {
        self.song.as_ref().map(|s| s.tracks.iter().map(|t| t.name.as_str()).collect()).unwrap_or_default()
    }

    fn layer_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for track in self.song.as_ref().map(|s| s.tracks.as_slice()).unwrap_or_default() {
            let layer = track.layer.clone().unwrap_or_else(|| track.name.clone());
            if !names.contains(&layer) {
                names.push(layer);
            }
        }
        names
    }
}

/// The block a line is in, read off the nearest header above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// Column one, or before any block.
    Top,
    /// `instrument <name> <kind>`; for `instrument <name> preset <p>` the
    /// kind is the preset's own.
    Instrument { kind: Option<String> },
    Pattern,
    Track,
    Master,
}

/// The header keywords that start a block.
const BLOCK_KEYWORDS: &[&str] = &["instrument", "pattern", "track", "master"];

/// The block the cursor's line belongs to. A line at column one is `Top`
/// even inside a block: what completes there is a new header.
pub fn block_at(lines: &[Line], line: usize) -> Block {
    let indented = lines.get(line).map(|l| l.indented && !l.tokens.is_empty()).unwrap_or(false);
    let blank = lines.get(line).map(|l| l.tokens.is_empty()).unwrap_or(true);
    if !indented && !blank {
        return Block::Top;
    }
    let mut at = line;
    loop {
        if at == 0 {
            return Block::Top;
        }
        at -= 1;
        let Some(candidate) = lines.get(at) else { continue };
        if candidate.indented || candidate.tokens.is_empty() {
            continue;
        }
        let keyword = candidate.tokens[0].text.as_str();
        return match keyword {
            "instrument" => {
                let kind = candidate.tokens.get(2).map(|t| t.text.clone());
                // A preset instrument takes the settings of the kind the preset is.
                let kind = match kind.as_deref() {
                    Some("preset") => candidate.tokens.get(3).and_then(|t| mat_core::presets::library().get(&t.text)).map(|p| p.kind.clone()),
                    _ => kind,
                };
                Block::Instrument { kind }
            }
            "pattern" => Block::Pattern,
            "track" => Block::Track,
            "master" => Block::Master,
            // A song setting between blocks: nothing is open.
            _ => Block::Top,
        };
    }
}

// MARK: - Positions

/// A UTF-16 column into a line, as a character index.
fn char_index(line: &str, utf16: u32) -> usize {
    let mut units = 0u32;
    for (index, character) in line.chars().enumerate() {
        if units >= utf16 {
            return index;
        }
        units += character.len_utf16() as u32;
    }
    line.chars().count()
}

/// A character index in a line, as a UTF-16 column.
fn utf16_column(line: &str, chars: usize) -> u32 {
    line.chars().take(chars).map(|c| c.len_utf16() as u32).sum()
}

/// A span of the song, in the protocol's terms.
pub fn range(text: &str, span: Span) -> Range {
    let line = text.lines().nth(span.line.saturating_sub(1)).unwrap_or("");
    let start = span.col.saturating_sub(1);
    let end = start + span.len.max(1);
    let row = span.line.saturating_sub(1) as u32;
    Range { start: Position::new(row, utf16_column(line, start)), end: Position::new(row, utf16_column(line, end)) }
}

// MARK: - Diagnostics


/// The diagnostics of the file being looked at.
pub fn diagnostics(analysis: &Analysis) -> Vec<Diagnostic> {
    diagnostics_in(analysis, analysis.file)
}

/// The diagnostics of one file of the song, by its index.
pub fn diagnostics_in(analysis: &Analysis, file: usize) -> Vec<Diagnostic> {
    let text = analysis.files.get(file).map_or("", |f| f.text.as_str());
    analysis
        .diagnostics
        .iter()
        .filter(|d| d.span.file == file)
        .map(|d| Diagnostic {
            range: range(text, d.span),
            severity: Some(match d.severity {
                Severity::Error => DiagnosticSeverity::ERROR,
                Severity::Warning => DiagnosticSeverity::WARNING,
            }),
            source: Some("mat".into()),
            message: match &d.hint {
                Some(hint) => format!("{}\n{hint}", d.message),
                None => d.message.clone(),
            },
            ..Default::default()
        })
        .collect()
}

// MARK: - Completion

/// What is being typed: the words before the cursor on its line, and the
/// prefix of the one under it.
struct Typing {
    indented: bool,
    /// Whole words before the one being typed.
    before: Vec<String>,
    /// The word under the cursor so far; empty at a word boundary.
    prefix: String,
}

fn typing(line: &str, character: u32) -> Typing {
    let upto: String = line.chars().take(char_index(line, character)).collect();
    let indented = upto.starts_with(char::is_whitespace);
    let mut words: Vec<String> = upto.split_whitespace().map(String::from).collect();
    let prefix = if upto.ends_with(char::is_whitespace) || words.is_empty() { String::new() } else { words.pop().unwrap_or_default() };
    Typing { indented, before: words, prefix }
}

fn item(label: &str, kind: CompletionItemKind, detail: Option<&str>) -> CompletionItem {
    CompletionItem { label: label.to_string(), kind: Some(kind), detail: detail.map(String::from), ..Default::default() }
}

fn keyword_items(entries: &[(&str, &str)]) -> Vec<CompletionItem> {
    entries.iter().map(|(word, doc)| item(word, CompletionItemKind::KEYWORD, Some(doc))).collect()
}

fn option_items(keys: &[&str]) -> Vec<CompletionItem> {
    keys.iter().map(|key| item(&format!("{key}="), CompletionItemKind::PROPERTY, None)).collect()
}

fn name_items<'a>(names: impl IntoIterator<Item = &'a str>, kind: CompletionItemKind, what: &str) -> Vec<CompletionItem> {
    names.into_iter().map(|name| item(name, kind, Some(what))).collect()
}

fn preset_items(kind_filter: impl Fn(&str) -> bool) -> Vec<CompletionItem> {
    let mut presets: Vec<_> = mat_core::presets::library().all().filter(|p| kind_filter(&p.kind)).collect();
    presets.sort_by(|a, b| a.name.cmp(&b.name));
    presets.iter().map(|p| item(&p.name, CompletionItemKind::VALUE, Some(&format!("{} — {}", p.kind, p.description)))).collect()
}

/// What could go at the cursor.
pub fn completions(analysis: &Analysis, position: Position) -> Vec<CompletionItem> {
    let line = position.line as usize;
    let text = analysis.line_text(line);
    let typing = typing(text, position.character);
    let index = typing.before.len();
    let word = |i: usize| typing.before.get(i).map(String::as_str).unwrap_or("");

    let mut items: Vec<CompletionItem> = if !typing.indented {
        match (word(0), index) {
            (_, 0) => keyword_items(docs::TOP_LEVEL),
            ("include", 1) => return include_completions(analysis, position, &typing),
            ("instrument", 2) => keyword_items(docs::INSTRUMENT_KINDS),
            ("instrument", 3) if word(2) == "preset" => preset_items(|kind| kind != "master"),
            ("master", 1) => keyword_items(&[("preset", "Start from a built-in master chain, then override settings below.")]),
            ("master", 2) if word(1) == "preset" => preset_items(|kind| kind == "master"),
            ("pattern", i) if i >= 2 => keyword_items(docs::PATTERN_HEADER),
            ("section", i) if i >= 2 => option_items(&["bars"]),
            ("meter", 1) => ["4/4", "3/4", "6/8", "2/4"].iter().map(|m| item(m, CompletionItemKind::VALUE, None)).collect(),
            ("swing", 1) => keyword_items(&[("0.5", "straight"), ("0.58", "a light swing"), ("0.66", "triplet feel")]),
            _ => Vec::new(),
        }
    } else {
        match block_at(analysis.lines(), line) {
            Block::Top => Vec::new(),
            Block::Instrument { kind } => instrument_completions(analysis, kind.as_deref(), &typing),
            Block::Pattern => {
                if index == 0 {
                    let mut items = keyword_items(docs::DRUMS);
                    items.extend(keyword_items(docs::SCRATCH_MOVES));
                    items
                } else {
                    Vec::new()
                }
            }
            Block::Track => track_completions(analysis, &typing),
            Block::Master => match (word(0), index) {
                (_, 0) => keyword_items(docs::MASTER_SETTINGS),
                (setting, _) => option_items(docs::options(setting, "master")),
            },
        }
    };

    if !typing.prefix.is_empty() {
        let prefix = typing.prefix.to_lowercase();
        items.retain(|item| item.label.to_lowercase().starts_with(&prefix));
    }
    items
}

/// The `.song` files under the folder of the file being typed in, as paths
/// from that folder, for `include "`. The whole string is replaced, quotes
/// and all, so it does not matter what an editor takes a word to be.
fn include_completions(analysis: &Analysis, position: Position, typing: &Typing) -> Vec<CompletionItem> {
    let own = &analysis.files[analysis.file].path;
    let Some(dir) = own.parent().filter(|_| !own.as_os_str().is_empty()) else { return Vec::new() };
    let text = analysis.line_text(position.line as usize);
    let cursor = char_index(text, position.character);
    let start = cursor.saturating_sub(typing.prefix.chars().count());
    let typed = typing.prefix.trim_start_matches('"').to_lowercase();
    let closed = text.chars().nth(cursor) == Some('"');
    let edit_range = Range { start: Position::new(position.line, utf16_column(text, start)), end: Position::new(position.line, utf16_column(text, cursor + usize::from(closed))) };
    let mut found = Vec::new();
    song_files(dir, 4, &mut found);
    found.sort();
    let own_identity = identity(own);
    found
        .into_iter()
        .filter(|path| identity(path) != own_identity)
        .filter_map(|path| {
            let relative = path.strip_prefix(dir).ok()?.to_string_lossy().replace('\\', "/");
            if !relative.to_lowercase().starts_with(&typed) {
                return None;
            }
            let quoted = format!("\"{relative}\"");
            Some(CompletionItem {
                label: relative.clone(),
                kind: Some(CompletionItemKind::FILE),
                filter_text: Some(format!("\"{relative}")),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit { range: edit_range, new_text: quoted })),
                ..Default::default()
            })
        })
        .collect()
}

/// The `.song` files under a folder, `depth` folders down, leaving out
/// hidden folders and the ones tools fill: `target`, `node_modules`.
fn song_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    const MOST: usize = 5000;
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if out.len() >= MOST {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if depth > 0 && !name.starts_with('.') && name != "target" && name != "node_modules" {
                song_files(&path, depth - 1, out);
            }
        } else if path.extension().is_some_and(|e| e == "song") {
            out.push(path);
        }
    }
}

fn instrument_completions(analysis: &Analysis, kind: Option<&str>, typing: &Typing) -> Vec<CompletionItem> {
    let index = typing.before.len();
    let word = |i: usize| typing.before.get(i).map(String::as_str).unwrap_or("");
    let kind = kind.unwrap_or("synth");
    if index == 0 {
        return keyword_items(docs::instrument_settings(kind));
    }
    match (kind, word(0), index) {
        (_, "osc", 1) => keyword_items(docs::WAVES),
        (_, "filter", 1) => keyword_items(docs::FILTER_MODES),
        (_, "lfo", 1) => keyword_items(docs::LFO_TARGETS),
        ("tb303", "wave", 1) => keyword_items(&[("saw", ""), ("square", "")]),
        ("scratch", "pitch", 1) => keyword_items(&[("keep", "time-stretch: the same pitch at any speed"), ("follow", "like vinyl: pitch follows speed")]),
        ("scratch", "source", 1) => {
            let mut items = name_items(analysis.track_names(), CompletionItemKind::REFERENCE, "track");
            items.push(item("mix", CompletionItemKind::VALUE, Some("everything but scratches")));
            items
        }
        (_, setting, _) => option_items(docs::options(setting, kind)),
    }
}

fn track_completions(analysis: &Analysis, typing: &Typing) -> Vec<CompletionItem> {
    let index = typing.before.len();
    let word = |i: usize| typing.before.get(i).map(String::as_str).unwrap_or("");
    match (word(0), index) {
        (_, 0) => keyword_items(docs::TRACK_SETTINGS),
        ("instrument", 1) => name_items(analysis.instrument_names(), CompletionItemKind::REFERENCE, "instrument"),
        ("play", 1) => {
            let mut items = name_items(analysis.pattern_names(), CompletionItemKind::REFERENCE, "pattern");
            items.push(item("all", CompletionItemKind::VALUE, Some("an audio track: the whole file, bar 1 aligned")));
            items
        }
        ("play", _) => {
            let mut items = option_items(&["transpose", "vel", "bars"]);
            items.push(item("x2", CompletionItemKind::VALUE, Some("repeat; any count")));
            items
        }
        ("sidechain", 1) => name_items(analysis.track_names(), CompletionItemKind::REFERENCE, "track"),
        ("layer", 1) => name_items(analysis.layer_names().iter().map(String::as_str), CompletionItemKind::VALUE, "layer"),
        ("sweep", 1) => keyword_items(docs::SWEEP_PARAMETERS),
        (setting, _) => option_items(docs::options(setting, "track")),
    }
}

// MARK: - Names and where they are defined

/// A top-level line's kind, name and the name's span — which says its file —
/// and its line index, for each block that has a name, in every file of the
/// song: the file being looked at first. Off the lexer, so it holds while the
/// song does not parse.
fn definitions(analysis: &Analysis) -> Vec<(&str, &str, Span, usize)> {
    let order = std::iter::once(analysis.file).chain((0..analysis.files.len()).filter(|f| *f != analysis.file));
    order
        .flat_map(|file| analysis.files[file].lines.iter().enumerate())
        .filter(|(_, l)| !l.indented && l.tokens.len() >= 2 && BLOCK_KEYWORDS.contains(&l.tokens[0].text.as_str()))
        .map(|(index, l)| (l.tokens[0].text.as_str(), l.tokens[1].text.as_str(), l.tokens[1].span, index))
        .collect()
}

/// The token under a position, with its index on the line.
fn token_at(analysis: &Analysis, position: Position) -> Option<(usize, &mat_core::lexer::Token)> {
    let line = analysis.lines().get(position.line as usize)?;
    let text = analysis.line_text(position.line as usize);
    let at = char_index(text, position.character) + 1;
    line.tokens.iter().enumerate().find(|(_, t)| t.span.col <= at && at <= t.span.col + t.span.len)
}

/// What kind of thing the word under the cursor refers to, when it is a
/// reference: that word and nothing beside it. What a hover explains.
fn referent(analysis: &Analysis, position: Position) -> Option<(&'static str, String)> {
    let (at, _) = token_at(analysis, position)?;
    referent_at(analysis, position, at)
}

/// What the cursor's line refers to, counting the line's keyword as the name
/// beside it: "play" is as much a part of `play chords` as the pattern's name
/// is, and a jump from the keyword has nowhere else to go. A hover stays on
/// the word it points at, which has the keyword's own documentation to show.
/// Asked 2026-09-16, of a click that did nothing.
fn followed_referent(analysis: &Analysis, position: Position) -> Option<(&'static str, String)> {
    let (at, _) = token_at(analysis, position)?;
    let line = analysis.lines().get(position.line as usize)?;
    referent_at(analysis, position, if at == 0 && line.tokens.len() >= 2 { 1 } else { at })
}

/// What kind of thing the token at `index` on the cursor's line refers to.
fn referent_at(analysis: &Analysis, position: Position, index: usize) -> Option<(&'static str, String)> {
    let line = analysis.lines().get(position.line as usize)?;
    let token = line.tokens.get(index)?;
    let first = line.tokens.first()?.text.as_str();
    let block = block_at(analysis.lines(), position.line as usize);
    let kind = match (block, first, index) {
        (Block::Track, "instrument", 1) => "instrument",
        (Block::Track, "play", 1) => "pattern",
        (Block::Track, "sidechain", 1) => "track",
        (Block::Instrument { .. }, "source", 1) => "track",
        (Block::Master, "sidechain", 1) => "track",
        _ => return None,
    };
    Some((kind, token.text.clone()))
}

/// The file of the song an `include` line under the cursor names.
fn included_at(analysis: &Analysis, position: Position) -> Option<usize> {
    let (at, _) = token_at(analysis, position)?;
    let line = analysis.lines().get(position.line as usize)?;
    // Either half of `include "kit.song"`: the word or the name.
    if line.indented || at > 1 || line.tokens.len() < 2 || line.tokens[0].text != "include" {
        return None;
    }
    let token = line.tokens.get(1)?;
    let dir = analysis.files[analysis.file].path.parent()?;
    let wanted = identity(&normalize(&dir.join(&token.text)));
    analysis.files.iter().position(|f| !f.path.as_os_str().is_empty() && identity(&f.path) == wanted)
}

/// Where the name under the cursor is defined: the file of the song, by its
/// index, and the name's range in it. On an `include` line, the start of the
/// file it names.
pub fn definition(analysis: &Analysis, position: Position) -> Option<(usize, Range)> {
    if let Some(file) = included_at(analysis, position) {
        return Some((file, Range::default()));
    }
    let (kind, name) = followed_referent(analysis, position)?;
    definitions(analysis)
        .into_iter()
        .find(|(k, n, _, _)| *k == kind && *n == name)
        .map(|(_, _, span, _)| (span.file, range(&analysis.files[span.file].text, span)))
}

// MARK: - Hover

/// What the word under the cursor means: the keyword's documentation, or a
/// named thing's own lines, from the file they are in.
pub fn hover(analysis: &Analysis, position: Position) -> Option<(String, Range)> {
    let (index, token) = token_at(analysis, position)?;
    let token_range = range(analysis.text(), token.span);

    if let Some((kind, name)) = referent(analysis, position) {
        let (_, _, span, line) = definitions(analysis).into_iter().find(|(k, n, _, _)| *k == kind && *n == name)?;
        let file = &analysis.files[span.file];
        let text = |line: usize| file.text.lines().nth(line).unwrap_or("");
        let mut shown: Vec<&str> = vec![text(line)];
        for (offset, following) in file.lines.iter().enumerate().skip(line + 1) {
            if !following.indented && !following.tokens.is_empty() {
                break;
            }
            if shown.len() >= 10 {
                shown.push("  …");
                break;
            }
            shown.push(text(offset));
        }
        while shown.last().is_some_and(|l| l.trim().is_empty()) {
            shown.pop();
        }
        return Some((format!("```song\n{}\n```", shown.join("\n")), token_range));
    }

    let line = analysis.lines().get(position.line as usize)?;
    let first = line.tokens.first()?.text.as_str();
    let block = block_at(analysis.lines(), position.line as usize);
    // The brackets of a loop say what the loop is.
    match (&block, token.text.as_str()) {
        (Block::Pattern, text) if text == "(" || text.starts_with(')') => return Some((format!("**{}** — {}", docs::GROUP.0, docs::GROUP.1), token_range)),
        (Block::Track, "{" | "}") => {
            let doc = docs::find(docs::TRACK_SETTINGS, "repeat")?;
            return Some((format!("**repeat** — {doc}"), token_range));
        }
        _ => {}
    }
    let lookup = |word: &str| -> Option<&'static str> {
        match &block {
            Block::Top => docs::find(docs::TOP_LEVEL, word).or_else(|| docs::find(docs::INSTRUMENT_KINDS, word)),
            Block::Instrument { kind } => {
                let kind = kind.as_deref().unwrap_or("synth");
                docs::find(docs::instrument_settings(kind), word)
                    .or_else(|| docs::find(docs::WAVES, word))
                    .or_else(|| docs::find(docs::FILTER_MODES, word))
                    .or_else(|| docs::find(docs::LFO_TARGETS, word))
            }
            Block::Pattern => docs::find(docs::DRUMS, word).or_else(|| docs::find(docs::SCRATCH_MOVES, word)).or_else(|| docs::find(docs::PATTERN_HEADER, word)),
            Block::Track => docs::find(docs::TRACK_SETTINGS, word).or_else(|| docs::find(docs::SWEEP_PARAMETERS, word)),
            Block::Master => docs::find(docs::MASTER_SETTINGS, word),
        }
    };
    // The word under the cursor when it is a keyword of its own — a wave, a
    // filter mode — and otherwise the line's setting, whose documentation
    // names the option being pointed at.
    let word = token.text.split('=').next().unwrap_or("");
    if let Some(doc) = lookup(word) {
        return Some((format!("**{word}** — {doc}"), token_range));
    }
    if index > 0 {
        let doc = lookup(first)?;
        return Some((format!("**{first}** — {doc}"), token_range));
    }
    None
}

// MARK: - Symbols

/// The blocks and sections of the file being looked at, in order — not the
/// ones of the files it includes, which are theirs.
pub fn symbols(analysis: &Analysis) -> Vec<DocumentSymbol> {
    let lines = analysis.lines();
    let headers: Vec<(usize, &Line)> = lines.iter().enumerate().filter(|(_, l)| !l.indented && !l.tokens.is_empty()).collect();
    let mut out = Vec::new();
    for (position, (line, header)) in headers.iter().enumerate() {
        let keyword = header.tokens[0].text.as_str();
        let (kind, name) = match keyword {
            "instrument" => (SymbolKind::CLASS, header.tokens.get(1).map(|t| t.text.clone())),
            "pattern" => (SymbolKind::ARRAY, header.tokens.get(1).map(|t| t.text.clone())),
            "track" => (SymbolKind::FUNCTION, header.tokens.get(1).map(|t| t.text.clone())),
            "master" => (SymbolKind::MODULE, Some(String::from("master"))),
            "section" => (SymbolKind::EVENT, header.tokens.get(1).map(|t| t.text.clone())),
            _ => continue,
        };
        let Some(name) = name else { continue };
        // The block runs to the line before the next header, trailing blank
        // lines left out.
        let mut end = headers.get(position + 1).map(|(next, _)| next.saturating_sub(1)).unwrap_or(lines.len().saturating_sub(1));
        while end > *line && lines.get(end).is_some_and(|l| l.tokens.is_empty()) {
            end -= 1;
        }
        let end_text = analysis.line_text(end);
        let full = Range {
            start: Position::new(*line as u32, 0),
            end: Position::new(end as u32, utf16_column(end_text, end_text.chars().count())),
        };
        let name_span = header.tokens.get(1).map(|t| t.span).unwrap_or(header.tokens[0].span);
        let detail = match keyword {
            "instrument" => header.tokens.get(2).map(|t| t.text.clone()),
            "section" => header.tokens.get(2).map(|t| t.text.clone()),
            "pattern" => header.tokens.get(2).map(|t| t.text.clone()),
            _ => None,
        };
        #[allow(deprecated)]
        out.push(DocumentSymbol {
            name,
            detail,
            kind,
            tags: None,
            deprecated: None,
            range: full,
            selection_range: range(analysis.text(), name_span),
            children: None,
        });
    }
    out
}

// MARK: - Where lines are heard

/// The notification an editor is sent after each analysis that parsed: for
/// every line heard somewhere, the stretches of the song where, in seconds.
/// One for each file of the song, each with that file's lines.
pub const TIMELINE: &str = "mat/timeline";

/// `mat/timeline`'s parameters for every file of the song, in file order, or
/// nil when this text did not parse — lines placed from the last song that
/// did would be drawn beside lines that have since moved.
///
/// `uris` names the song's files by index, the song first. Each message is
/// for one file — `uri`, and `lines` in it — and says the song it is part of
/// (`song`, `files`), and the song's length, bar and tracks, whose `file`,
/// `instrumentFile`, and plays' `file` and `patternFile` are indexes into
/// `files`.
pub fn timelines(analysis: &Analysis, uris: &[Url]) -> Option<Vec<serde_json::Value>> {
    if !analysis.parsed || uris.len() < analysis.files.len() {
        return None;
    }
    let placements = mat_core::placement::placements(analysis.song.as_ref()?);
    // Lines 0-based, as everywhere on the wire.
    let tracks: Vec<serde_json::Value> = placements
        .tracks
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "file": t.file,
                "line": t.line.saturating_sub(1),
                "layer": t.layer,
                "instrument": t.instrument,
                "instrumentFile": t.instrument_file,
                "instrumentLine": t.instrument_line.map(|l| l.saturating_sub(1)),
                "plays": t.plays.iter().map(|p| serde_json::json!({
                    "file": p.file,
                    "line": p.line.saturating_sub(1),
                    "pattern": p.pattern,
                    "patternFile": p.pattern_file,
                    "patternLine": p.pattern_line.map(|l| l.saturating_sub(1)),
                    "start": p.start,
                    "end": p.end,
                    "pass": p.pass_seconds,
                    "transpose": p.transpose,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let files = &uris[..analysis.files.len()];
    let messages = analysis
        .files
        .iter()
        .enumerate()
        .map(|(file, source)| {
            let lines: Vec<serde_json::Value> = placements
                .lines
                .iter()
                .filter(|l| l.file == file)
                .map(|l| {
                    let mut entry = serde_json::json!({ "line": l.line.saturating_sub(1), "spans": l.spans.iter().map(|s| [s.0, s.1]).collect::<Vec<_>>() });
                    if !l.notes.is_empty() {
                        // Columns as the protocol counts them: 0-based UTF-16 units, the
                        // note's first and the one after its last.
                        let text = source.text.lines().nth(l.line.saturating_sub(1)).unwrap_or("");
                        entry["passes"] = serde_json::json!(l.passes);
                        entry["notes"] = l
                            .notes
                            .iter()
                            .map(|n| {
                                let from = utf16_column(text, n.col.saturating_sub(1));
                                let to = utf16_column(text, n.col.saturating_sub(1) + n.len);
                                serde_json::json!([n.start, n.end, from, to])
                            })
                            .collect();
                    }
                    entry
                })
                .collect();
            serde_json::json!({
                "uri": files[file],
                "song": files[0],
                "files": files,
                "seconds": placements.seconds,
                "barSeconds": placements.bar_seconds,
                "lines": lines,
                "tracks": tracks,
            })
        })
        .collect();
    Some(messages)
}

// MARK: - The server

/// A song the server has analysed, and the URI of each of its files.
struct Analysed {
    analysis: Analysis,
    uris: Vec<Url>,
}

/// The open documents and the songs they are in, answering the protocol's
/// notifications with the notifications to send back, and its requests.
pub struct Server {
    /// Folders to look for a song in, for an included file opened on its own.
    workspace: Vec<PathBuf>,
    /// The text of each open document.
    open: HashMap<Url, String>,
    /// Every song analysed, by its own URI.
    songs: HashMap<Url, Analysed>,
    /// The song each open document is analysed in — its own URI when it is one.
    home: HashMap<Url, Url>,
}

impl Server {
    pub fn new(workspace: Vec<PathBuf>) -> Server {
        Server { workspace, open: HashMap::new(), songs: HashMap::new(), home: HashMap::new() }
    }

    /// What to send after a notification from the editor.
    pub fn notification(&mut self, notification: &Notification) -> Vec<Notification> {
        if let Some((uri, text)) = document_change(notification) {
            return self.changed(uri, text);
        }
        if notification.method == DidCloseTextDocument::METHOD
            && let Ok(params) = serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(notification.params.clone())
        {
            return self.closed(params.text_document.uri);
        }
        Vec::new()
    }

    fn changed(&mut self, uri: Url, text: String) -> Vec<Notification> {
        self.open.insert(uri.clone(), text);
        let mut songs: Vec<Url> = self.songs.iter().filter(|(_, s)| s.uris.contains(&uri)).map(|(song, _)| song.clone()).collect();
        if songs.is_empty() {
            songs.push(self.find_song(&uri));
        }
        songs.sort();
        let mut out = Vec::new();
        for song in songs {
            self.analyse(&song, &mut out);
        }
        out
    }

    fn closed(&mut self, uri: Url) -> Vec<Notification> {
        let mut out = Vec::new();
        self.open.remove(&uri);
        self.home.remove(&uri);
        let mut songs: Vec<Url> = self.songs.iter().filter(|(_, s)| s.uris.contains(&uri)).map(|(song, _)| song.clone()).collect();
        songs.sort();
        if songs.is_empty() {
            out.push(publish(uri, Vec::new()));
        }
        for song in songs {
            if self.home.values().any(|home| *home == song) {
                // Still open elsewhere: the closed file is read from the disk now.
                self.analyse(&song, &mut out);
            } else {
                self.forget(&song, &mut out);
            }
        }
        out
    }

    /// Reads a file of a song: an open document's text, or the disk's.
    fn read(&self, path: &Path) -> std::io::Result<String> {
        let wanted = normalize(path);
        for (uri, text) in &self.open {
            if uri.to_file_path().is_ok_and(|p| normalize(&p) == wanted) {
                return Ok(text.clone());
            }
        }
        std::fs::read_to_string(path)
    }

    /// The URI of a file of a song: an open document's, when one is that file.
    fn uri_of(&self, path: &Path) -> Url {
        let wanted = normalize(path);
        self.open
            .keys()
            .find(|uri| uri.to_file_path().is_ok_and(|p| normalize(&p) == wanted))
            .cloned()
            .or_else(|| Url::from_file_path(path).ok())
            .unwrap_or_else(|| Url::parse("file:///").expect("a URL"))
    }

    /// The song a document is analysed in: a song that includes it — an open
    /// one first, else one in the workspace, and of several the one no other
    /// includes — or the document itself.
    fn find_song(&self, uri: &Url) -> Url {
        let Ok(path) = uri.to_file_path() else { return uri.clone() };
        let wanted = identity(&normalize(&path));
        // The identities of a candidate's files, when it includes the document.
        let includes = |song: &Path, text: &str| -> Option<Vec<PathBuf>> {
            if !text.contains("include") {
                return None;
            }
            let parsed = mat_core::parse_with(text, song, &|p| self.read(p));
            let files: Vec<PathBuf> = parsed.sources.iter().map(|s| identity(&s.path)).collect();
            files[1..].contains(&wanted).then_some(files)
        };
        let mut candidates: Vec<(Url, Vec<PathBuf>)> = self
            .open
            .iter()
            .filter(|(other, _)| *other != uri)
            .filter_map(|(other, text)| Some((other.clone(), includes(&other.to_file_path().ok()?, text)?)))
            .collect();
        if candidates.is_empty() {
            let mut found = Vec::new();
            for folder in &self.workspace {
                song_files(folder, 16, &mut found);
            }
            found.sort();
            found.dedup();
            for file in found {
                if identity(&file) == wanted {
                    continue;
                }
                let Ok(text) = self.read(&file) else { continue };
                if let Some(files) = includes(&file, &text) {
                    candidates.push((self.uri_of(&file), files));
                }
            }
        }
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        let topmost = candidates.iter().find(|(candidate, _)| {
            let own = candidate.to_file_path().map(|p| identity(&normalize(&p))).ok();
            !candidates.iter().any(|(other, files)| other != candidate && own.as_ref().is_some_and(|own| files[1..].contains(own)))
        });
        topmost.or(candidates.first()).map_or_else(|| uri.clone(), |(song, _)| song.clone())
    }

    /// Analyses a song and says so: diagnostics for each of its files, and a
    /// timeline for each when it parses.
    fn analyse(&mut self, song: &Url, out: &mut Vec<Notification>) {
        let text = match self.open.get(song) {
            Some(text) => text.clone(),
            None => match song.to_file_path().ok().and_then(|p| std::fs::read_to_string(p).ok()) {
                Some(text) => text,
                None => return self.forget(song, out),
            },
        };
        let previous = self.songs.remove(song);
        let previous_song = previous.as_ref().and_then(|p| p.analysis.song.clone());
        let analysis = match song.to_file_path() {
            Ok(path) => Analysis::of_song(&text, &path, &|p| self.read(p), previous_song),
            Err(_) => Analysis::of(&text, previous_song),
        };
        let uris: Vec<Url> = analysis.files.iter().enumerate().map(|(i, f)| if i == 0 { song.clone() } else { self.uri_of(&f.path) }).collect();
        for (file, uri) in uris.iter().enumerate() {
            out.push(publish(uri.clone(), diagnostics_in(&analysis, file)));
        }
        if let Some(previous) = &previous {
            for gone in previous.uris.iter().filter(|u| !uris.contains(u) && !self.songs.values().any(|s| s.uris.contains(u))) {
                out.push(publish(gone.clone(), Vec::new()));
            }
        }
        if let Some(messages) = timelines(&analysis, &uris) {
            out.extend(messages.into_iter().map(|m| Notification::new(TIMELINE.into(), m)));
        }
        // An open file of this song is analysed in it, and one that was a
        // song of its own is not one any more.
        for uri in &uris {
            if self.open.contains_key(uri) && self.home.insert(uri.clone(), song.clone()).is_some_and(|old| old == *uri && uri != song) {
                self.songs.remove(uri);
            }
        }
        // A document this song no longer includes finds its song again.
        let mut orphans: Vec<Url> = self.home.iter().filter(|(doc, home)| *home == song && !uris.contains(doc)).map(|(doc, _)| doc.clone()).collect();
        orphans.sort();
        self.songs.insert(song.clone(), Analysed { analysis, uris });
        for orphan in orphans {
            self.home.remove(&orphan);
            let found = self.find_song(&orphan);
            self.analyse(&found, out);
        }
    }

    /// Stops analysing a song, and clears the diagnostics of its files that
    /// no other song has.
    fn forget(&mut self, song: &Url, out: &mut Vec<Notification>) {
        if let Some(gone) = self.songs.remove(song) {
            for uri in gone.uris {
                if !self.songs.values().any(|s| s.uris.contains(&uri)) {
                    out.push(publish(uri, Vec::new()));
                }
            }
        }
    }

    /// The analysis of the song an open document is in, looked at from that document.
    fn view(&mut self, uri: &Url) -> Option<(&Analysis, &[Url])> {
        let home = self.home.get(uri)?;
        let analysed = self.songs.get_mut(home)?;
        analysed.analysis.file = analysed.uris.iter().position(|u| u == uri)?;
        Some((&analysed.analysis, &analysed.uris))
    }

    /// The answer to a request.
    pub fn answer(&mut self, request: Request) -> Response {
        let id = request.id.clone();
        let result: Option<serde_json::Value> = match request.method.as_str() {
            Completion::METHOD => serde_json::from_value::<lsp_types::CompletionParams>(request.params).ok().and_then(|p| {
                let (analysis, _) = self.view(&p.text_document_position.text_document.uri)?;
                let items = completions(analysis, p.text_document_position.position);
                serde_json::to_value(CompletionResponse::Array(items)).ok()
            }),
            HoverRequest::METHOD => serde_json::from_value::<lsp_types::HoverParams>(request.params).ok().and_then(|p| {
                let (analysis, _) = self.view(&p.text_document_position_params.text_document.uri)?;
                let (value, range) = hover(analysis, p.text_document_position_params.position)?;
                serde_json::to_value(Hover { contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value }), range: Some(range) }).ok()
            }),
            GotoDefinition::METHOD => serde_json::from_value::<lsp_types::GotoDefinitionParams>(request.params).ok().and_then(|p| {
                let (analysis, uris) = self.view(&p.text_document_position_params.text_document.uri)?;
                let (file, range) = definition(analysis, p.text_document_position_params.position)?;
                serde_json::to_value(GotoDefinitionResponse::Scalar(Location { uri: uris.get(file)?.clone(), range })).ok()
            }),
            DocumentSymbolRequest::METHOD => serde_json::from_value::<lsp_types::DocumentSymbolParams>(request.params).ok().and_then(|p| {
                let (analysis, _) = self.view(&p.text_document.uri)?;
                serde_json::to_value(DocumentSymbolResponse::Nested(symbols(analysis))).ok()
            }),
            _ => None,
        };
        Response::new_ok(id, result.unwrap_or(serde_json::Value::Null))
    }
}

fn publish(uri: Url, diagnostics: Vec<Diagnostic>) -> Notification {
    Notification::new(PublishDiagnostics::METHOD.into(), PublishDiagnosticsParams { uri, diagnostics, version: None })
}

/// Serves over stdin and stdout until the client says shutdown.
pub fn run_stdio() -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        completion_provider: Some(CompletionOptions { trigger_characters: Some(vec![" ".into(), "\"".into(), "/".into()]), ..Default::default() }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        // Says the server sends `mat/timeline`, so an editor that draws it knows
        // to wait for one.
        experimental: Some(serde_json::json!({ "timeline": true })),
        ..Default::default()
    };
    let init = connection.initialize(serde_json::to_value(capabilities)?)?;
    let params: InitializeParams = serde_json::from_value(init)?;
    let mut workspace: Vec<PathBuf> = params.workspace_folders.unwrap_or_default().iter().filter_map(|f| f.uri.to_file_path().ok()).collect();
    if let Some(root) = params.root_uri.and_then(|u| u.to_file_path().ok())
        && !workspace.contains(&root)
    {
        workspace.push(root);
    }

    let mut server = Server::new(workspace);
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                let response = server.answer(request);
                connection.sender.send(Message::Response(response))?;
            }
            Message::Notification(notification) => {
                for outgoing in server.notification(&notification) {
                    connection.sender.send(Message::Notification(outgoing))?;
                }
            }
            Message::Response(_) => {}
        }
    }
    // The writer thread ends when the last sender goes, and the connection
    // holds one: joined with it alive, the process outlives the editor that
    // told it to exit.
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// The document and its new text, for an open or a change.
fn document_change(notification: &Notification) -> Option<(Url, String)> {
    if notification.method == DidOpenTextDocument::METHOD {
        let params: lsp_types::DidOpenTextDocumentParams = serde_json::from_value(notification.params.clone()).ok()?;
        return Some((params.text_document.uri, params.text_document.text));
    }
    if notification.method == DidChangeTextDocument::METHOD {
        let params: lsp_types::DidChangeTextDocumentParams = serde_json::from_value(notification.params.clone()).ok()?;
        // Full sync: the last change carries the whole text.
        let text = params.content_changes.into_iter().last()?.text;
        return Some((params.text_document.uri, text));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SONG: &str = "title \"Test\"
tempo 120

instrument lead synth
  osc saw voices=3
  filter lowpass cutoff=800

instrument kit drums
  kick decay=1.2

pattern beat grid=1/16
  kick X.....x.

pattern verse
  A4:q A4:e A4 A4:q A4:e A4 |

track melody
  instrument lead
  reverb 0.3
  play verse x2

track drums
  instrument kit
  layer rhythm
  play beat x4

master
  gain 3
";

    fn analysis() -> Analysis {
        Analysis::of(SONG, None)
    }

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn the_blocks_are_found_from_the_header_above() {
        let a = analysis();
        assert_eq!(block_at(a.lines(), 0), Block::Top);
        assert_eq!(block_at(a.lines(), 4), Block::Instrument { kind: Some("synth".into()) });
        assert_eq!(block_at(a.lines(), 6), Block::Instrument { kind: Some("synth".into()) }, "a blank line is still in the block");
        assert_eq!(block_at(a.lines(), 11), Block::Pattern);
        assert_eq!(block_at(a.lines(), 17), Block::Track);
        assert_eq!(block_at(a.lines(), 25), Block::Track, "the blank line before a header still belongs to the block above");
        assert_eq!(block_at(a.lines(), 27), Block::Master);
    }

    #[test]
    fn column_one_completes_the_block_keywords() {
        let a = analysis();
        let items = completions(&a, Position::new(2, 0));
        assert!(labels(&items).contains(&"track"));
        assert!(labels(&items).contains(&"instrument"));
        assert!(!labels(&items).contains(&"osc"));
    }

    #[test]
    fn a_track_completes_its_settings_and_the_names_it_may_use() {
        // The `  reverb 0.3` line of `track melody`, replaced by what is typed.
        let text_line = 18;
        let items = completions(&Analysis::of(&SONG.replace("  reverb 0.3", "  "), None), Position::new(text_line, 2));
        assert!(labels(&items).contains(&"play"));
        assert!(labels(&items).contains(&"instrument"));
        assert!(!labels(&items).contains(&"osc"));

        // A half-typed line does not parse, so the names come from the last
        // song that did — which is what the server keeps per document.
        let last = analysis().song;
        let after_instrument = Analysis::of(&SONG.replace("  reverb 0.3", "  instrument "), last.clone());
        let items = completions(&after_instrument, Position::new(text_line, 13));
        assert_eq!(labels(&items), ["lead", "kit"]);

        let after_play = Analysis::of(&SONG.replace("  reverb 0.3", "  play "), last.clone());
        let items = completions(&after_play, Position::new(text_line, 7));
        assert_eq!(labels(&items), ["beat", "verse", "all"]);

        let after_play_v = Analysis::of(&SONG.replace("  reverb 0.3", "  play v"), last.clone());
        let items = completions(&after_play_v, Position::new(text_line, 8));
        assert_eq!(labels(&items), ["verse"], "filtered by what is typed");

        let after_layer = Analysis::of(&SONG.replace("  reverb 0.3", "  layer "), last);
        let items = completions(&after_layer, Position::new(text_line, 8));
        assert_eq!(labels(&items), ["melody", "rhythm"]);
    }

    #[test]
    fn an_instrument_completes_by_its_kind() {
        let a = analysis();
        let synth = completions(&Analysis::of(&SONG.replace("  osc saw voices=3", "  "), None), Position::new(4, 2));
        assert!(labels(&synth).contains(&"filter"));
        assert!(!labels(&synth).contains(&"kick"));
        let drums = completions(&Analysis::of(&SONG.replace("  kick decay=1.2", "  "), None), Position::new(8, 2));
        assert!(labels(&drums).contains(&"kick"));
        assert!(!labels(&drums).contains(&"osc"));
        let osc_options = completions(&a, Position::new(4, 12));
        assert!(labels(&osc_options).contains(&"voices="));
        let waves = completions(&Analysis::of(&SONG.replace("  osc saw voices=3", "  osc "), None), Position::new(4, 6));
        assert_eq!(labels(&waves), ["sine", "triangle", "saw", "square", "supersaw"]);
    }

    #[test]
    fn a_preset_instrument_completes_the_presets_kind() {
        let text = SONG.replace("instrument lead synth\n  osc saw voices=3\n  filter lowpass cutoff=800", "instrument lead preset dream-pad\n  ");
        let items = completions(&Analysis::of(&text, None), Position::new(4, 2));
        assert!(labels(&items).contains(&"filter"), "a dream-pad is a synth: {:?}", labels(&items));
    }

    #[test]
    fn a_preset_name_completes_after_preset() {
        let text = SONG.replace("instrument lead synth", "instrument lead preset ");
        let items = completions(&Analysis::of(&text, None), Position::new(3, 23));
        assert!(items.iter().any(|i| i.label == "dream-pad"));
        assert!(!items.iter().any(|i| i.detail.as_deref().is_some_and(|d| d.starts_with("master"))));
    }

    #[test]
    fn a_name_goes_to_its_definition() {
        let a = analysis();
        // `  instrument lead` is line 17: the name starts at character 13.
        let (file, target) = definition(&a, Position::new(17, 14)).expect("lead is defined");
        assert_eq!((file, target.start), (0, Position::new(3, 11)));
        let (_, pattern) = definition(&a, Position::new(19, 8)).expect("verse is defined");
        assert_eq!(pattern.start.line, 13);
        assert!(definition(&a, Position::new(18, 4)).is_none(), "a setting is not a reference");
    }

    #[test]
    fn the_keyword_goes_where_the_name_beside_it_goes() {
        let a = analysis();
        // The caret on `instrument` of `  instrument lead`, and on `play` of
        // `  play verse x2`: the word answers what the name answers.
        let (file, target) = definition(&a, Position::new(17, 4)).expect("instrument is a jump");
        assert_eq!((file, target.start), (0, Position::new(3, 11)));
        let (_, pattern) = definition(&a, Position::new(19, 3)).expect("play is a jump");
        assert_eq!(pattern.start.line, 13);
        assert!(definition(&a, Position::new(18, 2)).is_none(), "a setting is still not a reference");
    }

    #[test]
    fn a_hover_explains_the_word_it_points_at_while_the_jump_follows_the_line() {
        let a = analysis();
        // `  play verse x2` is line 19: `play` at character 3, `verse` at 8.
        // Both jump to the pattern; the hovers say two different things.
        let (text, _) = hover(&a, Position::new(19, 3)).expect("play is documented");
        assert!(text.starts_with("**play**"), "the keyword's own documentation: {text}");
        let (text, _) = hover(&a, Position::new(19, 8)).expect("verse has lines");
        assert!(text.contains("pattern verse") && text.contains("A4:q"), "the pattern it names: {text}");
        // And the same of `  instrument lead` on line 17.
        let (text, _) = hover(&a, Position::new(17, 4)).expect("instrument is documented");
        assert!(text.starts_with("**instrument**"), "{text}");
        let (text, _) = hover(&a, Position::new(17, 14)).expect("lead has lines");
        assert!(text.contains("instrument lead synth"), "{text}");
        assert!(definition(&a, Position::new(19, 3)).is_some(), "the jump still follows the keyword");
        assert!(definition(&a, Position::new(17, 4)).is_some(), "the jump still follows the keyword");
    }

    #[test]
    fn hover_says_what_a_keyword_means_and_shows_a_named_things_lines() {
        let a = analysis();
        let (text, _) = hover(&a, Position::new(18, 3)).expect("reverb is documented");
        assert!(text.starts_with("**reverb**"), "{text}");
        let (text, _) = hover(&a, Position::new(17, 14)).expect("lead has lines");
        assert!(text.contains("instrument lead synth") && text.contains("osc saw"), "{text}");
        let (text, _) = hover(&a, Position::new(4, 12)).expect("an option");
        assert!(text.starts_with("**osc**"), "{text}");
    }

    #[test]
    fn the_blocks_are_the_symbols() {
        let a = analysis();
        let symbols = symbols(&a);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["lead", "kit", "beat", "verse", "melody", "drums", "master"]);
        assert_eq!(symbols[0].range.start.line, 3);
        assert_eq!(symbols[0].range.end.line, 5, "the block ends before the blank line");
        assert_eq!(symbols[0].detail.as_deref(), Some("synth"));
    }

    #[test]
    fn a_misspelt_name_is_a_diagnostic_with_its_hint() {
        let text = SONG.replace("  instrument lead", "  instrument leed");
        let a = Analysis::of(&text, None);
        let found = diagnostics(&a);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].range.start, Position::new(17, 13));
        assert!(found[0].message.contains("unknown instrument 'leed'"));
        assert!(found[0].message.contains("did you mean 'lead'"));
        assert!(a.song.is_some(), "the song still parses, so names still complete");
    }

    #[test]
    fn a_file_that_does_not_parse_keeps_the_last_songs_names() {
        let good = analysis();
        let broken = Analysis::of(&SONG.replace("track melody", "trak melody"), good.song.clone());
        assert!(!broken.diagnostics.is_empty());
        assert_eq!(broken.pattern_names(), ["beat", "verse"]);
    }

    /// Lines are 0-based on the wire; a text that does not parse sends none.
    #[test]
    fn the_timeline_places_lines_and_is_withheld_when_the_text_does_not_parse() {
        let url = Url::parse("file:///tmp/test.song").unwrap();
        let a = analysis();
        let placed = timelines(&a, std::slice::from_ref(&url)).expect("the song parses").remove(0);
        assert_eq!((placed["uri"].clone(), placed["song"].clone(), placed["files"].clone()), (serde_json::json!(url), serde_json::json!(url), serde_json::json!([url])));
        let lines = placed["lines"].as_array().unwrap();
        // `  play verse x2` is line 19 (0-based): the verse is one bar, twice.
        let play = lines.iter().find(|l| l["line"] == 19).expect("the play line is placed");
        assert_eq!(play["spans"][0][1].as_f64().unwrap() - play["spans"][0][0].as_f64().unwrap(), 4.0);
        assert!(play.get("notes").is_none(), "a play step has no notes of its own");
        // A pattern's line says its passes and its notes, columns 0-based.
        let noted = lines.iter().find(|l| l.get("notes").is_some()).expect("some pattern line has notes");
        let first = &noted["notes"][0];
        assert!(first[3].as_u64().unwrap() > first[2].as_u64().unwrap());
        assert!(!noted["passes"].as_array().unwrap().is_empty());
        // The tracks, with what they play: the melody's play line is 19.
        let melody = placed["tracks"].as_array().unwrap().iter().find(|t| t["name"] == "melody").expect("the melody is a track");
        assert!(melody["plays"].as_array().unwrap().iter().any(|p| p["line"] == 19 && p["pattern"] == "verse"));
        let broken = Analysis::of(&SONG.replace("track melody", "trak melody"), a.song.clone());
        assert!(timelines(&broken, &[url]).is_none());
    }

    #[test]
    fn positions_count_utf16_units() {
        assert_eq!(char_index("äbc", 1), 1);
        assert_eq!(char_index("𝄞bc", 2), 1);
        assert_eq!(utf16_column("𝄞bc", 1), 2);
        let r = range("  𝄞 x", Span { file: 0, line: 1, col: 5, len: 1 });
        assert_eq!(r.start.character, 5);
    }

    // MARK: - Songs of several files

    /// A folder of its own under the temporary directory, removed when dropped.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Folder {
            let dir = std::env::temp_dir().join(format!("mat-lsp-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            // The disk's own spelling: /tmp is /private/tmp on macOS.
            Folder(std::fs::canonicalize(&dir).unwrap())
        }

        fn write(&self, name: &str, text: &str) -> Url {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            Url::from_file_path(path).unwrap()
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const ROOT: &str = "tempo 120
include \"kit.song\"

track melody
  instrument lead
  play verse x2
";

    const KIT: &str = "# a shared kit
instrument lead synth
  osc saw

pattern verse
  C4:h D4 |
";

    fn open(uri: &Url, text: &str) -> Notification {
        Notification::new(
            DidOpenTextDocument::METHOD.into(),
            lsp_types::DidOpenTextDocumentParams { text_document: lsp_types::TextDocumentItem { uri: uri.clone(), language_id: "song".into(), version: 1, text: text.into() } },
        )
    }

    fn change(uri: &Url, text: &str) -> Notification {
        Notification::new(
            DidChangeTextDocument::METHOD.into(),
            lsp_types::DidChangeTextDocumentParams {
                text_document: lsp_types::VersionedTextDocumentIdentifier { uri: uri.clone(), version: 2 },
                content_changes: vec![lsp_types::TextDocumentContentChangeEvent { range: None, range_length: None, text: text.into() }],
            },
        )
    }

    /// The diagnostics published for a file, the last time they were.
    fn published(sent: &[Notification], uri: &Url) -> Option<Vec<Diagnostic>> {
        sent.iter()
            .rev()
            .filter(|n| n.method == PublishDiagnostics::METHOD)
            .filter_map(|n| serde_json::from_value::<PublishDiagnosticsParams>(n.params.clone()).ok())
            .find(|p| p.uri == *uri)
            .map(|p| p.diagnostics)
    }

    fn timelines_sent(sent: &[Notification]) -> Vec<serde_json::Value> {
        sent.iter().filter(|n| n.method == TIMELINE).map(|n| n.params.clone()).collect()
    }

    #[test]
    fn a_problem_in_an_included_file_is_published_to_that_file() {
        let folder = Folder::new("diagnostics");
        let song = folder.write("song.song", ROOT);
        let kit = folder.write("kit.song", &KIT.replace("osc saw", "osc sawz"));
        let mut server = Server::new(vec![folder.0.clone()]);
        let sent = server.notification(&open(&song, ROOT));
        assert_eq!(published(&sent, &song).expect("the song's own"), Vec::new());
        let in_kit = published(&sent, &kit).expect("the kit's, though it is not open");
        assert_eq!(in_kit.len(), 1, "{in_kit:?}");
        assert_eq!(in_kit[0].range.start, Position::new(2, 6));
        assert!(in_kit[0].message.contains("unknown waveform"));
        assert!(timelines_sent(&sent).is_empty(), "a song that does not parse is not placed");

        // The kit opened and fixed, unsaved: the song reads the open text, and
        // the kit is told it has no problems any more.
        let sent = server.notification(&open(&kit, KIT));
        assert_eq!(published(&sent, &kit).expect("the kit's again"), Vec::new());
        assert_eq!(timelines_sent(&sent).len(), 2);
    }

    #[test]
    fn each_file_of_a_song_is_sent_its_own_timeline() {
        let folder = Folder::new("timeline");
        let song = folder.write("song.song", ROOT);
        let kit = folder.write("kit.song", KIT);
        let mut server = Server::new(Vec::new());
        let sent = timelines_sent(&server.notification(&open(&song, ROOT)));
        assert_eq!(sent.len(), 2, "one for the song and one for the kit");
        for message in &sent {
            assert_eq!(message["song"], serde_json::json!(song));
            assert_eq!(message["files"], serde_json::json!([song, kit]));
            assert_eq!(message["barSeconds"], 2.0);
            assert_eq!(message["tracks"], sent[0]["tracks"], "every file is told the whole song's tracks");
        }
        let (root, included) = (&sent[0], &sent[1]);
        assert_eq!(root["uri"], serde_json::json!(song));
        assert_eq!(included["uri"], serde_json::json!(kit));
        let lines = |message: &serde_json::Value| message["lines"].as_array().unwrap().iter().map(|l| l["line"].as_u64().unwrap()).collect::<Vec<_>>();
        // The song: the track's header and its play step; the kit: the
        // instrument's header, the pattern's and its line of notes.
        assert_eq!(lines(root), [3, 5]);
        assert_eq!(lines(included), [1, 4, 5]);
        assert!(included["lines"][2]["notes"].as_array().is_some_and(|n| n.len() == 2));
        let melody = &root["tracks"][0];
        assert_eq!((melody["file"].as_u64(), melody["line"].as_u64()), (Some(0), Some(3)));
        assert_eq!((melody["instrumentFile"].as_u64(), melody["instrumentLine"].as_u64()), (Some(1), Some(1)));
        let play = &melody["plays"][0];
        assert_eq!((play["file"].as_u64(), play["line"].as_u64(), play["patternFile"].as_u64(), play["patternLine"].as_u64()), (Some(0), Some(5), Some(1), Some(4)));
    }

    #[test]
    fn a_name_goes_to_its_definition_in_another_file() {
        let folder = Folder::new("definition");
        let song = folder.write("song.song", ROOT);
        let kit = folder.write("kit.song", KIT);
        let mut server = Server::new(Vec::new());
        server.notification(&open(&song, ROOT));
        let request = |line: u32, character: u32| {
            Request::new(
                1.into(),
                GotoDefinition::METHOD.into(),
                lsp_types::GotoDefinitionParams {
                    text_document_position_params: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri: song.clone() },
                        position: Position::new(line, character),
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
            )
        };
        let location: Location = serde_json::from_value(server.answer(request(4, 14)).result.unwrap()).unwrap();
        assert_eq!(location.uri, kit);
        assert_eq!(location.range.start, Position::new(1, 11));
        let location: Location = serde_json::from_value(server.answer(request(1, 11)).result.unwrap()).unwrap();
        assert_eq!((location.uri.clone(), location.range.start), (kit.clone(), Position::new(0, 0)), "an include goes to its file");
        let location: Location = serde_json::from_value(server.answer(request(1, 2)).result.unwrap()).unwrap();
        assert_eq!((location.uri, location.range.start), (kit, Position::new(0, 0)), "the word include goes there too");
    }

    #[test]
    fn names_hover_and_symbols_across_files() {
        let loader = |path: &Path| if path.ends_with("kit.song") { Ok(KIT.to_string()) } else { Err(std::io::ErrorKind::NotFound.into()) };
        let a = Analysis::of_song(ROOT, Path::new("/songs/song.song"), &loader, None);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let typed = Analysis::of_song(&ROOT.replace("  play verse x2", "  play "), Path::new("/songs/song.song"), &loader, a.song.clone());
        assert_eq!(labels(&completions(&typed, Position::new(5, 7))), ["verse", "all"], "the kit's pattern completes in the song");
        let (text, _) = hover(&a, Position::new(4, 14)).expect("lead is defined in the kit");
        assert!(text.contains("instrument lead synth\n  osc saw"), "{text}");
        let names = |a: &Analysis| symbols(a).iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&a), ["melody"], "the song's own blocks");
        let kit = a.clone().at(1);
        assert_eq!(names(&kit), ["lead", "verse"]);
        assert_eq!(kit.text(), KIT);
        assert!(definition(&kit, Position::new(1, 12)).is_none(), "a header is not a reference");
    }

    #[test]
    fn include_completes_as_a_keyword_and_its_paths() {
        let folder = Folder::new("paths");
        folder.write("song.song", ROOT);
        folder.write("kit.song", KIT);
        folder.write("parts/verse.song", "");
        folder.write("target/built.song", "");
        folder.write(".hidden/secret.song", "");
        let top = completions(&Analysis::of("inc", None), Position::new(0, 3));
        assert_eq!(labels(&top), ["include"]);
        let text = "tempo 120\ninclude \"\n";
        let a = Analysis::of_song(text, &folder.0.join("song.song"), &|p| std::fs::read_to_string(p), None);
        let items = completions(&a, Position::new(1, 9));
        assert_eq!(labels(&items), ["kit.song", "parts/verse.song"], "not the song itself, hidden folders or target");
        let Some(CompletionTextEdit::Edit(edit)) = &items[1].text_edit else { panic!("an edit") };
        assert_eq!((edit.range.start, edit.range.end, edit.new_text.as_str()), (Position::new(1, 8), Position::new(1, 9), "\"parts/verse.song\""));
        let typed = Analysis::of_song("include \"pa\"", &folder.0.join("song.song"), &|p| std::fs::read_to_string(p), None);
        let items = completions(&typed, Position::new(0, 11));
        assert_eq!(labels(&items), ["parts/verse.song"]);
        let Some(CompletionTextEdit::Edit(edit)) = &items[0].text_edit else { panic!("an edit") };
        assert_eq!(edit.range.end, Position::new(0, 12), "the closing quote is replaced too");
    }

    #[test]
    fn an_included_file_opened_alone_is_analysed_through_a_song_that_includes_it() {
        let folder = Folder::new("alone");
        let song = folder.write("songs/song.song", &ROOT.replace("kit.song", "../kits/kit.song"));
        let kit = folder.write("kits/kit.song", KIT);
        folder.write("target/copy.song", &ROOT.replace("kit.song", "../kits/kit.song"));
        folder.write("other.song", "tempo 90\n");
        let mut server = Server::new(vec![folder.0.clone()]);
        let sent = server.notification(&open(&kit, KIT));
        assert_eq!(published(&sent, &song), Some(Vec::new()), "the song found in the workspace is analysed");
        let placed = timelines_sent(&sent);
        let for_kit = placed.iter().find(|m| m["uri"] == serde_json::json!(kit)).expect("the kit is placed in the song");
        assert_eq!(for_kit["song"], serde_json::json!(song));
        assert_eq!(for_kit["lines"].as_array().unwrap().len(), 3);

        // An edit to the kit re-analyses the song it is in.
        let sent = server.notification(&change(&kit, &KIT.replace("osc saw", "osc sawz")));
        assert_eq!(published(&sent, &kit).map(|d| d.len()), Some(1));
        assert_eq!(published(&sent, &song), Some(Vec::new()));

        // A file no song includes is a song of its own.
        let lone = folder.write("lone.song", "tempo 100\ninstrument a synth\n");
        let sent = server.notification(&open(&lone, "tempo 100\ninstrument a synth\n"));
        let placed = timelines_sent(&sent);
        assert_eq!(placed.len(), 1);
        assert_eq!((placed[0]["uri"].clone(), placed[0]["song"].clone()), (serde_json::json!(lone), serde_json::json!(lone)));
    }

    #[test]
    fn a_file_open_on_its_own_joins_the_song_that_starts_including_it() {
        let folder = Folder::new("joins");
        let song = folder.write("song.song", "tempo 120\n");
        let kit = folder.write("kit.song", KIT);
        let mut server = Server::new(Vec::new());
        server.notification(&open(&kit, KIT));
        server.notification(&open(&song, "tempo 120\n"));
        let sent = server.notification(&change(&song, ROOT));
        let placed = timelines_sent(&sent);
        assert_eq!(placed.len(), 2);
        assert!(placed.iter().all(|m| m["song"] == serde_json::json!(song)));
        // And leaves it again: the kit is its own song once more.
        let sent = server.notification(&change(&song, "tempo 120\n"));
        let placed = timelines_sent(&sent);
        assert!(placed.iter().any(|m| m["uri"] == serde_json::json!(kit) && m["song"] == serde_json::json!(kit)), "{placed:?}");
    }

    // MARK: - Loops

    const LOOPED: &str = "tempo 120
instrument lead synth
pattern verse
  (C4:e D4)x2 E4:h |
track melody
  instrument lead
  repeat 2 {
    play verse
  }
";

    #[test]
    fn repeat_completes_in_a_track_and_its_brackets_hover() {
        let typed = Analysis::of(&LOOPED.replace("  repeat 2 {\n    play verse\n  }\n", "  rep"), None);
        assert_eq!(labels(&completions(&typed, Position::new(6, 5))), ["repeat"]);
        let pattern_line = Analysis::of(&LOOPED.replace("  (C4:e D4)x2 E4:h |", "  "), None);
        assert!(!labels(&completions(&pattern_line, Position::new(3, 2))).contains(&"repeat"), "not in a pattern");

        let a = Analysis::of(LOOPED, None);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let (text, _) = hover(&a, Position::new(6, 3)).expect("repeat is documented");
        assert!(text.starts_with("**repeat** — `repeat <n> {`"), "{text}");
        let (text, range) = hover(&a, Position::new(8, 2)).expect("its closing brace says what it closes");
        assert!(text.starts_with("**repeat**"), "{text}");
        assert_eq!((range.start, range.end), (Position::new(8, 2), Position::new(8, 3)));
        let (text, _) = hover(&a, Position::new(3, 2)).expect("a group's '('");
        assert!(text.starts_with("**(…)x<n>** — Plays what is inside"), "{text}");
        let (text, range) = hover(&a, Position::new(3, 11)).expect("a group's ')x2'");
        assert!(text.starts_with("**(…)x<n>**"), "{text}");
        assert_eq!((range.start.character, range.end.character), (10, 13));
        assert!(hover(&a, Position::new(3, 4)).is_none_or(|(text, _)| !text.contains("(…)")), "a note inside a group is a note");
    }

    #[test]
    fn names_inside_a_repeat_block_go_to_their_definitions() {
        let a = Analysis::of(LOOPED, None);
        let (_, target) = definition(&a, Position::new(7, 10)).expect("verse inside the block is defined");
        assert_eq!(target.start, Position::new(2, 8));
        let names: Vec<String> = symbols(&a).iter().map(|s| s.name.clone()).collect();
        assert_eq!(names, ["lead", "verse", "melody"]);
        assert_eq!(symbols(&a)[2].range.end.line, 8, "the track runs to its block's '}}'");
    }

    #[test]
    fn a_wrong_loop_is_a_diagnostic_on_its_token() {
        let found = |text: &str| diagnostics(&Analysis::of(text, None)).into_iter().map(|d| (d.range.start, d.range.end, d.message)).collect::<Vec<_>>();
        let x0 = found(&LOOPED.replace("(C4:e D4)x2 E4:h |", "(C4:e D4)x0 E4:h. |"));
        assert_eq!(x0.len(), 1, "{x0:?}");
        assert_eq!((x0[0].0, x0[0].1), (Position::new(3, 11), Position::new(3, 13)));
        assert!(x0[0].2.starts_with("a group played x0 is never heard\n"), "with its hint: {}", x0[0].2);
        let unclosed = found(&LOOPED.replace("(C4:e D4)x2 E4:h |", "(C4:w |"));
        assert_eq!(unclosed.iter().map(|d| (d.0, d.2.lines().next().unwrap())).collect::<Vec<_>>(), [(Position::new(3, 2), "unclosed '('")]);
        let at = found(&LOOPED.replace("    play verse\n", "    at 2\n    play verse\n"));
        assert_eq!(at.iter().map(|d| (d.0, d.1, d.2.lines().next().unwrap())).collect::<Vec<_>>(), [(Position::new(7, 4), Position::new(7, 6), "'at' inside a repeat")]);
        let stray = found(&LOOPED.replace("  }\n", "  }\n  }\n"));
        assert_eq!(stray.iter().map(|d| (d.0, d.2.lines().next().unwrap())).collect::<Vec<_>>(), [(Position::new(9, 2), "'}' closes no repeat")]);
    }

    /// `examples/loops.song`, as an editor is told it: a group's notes at
    /// their tokens on every pass, and a `repeat` line across its passes.
    #[test]
    fn the_timeline_of_the_loops_example() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/loops.song");
        let text = std::fs::read_to_string(&path).expect("the example is there");
        let at = |needle: &str| text.lines().position(|l| l == needle).expect("the line is in the example") as u64;
        let a = Analysis::of_song(&text, &path, &|p| std::fs::read_to_string(p), None);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let url = Url::parse("file:///tmp/loops.song").unwrap();
        let placed = timelines(&a, &[url]).expect("the example parses").remove(0);
        let bar = placed["barSeconds"].as_f64().unwrap();
        let line = |needle: &str| placed["lines"].as_array().unwrap().iter().find(|l| l["line"] == at(needle)).cloned().unwrap_or_else(|| panic!("{needle} is placed"));
        let bars = |value: &serde_json::Value| value.as_array().unwrap().iter().map(|s| ((s[0].as_f64().unwrap() / bar * 1e6).round() / 1e6, (s[1].as_f64().unwrap() / bar * 1e6).round() / 1e6)).collect::<Vec<_>>();

        // The riff's line: `A1:s` at characters 3-7 three times a pass, then the turn.
        let riff = line("  (A1:s A1 A2! C2~)x3 E2:s G2! A1 C2~ |");
        assert_eq!(riff["passes"].as_array().unwrap().len(), 6, "riff x2, in three passes of the repeat");
        let notes = riff["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 16);
        let a1: Vec<f64> = notes.iter().filter(|n| n[2] == 3).map(|n| (n[0].as_f64().unwrap() / bar * 16.0).round()).collect();
        assert_eq!(a1, [0.0, 4.0, 8.0], "in sixteenths");
        assert!(notes.iter().filter(|n| n[2] == 3).all(|n| n[3] == 7));
        assert!(notes.windows(2).all(|w| w[0][0].as_f64() <= w[1][0].as_f64()), "in start order");

        // The acid's repeat is heard over all twelve bars; the drums' blocks from bar 3.
        assert_eq!(bars(&line("  repeat 3 {")["spans"]), [(0.0, 12.0)]);
        assert_eq!(bars(&line("  repeat 2 {")["spans"]), [(2.0, 10.0)]);
        assert_eq!(bars(&line("    repeat 3 {")["spans"]), [(2.0, 5.0), (6.0, 9.0)]);
        assert_eq!(bars(&line("    play fill")["spans"]), [(5.0, 6.0), (9.0, 10.0)]);
        assert!(line("  repeat 3 {").get("notes").is_none());

        // A play in a block is a frame once per pass, at its own line.
        let acid = placed["tracks"].as_array().unwrap().iter().find(|t| t["name"] == "acid").unwrap();
        let plays: Vec<(u64, f64)> = acid["plays"].as_array().unwrap().iter().map(|p| (p["line"].as_u64().unwrap(), (p["start"].as_f64().unwrap() / bar).round())).collect();
        let (riff, lift) = (at("    play riff x2"), at("    play lift"));
        assert_eq!(plays, [(riff, 0.0), (lift, 2.0), (riff, 4.0), (lift, 6.0), (riff, 8.0), (lift, 10.0)]);
    }
}
