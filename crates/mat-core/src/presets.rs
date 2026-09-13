//! Built-in presets: instrument and master blocks that songs can start from
//! with `instrument name preset <preset>` and `master preset <preset>`.
//! The library lives in `presets.song`, in the normal song syntax.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::lexer::{Line, lex};

pub struct Preset {
    pub name: String,
    /// Instrument kind (synth, drums, samples, ...) or "master".
    pub kind: String,
    pub description: String,
    pub lines: Vec<&'static Line>,
}

pub struct Library {
    presets: BTreeMap<String, Preset>,
}

impl Library {
    pub fn get(&self, name: &str) -> Option<&Preset> {
        self.presets.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.presets.keys().map(String::as_str)
    }

    pub fn all(&self) -> impl Iterator<Item = &Preset> {
        self.presets.values()
    }
}

const SOURCE: &str = include_str!("../presets.song");

pub fn library() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| {
        let mut diags = Vec::new();
        let lines: &'static Vec<Line> = Box::leak(Box::new(lex(SOURCE, &mut diags)));
        assert!(diags.is_empty(), "presets.song has lexer errors: {diags:?}");
        let mut presets = BTreeMap::new();
        let mut current: Option<Preset> = None;
        for line in lines {
            if line.indented {
                if let Some(p) = &mut current {
                    p.lines.push(line);
                }
                continue;
            }
            if let Some(p) = current.take() {
                presets.insert(p.name.clone(), p);
            }
            let t = &line.tokens;
            assert!(t.len() >= 3 && t[0].text == "preset", "presets.song: expected 'preset <name> <kind> \"description\"'");
            current = Some(Preset {
                name: t[1].text.clone(),
                kind: t[2].text.clone(),
                description: t.get(3).map(|d| d.text.clone()).unwrap_or_default(),
                lines: Vec::new(),
            });
        }
        if let Some(p) = current.take() {
            presets.insert(p.name.clone(), p);
        }
        Library { presets }
    })
}
