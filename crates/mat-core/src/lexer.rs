//! Line-oriented tokenizer.
//!
//! * `#` at the start of a token begins a comment (so `C#4` stays a note).
//! * `"..."` is a quoted string.
//! * `|` is always its own token (bar check).
//! * `[` ... `]` groups a chord into a single token, spaces included.

use crate::diag::{Diagnostic, Span};

#[derive(Debug, Clone)]
pub struct Token {
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub indented: bool,
    pub tokens: Vec<Token>,
}

pub fn lex(source: &str, diags: &mut Vec<Diagnostic>) -> Vec<Line> {
    lex_file(source, 0, diags)
}

/// Lexes the file of a song at index `file`: every span says so.
pub fn lex_file(source: &str, file: usize, diags: &mut Vec<Diagnostic>) -> Vec<Line> {
    let span = |line: usize, start: usize, len: usize| Span { file, line, col: start + 1, len };
    let mut lines = Vec::new();
    for (idx, raw) in source.lines().enumerate() {
        let line_no = idx + 1;
        let chars: Vec<char> = raw.chars().collect();
        let mut tokens = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c.is_whitespace() {
                i += 1;
                continue;
            }
            if c == '#' {
                break;
            }
            let start = i;
            if c == '|' {
                i += 1;
                tokens.push(Token { text: "|".into(), span: span(line_no, start, 1) });
                continue;
            }
            if c == '"' {
                i += 1;
                let mut text = String::new();
                while i < chars.len() && chars[i] != '"' {
                    text.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    diags.push(Diagnostic::error(span(line_no, start, chars.len() - start), "unterminated string"));
                } else {
                    i += 1;
                }
                tokens.push(Token { text, span: span(line_no, start, i - start) });
                continue;
            }
            let mut depth = 0usize;
            let mut text = String::new();
            while i < chars.len() {
                let c = chars[i];
                if depth == 0 && (c.is_whitespace() || c == '|') {
                    break;
                }
                match c {
                    '[' => depth += 1,
                    ']' => depth = depth.saturating_sub(1),
                    _ => {}
                }
                text.push(c);
                i += 1;
            }
            if depth > 0 {
                diags.push(
                    Diagnostic::error(span(line_no, start, i - start), "unclosed '['")
                        .with_hint("chords are written like [C4 E4 G4]:h"),
                );
            }
            tokens.push(Token { text, span: span(line_no, start, i - start) });
        }
        if !tokens.is_empty() {
            let indented = chars.first().is_some_and(|c| c.is_whitespace());
            lines.push(Line { indented, tokens });
        }
    }
    lines
}
