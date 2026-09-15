//! Diagnostics with source locations, rendered compiler-style so that both
//! humans and AI assistants can fix song files quickly.

use std::fmt::Write;

/// A location in the source. `line` and `col` are 1-based, `len` is in chars.
/// `file` says which file of the song: 0 is the song itself, and each file it
/// includes has the index it was first read at (see `parser::Parsed::sources`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct Span {
    pub file: usize,
    pub line: usize,
    pub col: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    pub hint: Option<String>,
}

impl Diagnostic {
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Self { severity: Severity::Error, span, message: message.into(), hint: None }
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        Self { severity: Severity::Warning, span, message: message.into(), hint: None }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn render(&self, file_name: &str, source: &str) -> String {
        let mut out = String::new();
        let label = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let _ = writeln!(out, "{label}: {}", self.message);
        let line_no = self.span.line;
        let gutter = line_no.to_string().len().max(2);
        let _ = writeln!(out, "{:gutter$}--> {file_name}:{}:{}", "", line_no, self.span.col);
        if let Some(line) = source.lines().nth(line_no.saturating_sub(1)) {
            let _ = writeln!(out, "{:gutter$} |", "");
            let _ = writeln!(out, "{line_no:>gutter$} | {line}");
            let pad: String = line
                .chars()
                .take(self.span.col.saturating_sub(1))
                .map(|c| if c == '\t' { '\t' } else { ' ' })
                .collect();
            let carets = "^".repeat(self.span.len.max(1));
            let _ = writeln!(out, "{:gutter$} | {pad}{carets}", "");
        }
        if let Some(hint) = &self.hint {
            let _ = writeln!(out, "{:gutter$} = hint: {hint}", "");
        }
        out
    }
}

pub fn has_errors(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| d.severity == Severity::Error)
}

/// Suggests the closest candidate for a misspelled name.
pub fn did_you_mean<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<String> {
    candidates
        .into_iter()
        .map(|c| (levenshtein(name, c), c))
        .filter(|(d, c)| *d <= 2.max(c.len() / 3))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| format!("did you mean '{c}'?"))
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}
