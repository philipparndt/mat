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
//! transport; `run_stdio` wraps them in the protocol.

use std::collections::HashMap;
use std::error::Error;

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _, PublishDiagnostics};
use lsp_types::request::{Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, Request as _};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionResponse, Diagnostic, DiagnosticSeverity, DocumentSymbol,
    DocumentSymbolResponse, GotoDefinitionResponse, Hover, HoverContents, HoverProviderCapability, InitializeParams, Location,
    MarkupContent, MarkupKind, OneOf, Position, PublishDiagnosticsParams, Range, ServerCapabilities, SymbolKind,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use mat_core::diag::{Severity, Span};
use mat_core::lexer::{Line, lex};
use mat_core::model::Song;

pub mod docs;

// MARK: - What the text says

/// A document as the server sees it: the text, its lines of tokens, and the
/// song when it parses — or the last one that did, so names keep completing
/// while a line is half typed.
pub struct Analysis {
    pub text: String,
    pub lines: Vec<Line>,
    pub song: Option<Song>,
    pub diagnostics: Vec<mat_core::Diagnostic>,
    /// Whether `song` is this text's, rather than the last one that parsed.
    pub parsed: bool,
}

impl Analysis {
    pub fn of(text: &str, previous: Option<Song>) -> Analysis {
        let mut lex_diags = Vec::new();
        // One entry per source line, blank and comment lines included as
        // lines with no tokens: the lexer leaves those out, and everything
        // here is asked by the line the cursor is on.
        let count = text.lines().count();
        let mut lines: Vec<Line> = (0..count).map(|_| Line { indented: false, tokens: Vec::new() }).collect();
        for line in lex(text, &mut lex_diags) {
            if let Some(first) = line.tokens.first() {
                let index = first.span.line.saturating_sub(1);
                if index < count {
                    lines[index] = line;
                }
            }
        }
        let (song, mut diagnostics) = mat_core::parse(text);
        if let Some(song) = &song {
            if let Err(errors) = mat_core::arrange(song) {
                diagnostics.extend(errors);
            }
        }
        let parsed = song.is_some();
        Analysis { text: text.to_string(), lines, song: song.or(previous), diagnostics, parsed }
    }

    fn line_text(&self, line: usize) -> &str {
        self.text.lines().nth(line).unwrap_or("")
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

pub fn diagnostics(analysis: &Analysis) -> Vec<Diagnostic> {
    analysis
        .diagnostics
        .iter()
        .map(|d| Diagnostic {
            range: range(&analysis.text, d.span),
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
        match block_at(&analysis.lines, line) {
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

/// A top-level line's kind, name and the name's span, for each block that has
/// a name. Off the lexer, so it holds while the file does not parse.
fn definitions(analysis: &Analysis) -> Vec<(&str, &str, Span, usize)> {
    analysis
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.indented && l.tokens.len() >= 2 && BLOCK_KEYWORDS.contains(&l.tokens[0].text.as_str()))
        .map(|(index, l)| (l.tokens[0].text.as_str(), l.tokens[1].text.as_str(), l.tokens[1].span, index))
        .collect()
}

/// The token under a position, with its index on the line.
fn token_at(analysis: &Analysis, position: Position) -> Option<(usize, &mat_core::lexer::Token)> {
    let line = analysis.lines.get(position.line as usize)?;
    let text = analysis.line_text(position.line as usize);
    let at = char_index(text, position.character) + 1;
    line.tokens.iter().enumerate().find(|(_, t)| t.span.col <= at && at <= t.span.col + t.span.len)
}

/// What kind of thing a token refers to, when it is a reference.
fn referent(analysis: &Analysis, position: Position) -> Option<(&'static str, String)> {
    let (index, token) = token_at(analysis, position)?;
    let line = analysis.lines.get(position.line as usize)?;
    let first = line.tokens.first()?.text.as_str();
    let block = block_at(&analysis.lines, position.line as usize);
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

/// Where the name under the cursor is defined.
pub fn definition(analysis: &Analysis, position: Position) -> Option<Range> {
    let (kind, name) = referent(analysis, position)?;
    definitions(analysis).into_iter().find(|(k, n, _, _)| *k == kind && *n == name).map(|(_, _, span, _)| range(&analysis.text, span))
}

// MARK: - Hover

/// What the word under the cursor means: the keyword's documentation, or a
/// named thing's own lines.
pub fn hover(analysis: &Analysis, position: Position) -> Option<(String, Range)> {
    let (index, token) = token_at(analysis, position)?;
    let token_range = range(&analysis.text, token.span);

    if let Some((kind, name)) = referent(analysis, position) {
        let (_, _, _, line) = definitions(analysis).into_iter().find(|(k, n, _, _)| *k == kind && *n == name)?;
        let mut shown: Vec<&str> = vec![analysis.line_text(line)];
        for (offset, following) in analysis.lines.iter().enumerate().skip(line + 1) {
            if !following.indented && !following.tokens.is_empty() {
                break;
            }
            if shown.len() >= 10 {
                shown.push("  …");
                break;
            }
            shown.push(analysis.line_text(offset));
        }
        while shown.last().is_some_and(|l| l.trim().is_empty()) {
            shown.pop();
        }
        return Some((format!("```song\n{}\n```", shown.join("\n")), token_range));
    }

    let line = analysis.lines.get(position.line as usize)?;
    let first = line.tokens.first()?.text.as_str();
    let block = block_at(&analysis.lines, position.line as usize);
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

/// The blocks and sections of the song, in order.
pub fn symbols(analysis: &Analysis) -> Vec<DocumentSymbol> {
    let headers: Vec<(usize, &Line)> = analysis.lines.iter().enumerate().filter(|(_, l)| !l.indented && !l.tokens.is_empty()).collect();
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
        let mut end = headers.get(position + 1).map(|(next, _)| next.saturating_sub(1)).unwrap_or(analysis.lines.len().saturating_sub(1));
        while end > *line && analysis.lines.get(end).is_some_and(|l| l.tokens.is_empty()) {
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
            selection_range: range(&analysis.text, name_span),
            children: None,
        });
    }
    out
}

// MARK: - Where lines are heard

/// The notification an editor is sent after each analysis that parsed: for
/// every line heard somewhere, the stretches of the song where, in seconds.
pub const TIMELINE: &str = "mat/timeline";

/// `mat/timeline`'s parameters, or nil when this text did not parse — lines
/// placed from the last song that did would be drawn beside lines that have
/// since moved.
pub fn timeline(analysis: &Analysis, uri: &Url) -> Option<serde_json::Value> {
    if !analysis.parsed {
        return None;
    }
    let placements = mat_core::placement::placements(analysis.song.as_ref()?);
    let lines: Vec<serde_json::Value> = placements
        .lines
        .iter()
        .map(|l| {
            let mut entry = serde_json::json!({ "line": l.line.saturating_sub(1), "spans": l.spans.iter().map(|s| [s.0, s.1]).collect::<Vec<_>>() });
            if !l.notes.is_empty() {
                // Columns as the protocol counts them: 0-based UTF-16 units, the
                // note's first and the one after its last.
                let text = analysis.text.lines().nth(l.line.saturating_sub(1)).unwrap_or("");
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
    Some(serde_json::json!({
        "uri": uri,
        "seconds": placements.seconds,
        "barSeconds": placements.bar_seconds,
        "lines": lines,
        // Lines 0-based, as everywhere on the wire.
        "tracks": placements.tracks.iter().map(|t| serde_json::json!({
            "name": t.name,
            "line": t.line.saturating_sub(1),
            "layer": t.layer,
            "instrument": t.instrument,
            "instrumentLine": t.instrument_line.map(|l| l.saturating_sub(1)),
            "plays": t.plays.iter().map(|p| serde_json::json!({
                "line": p.line.saturating_sub(1),
                "pattern": p.pattern,
                "patternLine": p.pattern_line.map(|l| l.saturating_sub(1)),
                "start": p.start,
                "end": p.end,
                "pass": p.pass_seconds,
                "transpose": p.transpose,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    }))
}

// MARK: - The server

/// Serves over stdin and stdout until the client says shutdown.
pub fn run_stdio() -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        completion_provider: Some(CompletionOptions { trigger_characters: Some(vec![" ".into()]), ..Default::default() }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        // Says the server sends `mat/timeline`, so an editor that draws it knows
        // to wait for one.
        experimental: Some(serde_json::json!({ "timeline": true })),
        ..Default::default()
    };
    let init = connection.initialize(serde_json::to_value(capabilities)?)?;
    let _params: InitializeParams = serde_json::from_value(init)?;

    let mut documents: HashMap<Url, Analysis> = HashMap::new();
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                let response = answer(&documents, request);
                connection.sender.send(Message::Response(response))?;
            }
            Message::Notification(notification) => {
                if let Some((url, text)) = document_change(&notification) {
                    let previous = documents.remove(&url).and_then(|a| a.song);
                    let analysis = Analysis::of(&text, previous);
                    let params = PublishDiagnosticsParams { uri: url.clone(), diagnostics: diagnostics(&analysis), version: None };
                    let placed = timeline(&analysis, &url);
                    documents.insert(url, analysis);
                    connection.sender.send(Message::Notification(Notification::new(PublishDiagnostics::METHOD.into(), params)))?;
                    if let Some(placed) = placed {
                        connection.sender.send(Message::Notification(Notification::new(TIMELINE.into(), placed)))?;
                    }
                } else if notification.method == DidCloseTextDocument::METHOD {
                    if let Ok(params) = serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(notification.params) {
                        documents.remove(&params.text_document.uri);
                        let params = PublishDiagnosticsParams { uri: params.text_document.uri, diagnostics: Vec::new(), version: None };
                        connection.sender.send(Message::Notification(Notification::new(PublishDiagnostics::METHOD.into(), params)))?;
                    }
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

fn answer(documents: &HashMap<Url, Analysis>, request: Request) -> Response {
    let id = request.id.clone();
    let result: Option<serde_json::Value> = match request.method.as_str() {
        Completion::METHOD => serde_json::from_value::<lsp_types::CompletionParams>(request.params).ok().and_then(|p| {
            let analysis = documents.get(&p.text_document_position.text_document.uri)?;
            let items = completions(analysis, p.text_document_position.position);
            serde_json::to_value(CompletionResponse::Array(items)).ok()
        }),
        HoverRequest::METHOD => serde_json::from_value::<lsp_types::HoverParams>(request.params).ok().and_then(|p| {
            let analysis = documents.get(&p.text_document_position_params.text_document.uri)?;
            let (value, range) = hover(analysis, p.text_document_position_params.position)?;
            serde_json::to_value(Hover { contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value }), range: Some(range) }).ok()
        }),
        GotoDefinition::METHOD => serde_json::from_value::<lsp_types::GotoDefinitionParams>(request.params).ok().and_then(|p| {
            let uri = p.text_document_position_params.text_document.uri;
            let analysis = documents.get(&uri)?;
            let range = definition(analysis, p.text_document_position_params.position)?;
            serde_json::to_value(GotoDefinitionResponse::Scalar(Location { uri, range })).ok()
        }),
        DocumentSymbolRequest::METHOD => serde_json::from_value::<lsp_types::DocumentSymbolParams>(request.params).ok().and_then(|p| {
            let analysis = documents.get(&p.text_document.uri)?;
            serde_json::to_value(DocumentSymbolResponse::Nested(symbols(analysis))).ok()
        }),
        _ => None,
    };
    Response::new_ok(id, result.unwrap_or(serde_json::Value::Null))
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
        assert_eq!(block_at(&a.lines, 0), Block::Top);
        assert_eq!(block_at(&a.lines, 4), Block::Instrument { kind: Some("synth".into()) });
        assert_eq!(block_at(&a.lines, 6), Block::Instrument { kind: Some("synth".into()) }, "a blank line is still in the block");
        assert_eq!(block_at(&a.lines, 11), Block::Pattern);
        assert_eq!(block_at(&a.lines, 17), Block::Track);
        assert_eq!(block_at(&a.lines, 25), Block::Track, "the blank line before a header still belongs to the block above");
        assert_eq!(block_at(&a.lines, 27), Block::Master);
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
        let target = definition(&a, Position::new(17, 14)).expect("lead is defined");
        assert_eq!(target.start, Position::new(3, 11));
        let pattern = definition(&a, Position::new(19, 8)).expect("verse is defined");
        assert_eq!(pattern.start.line, 13);
        assert!(definition(&a, Position::new(18, 4)).is_none(), "a setting is not a reference");
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
        let placed = timeline(&a, &url).expect("the song parses");
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
        assert!(timeline(&broken, &url).is_none());
    }

    #[test]
    fn positions_count_utf16_units() {
        assert_eq!(char_index("äbc", 1), 1);
        assert_eq!(char_index("𝄞bc", 2), 1);
        assert_eq!(utf16_column("𝄞bc", 1), 2);
        let r = range("  𝄞 x", Span { line: 1, col: 5, len: 1 });
        assert_eq!(r.start.character, 5);
    }
}
