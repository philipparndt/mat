//! Parses `.song` files into a [`Song`]. See `docs/FORMAT.md` for the language.

use std::collections::HashSet;

use crate::diag::{Diagnostic, Span, did_you_mean, has_errors};
use crate::lexer::{Line, Token, lex};
use crate::presets;
use crate::model::*;

const TOP_LEVEL: &[&str] = &["title", "tempo", "meter", "section", "instrument", "pattern", "track", "master"];
const EPS: f64 = 1e-9;

struct Block<'a> {
    header: &'a Line,
    body: Vec<&'a Line>,
}

pub fn parse(source: &str) -> (Option<Song>, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let lines = lex(source, &mut diags);
    let lib = presets::library();

    // `instrument x preset y` and `master preset y` get a synthesized header and
    // the preset's lines in front of their own body.
    let mut arena: Vec<Line> = Vec::new();
    let mut expansions: Vec<(usize, Option<usize>, Vec<&'static Line>)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.indented {
            continue;
        }
        let t = &line.tokens;
        let (preset_at, kw) = match t.first().map(|t| t.text.as_str()) {
            Some("instrument") if t.len() >= 4 && t[2].text == "preset" => (3, "instrument"),
            Some("master") if t.len() >= 3 && t[1].text == "preset" => (2, "master"),
            _ => continue,
        };
        let name = &t[preset_at].text;
        match lib.get(name) {
            Some(preset) if (kw == "master") == (preset.kind == "master") => {
                let mut header = Line { indented: false, tokens: Vec::new() };
                header.tokens.push(t[0].clone());
                if kw == "instrument" {
                    header.tokens.push(t[1].clone());
                    header.tokens.push(Token { text: preset.kind.clone(), span: t[2].span });
                }
                header.tokens.extend(t[preset_at + 1..].iter().cloned());
                arena.push(header);
                expansions.push((i, Some(arena.len() - 1), preset.lines.clone()));
            }
            Some(_) => diags.push(Diagnostic::error(t[preset_at].span, format!("preset '{name}' is not for {kw}"))),
            None => {
                let mut d = Diagnostic::error(t[preset_at].span, format!("unknown preset '{name}'"));
                d = d.with_hint(did_you_mean(name, lib.names()).unwrap_or_else(|| "list presets with: mat presets".into()));
                diags.push(d);
                expansions.push((i, None, Vec::new()));
            }
        }
    }

    let mut blocks: Vec<Block> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.indented {
            match blocks.last_mut() {
                Some(block) => block.body.push(line),
                None => diags.push(Diagnostic::error(
                    line.tokens[0].span,
                    "indented line does not belong to any block",
                )),
            }
        } else if let Some((_, header, body)) = expansions.iter().find(|(idx, _, _)| *idx == i) {
            let header = header.map_or(line, |h| &arena[h]);
            blocks.push(Block { header, body: body.clone() });
        } else {
            blocks.push(Block { header: line, body: Vec::new() });
        }
    }

    let mut p = Parser {
        diags,
        song: Song {
            title: None,
            tempo: 120.0,
            meter: (4, 4),
            instruments: Vec::new(),
            patterns: Vec::new(),
            tracks: Vec::new(),
            master: Master::default(),
            sections: Vec::new(),
        },
    };

    // Globals first: patterns need the meter for bar checks regardless of order.
    for block in &blocks {
        let kw = &block.header.tokens[0];
        if matches!(kw.text.as_str(), "title" | "tempo" | "meter") {
            p.global(block);
        }
    }

    let mut master_seen = false;
    for block in &blocks {
        let kw = &block.header.tokens[0];
        match kw.text.as_str() {
            "title" | "tempo" | "meter" => {}
            "section" => p.section(block),
            "instrument" => p.instrument(block),
            "pattern" => p.pattern(block),
            "track" => p.track(block),
            "master" => {
                if master_seen {
                    p.err(kw.span, "duplicate 'master' block");
                }
                master_seen = true;
                p.master(block);
            }
            other => {
                let mut d = Diagnostic::error(kw.span, format!("unknown statement '{other}'"));
                d = d.with_hint(
                    did_you_mean(other, TOP_LEVEL.iter().copied())
                        .unwrap_or_else(|| format!("expected one of: {}", TOP_LEVEL.join(", "))),
                );
                p.diags.push(d);
            }
        }
    }

    p.check_duplicates();

    let ok = !has_errors(&p.diags);
    (ok.then_some(p.song), p.diags)
}

struct Parser {
    diags: Vec<Diagnostic>,
    song: Song,
}

impl Parser {
    fn err(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::error(span, msg));
    }

    fn err_hint(&mut self, span: Span, msg: impl Into<String>, hint: impl Into<String>) {
        self.diags.push(Diagnostic::error(span, msg).with_hint(hint));
    }

    fn no_body(&mut self, block: &Block) {
        if let Some(line) = block.body.first() {
            self.err(line.tokens[0].span, "this statement does not take an indented body");
        }
    }

    fn arg<'t>(&mut self, line: &'t Line, idx: usize, what: &str) -> Option<&'t Token> {
        let tok = line.tokens.get(idx);
        if tok.is_none() {
            let last = line.tokens.last().unwrap().span;
            self.err(Span { col: last.col + last.len, len: 1, ..last }, format!("missing {what}"));
        }
        tok
    }

    fn global(&mut self, block: &Block) {
        self.no_body(block);
        let line = block.header;
        let kw = line.tokens[0].text.as_str();
        match kw {
            "title" => {
                if let Some(t) = self.arg(line, 1, "title text") {
                    self.song.title = Some(t.text.clone());
                }
            }
            "tempo" => {
                if let Some(t) = self.arg(line, 1, "tempo in BPM")
                    && let Some(v) = self.number(t, 20.0, 400.0)
                {
                    self.song.tempo = v;
                }
            }
            "meter" => {
                if let Some(t) = self.arg(line, 1, "meter such as 4/4") {
                    let parsed = t
                        .text
                        .split_once('/')
                        .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
                        .filter(|(a, b)| *a > 0 && [1, 2, 4, 8, 16].contains(b));
                    match parsed {
                        Some(m) => self.song.meter = m,
                        None => self.err_hint(t.span, format!("invalid meter '{}'", t.text), "write it like 4/4, 3/4 or 6/8"),
                    }
                }
            }
            _ => unreachable!(),
        }
        self.extra_tokens(line, 2);
    }

    fn section(&mut self, block: &Block) {
        self.no_body(block);
        let line = block.header;
        let Some((name, span)) = self.name(line, "section") else { return };
        let Some(t) = self.arg(line, 2, "bar range such as bars=9-16") else { return };
        let range = t.text.strip_prefix("bars=").and_then(|r| r.split_once('-')).and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?)));
        match range {
            Some((a, b)) if a >= 1.0 && b >= a => {
                if let Some(prev) = self.song.sections.iter().find(|s| s.name == name) {
                    let _ = prev;
                    self.err(span, format!("section '{name}' is defined twice"));
                }
                self.song.sections.push(Section { name, from_bar: a, to_bar: b });
            }
            _ => self.err_hint(t.span, format!("invalid bar range '{}'", t.text), "write it like: section chorus bars=17-32"),
        }
        self.extra_tokens(line, 3);
    }

    fn extra_tokens(&mut self, line: &Line, from: usize) {
        if let Some(t) = line.tokens.get(from) {
            self.err(t.span, format!("unexpected '{}'", t.text));
        }
    }

    fn name(&mut self, line: &Line, what: &str) -> Option<(String, Span)> {
        let tok = self.arg(line, 1, &format!("{what} name"))?;
        let valid = tok.text.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            && tok.text.chars().next().is_some_and(|c| c.is_alphabetic());
        if !valid {
            self.err_hint(tok.span, format!("invalid {what} name '{}'", tok.text), "names start with a letter and use letters, digits, '-' and '_'");
            return None;
        }
        Some((tok.text.clone(), tok.span))
    }

    // ---------------------------------------------------------------- instruments

    fn instrument(&mut self, block: &Block) {
        let line = block.header;
        let Some((name, span)) = self.name(line, "instrument") else { return };
        let kind_tok = line.tokens.get(2);
        let kind = match kind_tok.map(|t| t.text.as_str()) {
            None | Some("synth") => InstrumentKind::Synth(self.synth_body(block)),
            Some("drums") => InstrumentKind::Drums(self.drums_body(block)),
            Some("sampler") => match self.sampler_body(block) {
                Some(def) => InstrumentKind::Sampler(def),
                None => return,
            },
            Some("samples") => match self.samples_body(block) {
                Some(def) => InstrumentKind::Samples(def),
                None => return,
            },
            Some("au") => InstrumentKind::AudioUnit(self.au_body(block)),
            Some("tb303") => InstrumentKind::Tb303(self.tb303_body(block)),
            Some("clap") => match self.clap_body(block) {
                Some(def) => InstrumentKind::Clap(def),
                None => return,
            },
            Some(other) => {
                self.err_hint(
                    kind_tok.unwrap().span,
                    format!("unknown instrument type '{other}'"),
                    "expected one of: synth, drums, sampler, samples, clap, tb303, au",
                );
                return;
            }
        };
        self.extra_tokens(line, 3);
        self.song.instruments.push(Instrument { name, span, kind });
    }

    fn synth_body(&mut self, block: &Block) -> SynthDef {
        let mut def = SynthDef::default();
        for line in &block.body {
            let kw = &line.tokens[0];
            match kw.text.as_str() {
                "osc" => {
                    let Some(wt) = self.arg(line, 1, "waveform") else { continue };
                    let mut osc = Oscillator::default();
                    match wt.text.as_str() {
                        "sine" => osc.wave = Waveform::Sine,
                        "triangle" | "tri" => osc.wave = Waveform::Triangle,
                        "saw" => osc.wave = Waveform::Saw,
                        "square" => osc.wave = Waveform::Square,
                        "supersaw" => {
                            osc.wave = Waveform::Saw;
                            osc.supersaw = Some(SuperSaw { detune: 0.35, mix: 0.6 });
                        }
                        other => {
                            self.err_hint(wt.span, format!("unknown waveform '{other}'"), "expected one of: sine, triangle, saw, square, supersaw");
                            continue;
                        }
                    }
                    for (key, val, tok) in self.options(line, 2) {
                        if let Some(ss) = &mut osc.supersaw {
                            match key {
                                "detune" => {
                                    set(&mut ss.detune, self.value(tok, val, 0.0, 1.0));
                                    continue;
                                }
                                "mix" => {
                                    set(&mut ss.mix, self.value(tok, val, 0.0, 1.0));
                                    continue;
                                }
                                "voices" | "spread" => {
                                    self.err_hint(tok.span, format!("'{key}' does not apply to supersaw"), "supersaw has 7 voices; shape it with detune=0..1 and mix=0..1, or use 'osc saw voices=7 spread=20' for plain unison");
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        match key {
                            "level" => set(&mut osc.level, self.value(tok, val, 0.0, 4.0)),
                            "octave" => set(&mut osc.octave, self.value::<f64>(tok, val, -4.0, 4.0).map(|v| v.round() as i32)),
                            "semi" => set(&mut osc.semitones, self.value(tok, val, -24.0, 24.0)),
                            "detune" => set(&mut osc.detune_cents, self.value(tok, val, -100.0, 100.0)),
                            "voices" => set(&mut osc.voices, self.value::<f64>(tok, val, 1.0, 16.0).map(|v| v.round() as u32)),
                            "spread" => set(&mut osc.spread_cents, self.value(tok, val, 0.0, 100.0)),
                            "width" => set(&mut osc.width, self.value(tok, val, 0.0, 1.0)),
                            _ => self.unknown_option(tok, key, "osc", &["level", "octave", "semi", "detune", "voices", "spread", "width"]),
                        }
                    }
                    def.oscillators.push(osc);
                }
                "noise" => {
                    if let Some(t) = self.arg(line, 1, "noise level") {
                        set(&mut def.noise, self.value(t, &t.text, 0.0, 1.0));
                    }
                    self.extra_tokens(line, 2);
                }
                "filter" => {
                    let Some(mt) = self.arg(line, 1, "filter mode") else { continue };
                    let mode = match mt.text.as_str() {
                        "lowpass" | "lp" => FilterMode::Lowpass,
                        "highpass" | "hp" => FilterMode::Highpass,
                        "bandpass" | "bp" => FilterMode::Bandpass,
                        "off" => {
                            // `filter off` removes every filter (e.g. from a preset).
                            def.filters.clear();
                            self.extra_tokens(line, 2);
                            continue;
                        }
                        other => {
                            self.err_hint(mt.span, format!("unknown filter mode '{other}'"), "expected one of: lowpass, highpass, bandpass, off");
                            continue;
                        }
                    };
                    // Filters chain in series; a line with an existing mode replaces that stage.
                    let mut f = Filter { mode, ..Filter::default() };
                    let existing = def.filters.iter().position(|x| x.mode == mode);
                    for (key, val, tok) in self.options(line, 2) {
                        let f = &mut f;
                        match key {
                            "cutoff" => set(&mut f.cutoff_hz, self.hz(tok, val)),
                            "res" | "resonance" => set(&mut f.resonance, self.value(tok, val, 0.0, 1.0)),
                            "env" => set(&mut f.env_octaves, self.value(tok, val, -10.0, 10.0)),
                            "keytrack" => set(&mut f.keytrack, self.value(tok, val, 0.0, 1.0)),
                            "drive" => set(&mut f.drive, self.value(tok, val, 0.0, 1.0)),
                            _ => self.unknown_option(tok, key, "filter", &["cutoff", "res", "env", "keytrack", "drive"]),
                        }
                    }
                    match existing {
                        Some(i) => def.filters[i] = f,
                        None => def.filters.push(f),
                    }
                }
                "amp" | "fenv" => {
                    let is_amp = kw.text == "amp";
                    let mut env = if is_amp { def.amp } else { def.filter_env };
                    for (key, val, tok) in self.options(line, 1) {
                        match key {
                            "attack" | "a" => set(&mut env.attack, self.seconds(tok, val)),
                            "decay" | "d" => set(&mut env.decay, self.seconds(tok, val)),
                            "sustain" | "s" => set(&mut env.sustain, self.value(tok, val, 0.0, 1.0)),
                            "release" | "r" => set(&mut env.release, self.seconds(tok, val)),
                            _ => self.unknown_option(tok, key, &kw.text, &["attack", "decay", "sustain", "release"]),
                        }
                    }
                    if is_amp {
                        def.amp = env;
                    } else {
                        def.filter_env = env;
                    }
                }
                "lfo" => {
                    let Some(tt) = self.arg(line, 1, "target") else { continue };
                    let target = match tt.text.as_str() {
                        "filter" => LfoTarget::Filter,
                        "pitch" => LfoTarget::Pitch,
                        "pan" => LfoTarget::Pan,
                        "amp" => LfoTarget::Amp,
                        "width" => LfoTarget::Width,
                        other => {
                            self.err_hint(tt.span, format!("unknown lfo target '{other}'"), "targets: filter, pitch, pan, amp, width");
                            continue;
                        }
                    };
                    let mut lfo = Lfo { target, rate_hz: 1.0, depth: 0.5, fade_in: 0.0, phase: None };
                    for (key, val, tok) in self.options(line, 2) {
                        match key {
                            "rate" => set(&mut lfo.rate_hz, self.value(tok, val, 0.01, 50.0)),
                            "depth" => set(&mut lfo.depth, self.value(tok, val, 0.0, 1200.0)),
                            "fade" => set(&mut lfo.fade_in, self.seconds(tok, val)),
                            "phase" => lfo.phase = self.value(tok, val, 0.0, 1.0),
                            _ => self.unknown_option(tok, key, "lfo", &["rate", "depth", "fade", "phase"]),
                        }
                    }
                    def.lfos.push(lfo);
                }
                "drift" => {
                    if let Some(t) = self.arg(line, 1, "cents") {
                        set(&mut def.drift_cents, self.value(t, &t.text, 0.0, 100.0));
                    }
                    self.extra_tokens(line, 2);
                }
                "vibrato" => {
                    for (key, val, tok) in self.options(line, 1) {
                        let v = &mut def.vibrato;
                        match key {
                            "rate" => set(&mut v.rate_hz, self.value(tok, val, 0.0, 30.0)),
                            "depth" => set(&mut v.depth_cents, self.value(tok, val, 0.0, 200.0)),
                            "delay" => set(&mut v.delay, self.seconds(tok, val)),
                            _ => self.unknown_option(tok, key, "vibrato", &["rate", "depth", "delay"]),
                        }
                    }
                }
                other => {
                    const KW: &[&str] = &["osc", "noise", "filter", "amp", "fenv", "vibrato", "lfo", "drift"];
                    self.unknown_keyword(kw, other, "synth instruments", KW);
                }
            }
        }
        if def.oscillators.is_empty() && def.noise == 0.0 {
            def.oscillators.push(Oscillator::default());
        }
        def
    }

    fn drums_body(&mut self, block: &Block) -> DrumKit {
        let mut kit = DrumKit::default();
        for line in &block.body {
            let kw = &line.tokens[0];
            let Some(kind) = DrumKind::from_name(&kw.text) else {
                let names: Vec<&str> = DrumKind::ALL.iter().map(|k| k.name()).collect();
                self.unknown_keyword(kw, &kw.text, "drum kits", &names);
                continue;
            };
            let mut voice = DrumVoice::default();
            for (key, val, tok) in self.options(line, 1) {
                match key {
                    "gain" => set(&mut voice.gain_db, self.db(tok, val)),
                    "tune" => set(&mut voice.tune, self.value(tok, val, -24.0, 24.0)),
                    "decay" => set(&mut voice.decay, self.value(tok, val, 0.1, 4.0)),
                    _ => self.unknown_option(tok, key, kind.name(), &["gain", "tune", "decay"]),
                }
            }
            kit.voices[kind as usize] = voice;
        }
        kit
    }

    fn samples_body(&mut self, block: &Block) -> Option<SamplesDef> {
        let mut settings = SamplerDef { load: String::new(), gain_db: 0.0, tune: 0.0, attack: 0.0, release: 0.15, velocity_db: 12.0, articulation: None, drum_map: Vec::new() };
        let mut zones = Vec::new();
        for line in &block.body {
            let kw = &line.tokens[0];
            match kw.text.as_str() {
                "amp" => {
                    for (key, val, tok) in self.options(line, 1) {
                        match key {
                            "attack" | "a" => set(&mut settings.attack, self.seconds(tok, val)),
                            "release" | "r" => set(&mut settings.release, self.seconds(tok, val)),
                            _ => self.unknown_option(tok, key, "amp", &["attack", "release"]),
                        }
                    }
                    continue;
                }
                "gain" | "velocity" => {
                    if let Some(t) = self.arg(line, 1, "value") {
                        let v = self.db(t, &t.text);
                        if kw.text == "gain" { set(&mut settings.gain_db, v) } else { set(&mut settings.velocity_db, v.map(f32::abs)) }
                    }
                    continue;
                }
                _ => {}
            }
            // <drum> "file" ...   or   note <Root> "file" ...
            let (drum, root, file_index) = if kw.text == "note" {
                let Some(root_tok) = self.arg(line, 1, "root note such as G2") else { continue };
                let Some(root) = parse_note(&root_tok.text) else {
                    self.err_hint(root_tok.span, format!("'{}' is not a note", root_tok.text), "write the note the sample was recorded at, e.g. note G2 \"bass.wav\" at=12.3s");
                    continue;
                };
                (None, root, 2)
            } else if let Some(d) = DrumKind::from_name(&kw.text) {
                (Some(d), d.gm_note() as f32, 1)
            } else {
                let names: Vec<&str> = DrumKind::ALL.iter().map(|k| k.name()).collect();
                let mut all = vec!["note", "amp", "gain", "velocity"];
                all.extend(names);
                self.unknown_keyword(kw, &kw.text, "samples instruments", &all);
                continue;
            };
            let Some(file) = self.arg(line, file_index, "audio file path") else { continue };
            let mut zone = SampleZone {
                path: file.text.clone(),
                drum,
                root,
                key_low: 0,
                key_high: 127,
                vel_low: 0,
                vel_high: 127,
                start: 0.0,
                length: None,
                loop_range: None,
                gain_db: 0.0,
                tune: 0.0,
            };
            if drum.is_some() {
                zone.key_low = root as u8;
                zone.key_high = root as u8;
            }
            for (key, val, tok) in self.options(line, file_index + 1) {
                match key {
                    "at" => match parse_seconds(val) {
                        Some(v) if v >= 0.0 => zone.start = v,
                        _ => self.err_hint(tok.span, format!("invalid time '{val}'"), "position in the file, e.g. at=41.52s or at=1500ms"),
                    },
                    "length" => match parse_seconds(val) {
                        Some(v) if v > 0.0 => zone.length = Some(v),
                        _ => self.err_hint(tok.span, format!("invalid length '{val}'"), "e.g. length=0.4s"),
                    },
                    "gain" => set(&mut zone.gain_db, self.db(tok, val)),
                    "tune" => set(&mut zone.tune, self.value(tok, val, -48.0, 48.0)),
                    "keys" => match val.split_once('-').and_then(|(a, b)| Some((parse_note(a)?, parse_note(b)?))) {
                        Some((a, b)) if a <= b => {
                            zone.key_low = a as u8;
                            zone.key_high = b as u8;
                        }
                        _ => self.err_hint(tok.span, format!("invalid key range '{val}'"), "write it like keys=C1-B2"),
                    },
                    "vel" => match val.split_once('-').and_then(|(a, b)| Some((a.parse::<u8>().ok()?, b.parse::<u8>().ok()?))) {
                        Some((a, b)) if a <= b && b <= 127 => {
                            zone.vel_low = a;
                            zone.vel_high = b;
                        }
                        _ => self.err_hint(tok.span, format!("invalid velocity range '{val}'"), "write it like vel=1-80"),
                    },
                    "loop" => match val.split_once('-').and_then(|(a, b)| Some((parse_seconds(a)?, parse_seconds(b)?))) {
                        Some((a, b)) if a < b => zone.loop_range = Some((a, b)),
                        _ => self.err_hint(tok.span, format!("invalid loop '{val}'"), "loop points are relative to the region start, e.g. loop=0.5s-1.4s"),
                    },
                    _ => self.unknown_option(tok, key, &kw.text, &["at", "length", "loop", "keys", "vel", "gain", "tune"]),
                }
            }
            zones.push(zone);
        }
        if zones.is_empty() {
            self.err_hint(block.header.tokens[1].span, "samples instrument has no samples", "add lines like: kick \"drums.wav\" at=41.5s length=0.4s");
            return None;
        }
        Some(SamplesDef { zones, settings })
    }

    fn sampler_body(&mut self, block: &Block) -> Option<SamplerDef> {
        let mut def = SamplerDef { load: String::new(), gain_db: 0.0, tune: 0.0, attack: 0.0, release: 0.3, velocity_db: 18.0, articulation: None, drum_map: Vec::new() };
        for line in &block.body {
            let kw = &line.tokens[0];
            match kw.text.as_str() {
                "load" => {
                    if let Some(t) = self.arg(line, 1, "file path") {
                        def.load = t.text.clone();
                    }
                    self.extra_tokens(line, 2);
                }
                "map" => {
                    for (key, val, tok) in self.options(line, 1) {
                        let Some(kind) = DrumKind::from_name(key) else {
                            let names = DrumKind::ALL.iter().map(|k| k.name());
                            let hint = did_you_mean(key, names).unwrap_or_else(|| "drums are kick, snare, clap, hat, openhat, tom, rim, crash, ride".into());
                            self.err_hint(tok.span, format!("unknown drum '{key}'"), hint);
                            continue;
                        };
                        let note = parse_note(val).map(|n| n as u8).or_else(|| val.parse::<u8>().ok().filter(|n| *n <= 127));
                        match note {
                            Some(n) => def.drum_map.push((kind, n)),
                            None => self.err_hint(tok.span, format!("invalid note '{val}'"), "use a MIDI note number (43) or a note name (G2)"),
                        }
                    }
                }
                "articulation" => {
                    if let Some(t) = self.arg(line, 1, "articulation number") {
                        def.articulation = self.number(t, 0.0, 255.0).map(|v| v as u8);
                    }
                    self.extra_tokens(line, 2);
                }
                "gain" | "tune" | "velocity" => {
                    let Some(t) = self.arg(line, 1, "value") else { continue };
                    match kw.text.as_str() {
                        "gain" => set(&mut def.gain_db, self.db(t, &t.text)),
                        "tune" => set(&mut def.tune, self.value(t, &t.text, -48.0, 48.0)),
                        _ => set(&mut def.velocity_db, self.db(t, &t.text).map(f32::abs)),
                    }
                    self.extra_tokens(line, 2);
                }
                "amp" => {
                    for (key, val, tok) in self.options(line, 1) {
                        match key {
                            "attack" | "a" => set(&mut def.attack, self.seconds(tok, val)),
                            "release" | "r" => set(&mut def.release, self.seconds(tok, val)),
                            _ => self.unknown_option(tok, key, "amp", &["attack", "release"]),
                        }
                    }
                }
                other => self.unknown_keyword(kw, other, "sampler instruments", &["load", "gain", "tune", "velocity", "articulation", "map", "amp"]),
            }
        }
        if def.load.is_empty() {
            self.err_hint(block.header.tokens[1].span, "sampler instrument has no file", "add an indented line: load \"logic:01 Acoustic Pianos/Steinway Grand Piano 2.exs\"");
            return None;
        }
        Some(def)
    }

    fn tb303_body(&mut self, block: &Block) -> Tb303Def {
        let mut def = Tb303Def::default();
        for line in &block.body {
            let kw = &line.tokens[0];
            let Some(t) = self.arg(line, 1, "value") else { continue };
            match kw.text.as_str() {
                "wave" => match t.text.as_str() {
                    "saw" => def.square = false,
                    "square" => def.square = true,
                    other => self.err_hint(t.span, format!("unknown waveform '{other}'"), "the 303 has: saw, square"),
                },
                "slide" => set(&mut def.slide_time, self.seconds(t, &t.text)),
                "tune" => set(&mut def.tune, self.value(t, &t.text, -24.0, 24.0)),
                "gate" => set(&mut def.gate, self.value(t, &t.text, 0.05, 1.0)),
                name => match Tb303Param::from_name(name) {
                    Some(p) => {
                        let v = self.value::<f32>(t, &t.text, 0.0, 1.0);
                        if let Some(v) = v {
                            match p {
                                Tb303Param::Cutoff => def.cutoff = v,
                                Tb303Param::Resonance => def.resonance = v,
                                Tb303Param::EnvMod => def.env_mod = v,
                                Tb303Param::Decay => def.decay = v,
                                Tb303Param::Accent => def.accent = v,
                                Tb303Param::Drive => def.drive = v,
                            }
                        }
                    }
                    None => self.unknown_keyword(kw, name, "tb303 instruments", &["wave", "cutoff", "resonance", "envmod", "decay", "accent", "drive", "gate", "slide", "tune"]),
                },
            }
            self.extra_tokens(line, 2);
        }
        def
    }

    fn clap_body(&mut self, block: &Block) -> Option<ClapDef> {
        let mut def = ClapDef { plugin: String::new(), plugin_id: None, patch: None, params: Vec::new(), gain_db: 0.0 };
        for line in &block.body {
            let kw = &line.tokens[0];
            match kw.text.as_str() {
                "plugin" | "id" | "patch" | "gain" => {
                    let Some(t) = self.arg(line, 1, "value") else { continue };
                    match kw.text.as_str() {
                        "plugin" => def.plugin = t.text.clone(),
                        "id" => def.plugin_id = Some(t.text.clone()),
                        "patch" => def.patch = Some(t.text.clone()),
                        _ => set(&mut def.gain_db, self.db(t, &t.text)),
                    }
                    self.extra_tokens(line, 2);
                }
                "param" => {
                    let (Some(name), Some(value)) = (self.arg(line, 1, "parameter name"), line.tokens.get(2)) else {
                        self.err_hint(kw.span, "param needs a name and a value", "for example: param \"Filter 1 Cutoff\" 0.6 (list names with: mat plugin-params <plugin>)");
                        continue;
                    };
                    if let Some(v) = self.value::<f64>(value, &value.text, -1e9, 1e9) {
                        def.params.push((name.text.clone(), v));
                    }
                    self.extra_tokens(line, 3);
                }
                other => self.unknown_keyword(kw, other, "CLAP instruments", &["plugin", "id", "patch", "param", "gain"]),
            }
        }
        if def.plugin.is_empty() {
            self.err_hint(block.header.tokens[1].span, "CLAP instrument has no plugin", "add an indented line: plugin \"Surge XT\"");
            return None;
        }
        Some(def)
    }

    fn au_body(&mut self, block: &Block) -> AudioUnitDef {
        let mut def = AudioUnitDef {
            component: ["aumu".into(), "samp".into(), "appl".into()],
            load: None,
            program: None,
            gain_db: 0.0,
        };
        for line in &block.body {
            let kw = &line.tokens[0];
            match kw.text.as_str() {
                "component" => {
                    let codes: Vec<&Token> = line.tokens[1..].iter().collect();
                    if codes.len() != 3 || codes.iter().any(|t| t.text.chars().count() != 4) {
                        self.err_hint(kw.span, "component needs three four-character codes", "for example: component aumu samp appl");
                        continue;
                    }
                    def.component = [codes[0].text.clone(), codes[1].text.clone(), codes[2].text.clone()];
                }
                "load" => {
                    if let Some(t) = self.arg(line, 1, "file path") {
                        def.load = Some(t.text.clone());
                    }
                    self.extra_tokens(line, 2);
                }
                "program" => {
                    if let Some(t) = self.arg(line, 1, "program number") {
                        def.program = self.number(t, 0.0, 127.0).map(|v| v as u8);
                    }
                    self.extra_tokens(line, 2);
                }
                "gain" => {
                    if let Some(t) = self.arg(line, 1, "gain in dB") {
                        set(&mut def.gain_db, self.db(t, &t.text));
                    }
                    self.extra_tokens(line, 2);
                }
                other => self.unknown_keyword(kw, other, "Audio Unit instruments", &["component", "load", "program", "gain"]),
            }
        }
        def
    }

    // ---------------------------------------------------------------- patterns

    fn pattern(&mut self, block: &Block) {
        let line = block.header;
        let Some((name, span)) = self.name(line, "pattern") else { return };
        let bar = self.song.bar_length();
        let mut grid: Option<Whole> = None;
        let mut bars: Option<f64> = None;
        let mut pedal = false;
        for tok in &line.tokens[2..] {
            if tok.text == "grid" {
                grid = Some(1.0 / 16.0);
                continue;
            }
            match tok.text.split_once('=') {
                Some(("grid", v)) => match parse_duration(v) {
                    Some(d) => grid = Some(d),
                    None => self.err_hint(tok.span, format!("invalid grid step '{v}'"), "use a duration such as 1/16 or s"),
                },
                Some(("bars", v)) => bars = self.value(tok, v, 0.0, 10_000.0),
                None if tok.text == "pedal" => pedal = true,
                _ => self.err_hint(tok.span, format!("unexpected '{}'", tok.text), "pattern options are: grid=<step>, bars=<count>, pedal"),
            }
        }

        let result = match grid {
            Some(step) => self.grid_body(block, step),
            None => self.melodic_body(block, bar),
        };
        let Some((mut events, content_len)) = result else { return };
        if pedal {
            // Sustain pedal, lifted at every bar line: notes ring to the end of their bar.
            for ev in &mut events {
                let bar_end = ((ev.start + EPS) / bar).floor() * bar + bar;
                ev.duration = ev.duration.max(bar_end - ev.start);
            }
        }

        if events.is_empty() && bars.is_none() {
            self.err_hint(span, format!("pattern '{name}' is empty"), "add notes, or give it a length with bars=N");
            return;
        }
        let length = match bars {
            Some(b) => {
                let len = b * bar;
                if content_len > len + EPS {
                    self.err(
                        span,
                        format!("pattern '{name}' is {} bars long but declares bars={b}", fmt_num(content_len / bar)),
                    );
                }
                len
            }
            None => ((content_len / bar - EPS).ceil().max(1.0)) * bar,
        };
        self.song.patterns.push(Pattern { name, span, length, events });
    }

    fn melodic_body(&mut self, block: &Block, bar: Whole) -> Option<(Vec<PatternEvent>, Whole)> {
        let mut events = Vec::new();
        let mut pos: Whole = 0.0;
        let mut last_bar_line: Whole = 0.0;
        let mut dur: Whole = 0.25;
        for line in &block.body {
            for tok in &line.tokens {
                if tok.text == "|" {
                    let len = pos - last_bar_line;
                    if (len - bar).abs() > EPS && pos > EPS {
                        let beats = |w: f64| fmt_num(w * 4.0);
                        self.err_hint(
                            tok.span,
                            format!("bar check failed: bar is {} beats long, meter {}/{} needs {}", beats(len), self.song.meter.0, self.song.meter.1, beats(bar)),
                            "1 beat = 1 quarter note; add or remove notes/rests before this '|'",
                        );
                    }
                    last_bar_line = pos;
                    continue;
                }
                let Some(ev) = self.event_token(tok) else { continue };
                if let Some(d) = ev.duration {
                    dur = d;
                }
                for pitch in ev.pitches {
                    events.push(PatternEvent { start: pos, duration: dur, pitch, velocity: ev.velocity, accent: ev.accent, slide: ev.slide });
                }
                pos += dur;
            }
        }
        Some((events, pos))
    }

    fn grid_body(&mut self, block: &Block, step: Whole) -> Option<(Vec<PatternEvent>, Whole)> {
        let mut events = Vec::new();
        let mut row_len: Option<(usize, &str)> = None;
        for line in &block.body {
            let name_tok = &line.tokens[0];
            let Some(pitch) = parse_pitch(&name_tok.text) else {
                self.err_hint(name_tok.span, format!("'{}' is neither a note nor a drum name", name_tok.text), "grid rows start with a note (C4) or a drum (kick, snare, clap, hat, openhat, tom, rim, crash, ride)");
                continue;
            };
            let mut steps = 0usize;
            let mut held: Option<usize> = None; // index into events of the note that '=' extends
            for tok in &line.tokens[1..] {
                if tok.text == "|" {
                    continue;
                }
                for (ci, c) in tok.text.chars().enumerate() {
                    let velocity = match c {
                        'x' => Some(100.0 / 127.0),
                        'X' => Some(1.0),
                        'o' => Some(60.0 / 127.0),
                        '1'..='9' => Some(c.to_digit(10).unwrap() as f32 / 9.0),
                        '.' | '-' | '_' => None,
                        '=' => {
                            match held {
                                Some(i) => events_extend(&mut events, i, step),
                                None => self.err(Span { col: tok.span.col + ci, len: 1, ..tok.span }, "'=' must follow a hit"),
                            }
                            steps += 1;
                            continue;
                        }
                        _ => {
                            self.err_hint(Span { col: tok.span.col + ci, len: 1, ..tok.span }, format!("invalid grid cell '{c}'"), "use x (hit), X (accent), o (soft), 1-9 (velocity), = (hold), . (rest)");
                            steps += 1;
                            continue;
                        }
                    };
                    held = None;
                    if let Some(velocity) = velocity {
                        held = Some(events.len());
                        events.push(PatternEvent { start: steps as f64 * step, duration: step, pitch, velocity, accent: c == 'X', slide: false });
                    }
                    steps += 1;
                }
            }
            match row_len {
                None => row_len = Some((steps, &name_tok.text)),
                Some((n, first)) if n != steps => self.err_hint(
                    name_tok.span,
                    format!("row '{}' has {steps} steps but row '{first}' has {n}", name_tok.text),
                    "all rows of a grid pattern must have the same number of steps",
                ),
                _ => {}
            }
        }
        let steps = row_len.map_or(0, |(n, _)| n);
        Some((events, steps as f64 * step))
    }

    fn event_token(&mut self, tok: &Token) -> Option<EventToken> {
        let markers = tok.text.len() - tok.text.trim_end_matches(['!', '~']).len();
        let (text, marks) = tok.text.split_at(tok.text.len() - markers);
        let accent = marks.contains('!');
        let slide = marks.contains('~');
        let (head, rest) = if let Some(inner) = text.strip_prefix('[') {
            let Some(close) = inner.find(']') else {
                self.err(tok.span, "unclosed '['");
                return None;
            };
            (&inner[..close], &inner[close + 1..])
        } else {
            let end = text.find([':', '@']).unwrap_or(text.len());
            (&text[..end], &text[end..])
        };

        let (dur_str, vel_str) = match rest.find('@') {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let duration = if dur_str.is_empty() {
            None
        } else if let Some(d) = dur_str.strip_prefix(':') {
            match parse_duration(d) {
                Some(v) if v > 0.0 => Some(v),
                _ => {
                    self.err_hint(tok.span, format!("invalid duration '{d}'"), "durations: w h q e s t, dotted q., triplet e3, fractions 3/16, sums h+e");
                    return None;
                }
            }
        } else {
            self.err_hint(tok.span, format!("unexpected '{rest}'"), "write notes like C4:q@90");
            return None;
        };
        let velocity = match vel_str {
            None => 100.0 / 127.0,
            Some(v) => match v.parse::<f32>() {
                Ok(x) if v.contains('.') && (0.0..=1.0).contains(&x) => x,
                Ok(x) if (1.0..=127.0).contains(&x) => x / 127.0,
                _ => {
                    self.err_hint(tok.span, format!("invalid velocity '{v}'"), "velocity is 1-127 (or 0.0-1.0)");
                    return None;
                }
            },
        };

        let pitches = if head == "r" || head == "_" {
            Vec::new()
        } else {
            let mut pitches = Vec::new();
            for part in head.split_whitespace() {
                match parse_pitch(part) {
                    Some(p) => pitches.push(p),
                    None => {
                        let names = DrumKind::ALL.iter().map(|k| k.name());
                        let hint = did_you_mean(part, names).unwrap_or_else(|| "notes look like C4, F#3, Bb2; rests are r; drums are kick, snare, clap, hat, openhat, tom, rim, crash, ride".into());
                        self.err_hint(tok.span, format!("'{part}' is not a note, rest or drum"), hint);
                        return None;
                    }
                }
            }
            if pitches.is_empty() {
                self.err(tok.span, "empty chord");
                return None;
            }
            pitches
        };
        Some(EventToken { pitches, duration, velocity, accent, slide })
    }

    // ---------------------------------------------------------------- tracks

    fn track(&mut self, block: &Block) {
        let line = block.header;
        let Some((name, span)) = self.name(line, "track") else { return };
        self.extra_tokens(line, 2);
        let mut track = Track {
            name,
            span,
            instrument: None,
            gain_db: 0.0,
            pan: 0.0,
            reverb: 0.0,
            delay: 0.0,
            mute: false,
            layer: None,
            eq: None,
            chorus: None,
            sidechain: None,
            audio: None,
            sweeps: Vec::new(),
            steps: Vec::new(),
        };
        for line in &block.body {
            let kw = &line.tokens[0];
            match kw.text.as_str() {
                "instrument" => {
                    if let Some(t) = self.arg(line, 1, "instrument name") {
                        track.instrument = Some((t.text.clone(), t.span));
                    }
                    self.extra_tokens(line, 2);
                }
                "gain" | "pan" | "reverb" | "delay" | "rest" | "at" => {
                    let Some(t) = self.arg(line, 1, "value") else { continue };
                    match kw.text.as_str() {
                        "gain" => set(&mut track.gain_db, self.db(t, &t.text)),
                        "pan" => set(&mut track.pan, self.value(t, &t.text, -1.0, 1.0)),
                        "reverb" => set(&mut track.reverb, self.value(t, &t.text, 0.0, 1.0)),
                        "delay" => set(&mut track.delay, self.value(t, &t.text, 0.0, 1.0)),
                        "rest" => {
                            if let Some(bars) = self.number(t, 0.0, 10_000.0) {
                                track.steps.push(TrackStep::Rest { bars });
                            }
                        }
                        "at" => {
                            if let Some(bar) = self.number(t, 1.0, 10_000.0) {
                                track.steps.push(TrackStep::At { bar });
                            }
                        }
                        _ => unreachable!(),
                    }
                    self.extra_tokens(line, 2);
                }
                "mute" => track.mute = true,
                "layer" => {
                    if let Some(t) = self.arg(line, 1, "layer name") {
                        track.layer = Some(t.text.clone());
                    }
                    self.extra_tokens(line, 2);
                }
                "audio" => {
                    let Some(t) = self.arg(line, 1, "audio file path") else { continue };
                    let mut source = AudioSource { path: t.text.clone(), offset: 0.0 };
                    for (key, val, tok) in self.options(line, 2) {
                        match key {
                            "offset" => set(&mut source.offset, self.seconds(tok, val).map(f64::from)),
                            _ => self.unknown_option(tok, key, "audio", &["offset"]),
                        }
                    }
                    track.audio = Some((source, t.span));
                }
                "eq" => track.eq = Some(self.eq_options(line)),
                "chorus" => {
                    let mut chorus = ChorusSettings::default();
                    for (key, val, tok) in self.options(line, 1) {
                        match key {
                            "mix" => set(&mut chorus.mix, self.value(tok, val, 0.0, 1.0)),
                            "rate" => set(&mut chorus.rate_hz, self.value(tok, val, 0.01, 10.0)),
                            "depth" => set(&mut chorus.depth_ms, self.seconds(tok, val).map(|s| (s * 1000.0).min(20.0))),
                            _ => self.unknown_option(tok, key, "chorus", &["mix", "rate", "depth"]),
                        }
                    }
                    track.chorus = Some(chorus);
                }
                "sidechain" => {
                    let Some(t) = self.arg(line, 1, "name of the track that triggers ducking") else { continue };
                    let mut sc = SidechainSettings { source: t.text.clone(), span: t.span, drum: None, depth: 0.7, attack: 0.005, release: 0.2 };
                    for (key, val, tok) in self.options(line, 2) {
                        match key {
                            "on" => match DrumKind::from_name(val) {
                                Some(d) => sc.drum = Some(d),
                                None => self.err_hint(tok.span, format!("unknown drum '{val}'"), "use kick, snare, clap, hat, openhat, tom or rim"),
                            },
                            "depth" => set(&mut sc.depth, self.value(tok, val, 0.0, 1.0)),
                            "attack" => set(&mut sc.attack, self.seconds(tok, val)),
                            "release" => set(&mut sc.release, self.seconds(tok, val)),
                            _ => self.unknown_option(tok, key, "sidechain", &["on", "depth", "attack", "release"]),
                        }
                    }
                    track.sidechain = Some(sc);
                }
                "sweep" => {
                    let Some(param) = self.arg(line, 1, "parameter name") else { continue };
                    let mut sweep = SweepDef { param: param.text.clone(), span: param.span, from: 0.0, to: 0.0, from_bar: 0.0, to_bar: 0.0 };
                    let mut have = (false, false, false);
                    for (key, val, tok) in self.options(line, 2) {
                        match key {
                            "from" => {
                                set(&mut sweep.from, self.value(tok, val, -1e6, 1e6));
                                have.0 = true;
                            }
                            "to" => {
                                set(&mut sweep.to, self.value(tok, val, -1e6, 1e6));
                                have.1 = true;
                            }
                            "bars" => match val.split_once('-').and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?))) {
                                Some((a, b)) if a >= 1.0 && b >= a => {
                                    sweep.from_bar = a;
                                    sweep.to_bar = b;
                                    have.2 = true;
                                }
                                _ => self.err_hint(tok.span, format!("invalid bar range '{val}'"), "write it like bars=9-16 (the sweep ends at the end of bar 16)"),
                            },
                            _ => self.unknown_option(tok, key, "sweep", &["from", "to", "bars"]),
                        }
                    }
                    if have == (true, true, true) {
                        track.sweeps.push(sweep);
                    } else {
                        self.err_hint(kw.span, "sweep needs from=, to= and bars=", "for example: sweep cutoff from=0.2 to=0.8 bars=9-16");
                    }
                }
                "play" if track.audio.is_some() => {
                    let mut bars = None;
                    let mut repeat = 1;
                    let mut whole = false;
                    for tok in &line.tokens[1..] {
                        if tok.text == "all" {
                            whole = true;
                        } else if let Some(n) = tok.text.strip_prefix('x').and_then(|n| n.parse::<u32>().ok()) {
                            repeat = n.max(1);
                        } else if let Some(range) = tok.text.strip_prefix("bars=") {
                            let parsed = range.split_once('-').and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?)));
                            match parsed {
                                Some((a, b)) if a >= 1.0 && b >= a => bars = Some((a, b)),
                                _ => self.err_hint(tok.span, format!("invalid bar range '{range}'"), "write it like bars=17-24 (inclusive, bar numbers of the audio file)"),
                            }
                        } else {
                            self.err_hint(tok.span, format!("unexpected '{}'", tok.text), "audio tracks play: all, or bars=<from>-<to>, optionally x<count>");
                        }
                    }
                    if !whole && bars.is_none() {
                        self.err_hint(kw.span, "missing what to play", "write: play all, or play bars=17-24");
                        continue;
                    }
                    track.steps.push(TrackStep::PlayAudio { bars, repeat });
                }
                "play" => {
                    let Some(t) = self.arg(line, 1, "pattern name") else { continue };
                    let mut repeat = 1;
                    let mut transpose = 0.0;
                    let mut velocity = 1.0;
                    for tok in &line.tokens[2..] {
                        if let Some(n) = tok.text.strip_prefix('x').and_then(|n| n.parse::<u32>().ok()) {
                            repeat = n.max(1);
                        } else if let Some(("transpose", v)) = tok.text.split_once('=') {
                            set(&mut transpose, self.value(tok, v, -48.0, 48.0));
                        } else if let Some(("vel", v)) = tok.text.split_once('=') {
                            set(&mut velocity, self.value(tok, v, 0.0, 2.0));
                        } else {
                            self.err_hint(tok.span, format!("unexpected '{}'", tok.text), "play options: x<count>, transpose=<semitones>, vel=<scale>");
                        }
                    }
                    track.steps.push(TrackStep::Play { pattern: t.text.clone(), span: t.span, repeat, transpose, velocity });
                }
                other => self.unknown_keyword(
                    kw,
                    other,
                    "tracks",
                    &["instrument", "audio", "layer", "gain", "pan", "reverb", "delay", "eq", "chorus", "sidechain", "sweep", "mute", "play", "rest", "at"],
                ),
            }
        }
        match (&track.instrument, &track.audio) {
            (None, None) => self.err_hint(track.span, format!("track '{}' has no instrument", track.name), "add an indented line: instrument <name>, or audio \"<file>\""),
            (Some(_), Some((_, span))) => self.err(*span, "a track plays either an instrument or an audio file, not both"),
            _ => {}
        }
        self.song.tracks.push(track);
    }

    fn eq_options(&mut self, line: &Line) -> EqSettings {
        let mut eq = EqSettings::default();
        for (key, val, tok) in self.options(line, 1) {
            match key {
                "lowcut" => set(&mut eq.lowcut_hz, self.hz(tok, val)),
                "low" => set(&mut eq.low_db, self.db(tok, val)),
                "lowfreq" => set(&mut eq.low_freq_hz, self.hz(tok, val)),
                "mid" => set(&mut eq.mid_db, self.db(tok, val)),
                "midfreq" => set(&mut eq.mid_freq_hz, self.hz(tok, val)),
                "high" => set(&mut eq.high_db, self.db(tok, val)),
                "highfreq" => set(&mut eq.high_freq_hz, self.hz(tok, val)),
                "highcut" => set(&mut eq.highcut_hz, self.hz(tok, val)),
                _ => self.unknown_option(tok, key, "eq", &["lowcut", "low", "lowfreq", "mid", "midfreq", "high", "highfreq", "highcut"]),
            }
        }
        eq
    }

    fn master(&mut self, block: &Block) {
        self.extra_tokens(block.header, 1);
        let mut m = std::mem::take(&mut self.song.master);
        for line in &block.body {
            let kw = &line.tokens[0];
            match kw.text.as_str() {
                "eq" => {
                    m.eq = Some(self.eq_options(line));
                    continue;
                }
                "width" => {
                    if let Some(t) = self.arg(line, 1, "width 0..2") {
                        set(&mut m.width, self.value(t, &t.text, 0.0, 2.0));
                    }
                    continue;
                }
                _ => {}
            }
            let off = line.tokens.get(1).is_some_and(|t| t.text == "off");
            match kw.text.as_str() {
                "gain" => {
                    if let Some(t) = self.arg(line, 1, "gain in dB") {
                        set(&mut m.gain_db, self.db(t, &t.text));
                    }
                }
                "reverb" => {
                    m.reverb.enabled = !off;
                    for (key, val, tok) in self.options(line, if off { 2 } else { 1 }) {
                        let r = &mut m.reverb;
                        match key {
                            "size" => set(&mut r.size, self.value(tok, val, 0.0, 1.0)),
                            "decay" => set(&mut r.decay, self.value(tok, val, 0.0, 1.0)),
                            "damping" => set(&mut r.damping, self.value(tok, val, 0.0, 1.0)),
                            "predelay" => set(&mut r.predelay_ms, self.seconds(tok, val).map(|s| s * 1000.0)),
                            _ => self.unknown_option(tok, key, "reverb", &["size", "decay", "damping", "predelay"]),
                        }
                    }
                }
                "delay" => {
                    m.delay.enabled = !off;
                    for (key, val, tok) in self.options(line, if off { 2 } else { 1 }) {
                        let d = &mut m.delay;
                        match key {
                            "time" => match parse_duration(val) {
                                Some(v) if v > 0.0 && v <= 2.0 => d.time = v,
                                _ => self.err_hint(tok.span, format!("invalid delay time '{val}'"), "use a note duration such as 3/16, e. or q"),
                            },
                            "feedback" => set(&mut d.feedback, self.value(tok, val, 0.0, 0.95)),
                            "tone" => set(&mut d.tone_hz, self.hz(tok, val)),
                            _ => self.unknown_option(tok, key, "delay", &["time", "feedback", "tone"]),
                        }
                    }
                }
                "comp" => {
                    if off {
                        m.comp = None;
                        continue;
                    }
                    let mut c = CompSettings { threshold_db: -12.0, ratio: 3.0, attack: 0.01, release: 0.15, makeup_db: 0.0 };
                    for (key, val, tok) in self.options(line, 1) {
                        match key {
                            "threshold" => set(&mut c.threshold_db, self.db(tok, val)),
                            "ratio" => set(&mut c.ratio, self.value(tok, val, 1.0, 20.0)),
                            "attack" => set(&mut c.attack, self.seconds(tok, val)),
                            "release" => set(&mut c.release, self.seconds(tok, val)),
                            "makeup" => set(&mut c.makeup_db, self.db(tok, val)),
                            _ => self.unknown_option(tok, key, "comp", &["threshold", "ratio", "attack", "release", "makeup"]),
                        }
                    }
                    m.comp = Some(c);
                }
                "saturation" => {
                    if let Some(t) = self.arg(line, 1, "amount 0..1") {
                        set(&mut m.saturation, self.value(t, &t.text, 0.0, 1.0));
                    }
                }
                "limiter" => {
                    m.limiter.enabled = !off;
                    for (key, val, tok) in self.options(line, if off { 2 } else { 1 }) {
                        let l = &mut m.limiter;
                        match key {
                            "ceiling" => set(&mut l.ceiling_db, self.db(tok, val).map(|v| v.min(0.0))),
                            "release" => set(&mut l.release_ms, self.seconds(tok, val).map(|s| s * 1000.0)),
                            _ => self.unknown_option(tok, key, "limiter", &["ceiling", "release"]),
                        }
                    }
                }
                other => self.unknown_keyword(kw, other, "the master block", &["gain", "eq", "width", "reverb", "delay", "comp", "saturation", "limiter"]),
            }
        }
        self.song.master = m;
    }

    fn check_duplicates(&mut self) {
        let mut dups = Vec::new();
        let mut seen = HashSet::new();
        for i in &self.song.instruments {
            if !seen.insert(i.name.clone()) {
                dups.push((i.span, format!("instrument '{}' is defined twice", i.name)));
            }
        }
        seen.clear();
        for p in &self.song.patterns {
            if !seen.insert(p.name.clone()) {
                dups.push((p.span, format!("pattern '{}' is defined twice", p.name)));
            }
        }
        seen.clear();
        for t in &self.song.tracks {
            if !seen.insert(t.name.clone()) {
                dups.push((t.span, format!("track '{}' is defined twice", t.name)));
            }
        }
        for (span, msg) in dups {
            self.err(span, msg);
        }
    }

    // ---------------------------------------------------------------- values

    /// Yields `key=value` options starting at token `from`.
    fn options<'t>(&mut self, line: &'t Line, from: usize) -> Vec<(&'t str, &'t str, &'t Token)> {
        let mut out = Vec::new();
        for tok in line.tokens.iter().skip(from) {
            match tok.text.split_once('=') {
                Some((k, v)) if !k.is_empty() && !v.is_empty() => out.push((k, v, tok)),
                _ => self.err_hint(tok.span, format!("expected key=value, found '{}'", tok.text), "for example: cutoff=1200"),
            }
        }
        out
    }

    fn unknown_option(&mut self, tok: &Token, key: &str, ctx: &str, valid: &[&str]) {
        let hint = did_you_mean(key, valid.iter().copied()).unwrap_or_else(|| format!("valid options: {}", valid.join(", ")));
        self.err_hint(tok.span, format!("unknown option '{key}' for '{ctx}'"), hint);
    }

    fn unknown_keyword(&mut self, tok: &Token, kw: &str, ctx: &str, valid: &[&str]) {
        let hint = did_you_mean(kw, valid.iter().copied()).unwrap_or_else(|| format!("expected one of: {}", valid.join(", ")));
        self.err_hint(tok.span, format!("unknown setting '{kw}' in {ctx}"), hint);
    }

    fn number(&mut self, tok: &Token, min: f64, max: f64) -> Option<f64> {
        self.value::<f64>(tok, &tok.text, min, max)
    }

    fn value<T: FromF64>(&mut self, tok: &Token, text: &str, min: f64, max: f64) -> Option<T> {
        match text.trim_start_matches('+').parse::<f64>() {
            Ok(v) if v >= min && v <= max => Some(T::from_f64(v)),
            Ok(v) => {
                self.err(tok.span, format!("value {v} is out of range ({min} to {max})"));
                None
            }
            Err(_) => {
                self.err(tok.span, format!("expected a number, found '{text}'"));
                None
            }
        }
    }

    fn db(&mut self, tok: &Token, text: &str) -> Option<f32> {
        let lower = text.to_ascii_lowercase();
        let num = lower.strip_suffix("db").unwrap_or(&lower);
        self.value(tok, num, -96.0, 24.0)
    }

    fn seconds(&mut self, tok: &Token, text: &str) -> Option<f32> {
        if let Some(ms) = text.strip_suffix("ms") {
            return self.value::<f32>(tok, ms, 0.0, 60_000.0).map(|v| v / 1000.0);
        }
        self.value(tok, text.strip_suffix('s').unwrap_or(text), 0.0, 60.0)
    }

    fn hz(&mut self, tok: &Token, text: &str) -> Option<f32> {
        let lower = text.to_ascii_lowercase();
        let lower = lower.strip_suffix("hz").unwrap_or(&lower);
        if let Some(k) = lower.strip_suffix('k') {
            return self.value::<f32>(tok, k, 0.01, 24.0).map(|v| v * 1000.0);
        }
        self.value(tok, lower, 10.0, 24_000.0)
    }
}

struct EventToken {
    pitches: Vec<Pitch>,
    duration: Option<Whole>,
    velocity: f32,
    accent: bool,
    slide: bool,
}

trait FromF64 {
    fn from_f64(v: f64) -> Self;
}
impl FromF64 for f64 {
    fn from_f64(v: f64) -> Self {
        v
    }
}
impl FromF64 for f32 {
    fn from_f64(v: f64) -> Self {
        v as f32
    }
}

fn set<T>(target: &mut T, value: Option<T>) {
    if let Some(v) = value {
        *target = v;
    }
}

fn events_extend(events: &mut [PatternEvent], idx: usize, step: Whole) {
    events[idx].duration += step;
}

fn fmt_num(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn parse_seconds(text: &str) -> Option<f64> {
    if let Some(ms) = text.strip_suffix("ms") {
        return ms.parse::<f64>().ok().map(|v| v / 1000.0);
    }
    text.strip_suffix('s').unwrap_or(text).parse::<f64>().ok()
}

/// Parses durations in whole notes: `w h q e s t`, dotted `q.`, triplets `e3`,
/// fractions `3/16`, and sums `h+e`.
pub fn parse_duration(text: &str) -> Option<Whole> {
    text.split('+').map(parse_duration_part).sum()
}

fn parse_duration_part(text: &str) -> Option<Whole> {
    let dots = text.chars().rev().take_while(|c| *c == '.').count();
    let body = &text[..text.len() - dots];
    let base = if let Some((a, b)) = body.split_once('/') {
        let (a, b) = (a.parse::<f64>().ok()?, b.parse::<f64>().ok()?);
        if b == 0.0 {
            return None;
        }
        a / b
    } else {
        let mut chars = body.chars();
        let base = match chars.next()? {
            'w' => 1.0,
            'h' => 0.5,
            'q' => 0.25,
            'e' => 0.125,
            's' => 0.0625,
            't' => 0.03125,
            _ => return None,
        };
        match chars.as_str() {
            "" => base,
            "3" => base * 2.0 / 3.0,
            _ => return None,
        }
    };
    Some(base * (2.0 - 0.5f64.powi(dots as i32)))
}

pub fn parse_pitch(text: &str) -> Option<Pitch> {
    parse_note(text).map(Pitch::Note).or_else(|| DrumKind::from_name(text).map(Pitch::Drum))
}

/// `C4` = 60, `A4` = 69. Accidentals `#` and `b` may repeat.
pub fn parse_note(text: &str) -> Option<f32> {
    let mut chars = text.chars().peekable();
    let pc = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let mut acc = 0;
    while let Some(c) = chars.peek() {
        match c {
            '#' => acc += 1,
            'b' => acc -= 1,
            _ => break,
        }
        chars.next();
    }
    let octave: i32 = chars.collect::<String>().parse().ok()?;
    let midi = (octave + 1) * 12 + pc + acc;
    (0..=127).contains(&midi).then_some(midi as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes() {
        assert_eq!(parse_note("C4"), Some(60.0));
        assert_eq!(parse_note("A4"), Some(69.0));
        assert_eq!(parse_note("C#4"), Some(61.0));
        assert_eq!(parse_note("Bb3"), Some(58.0));
        assert_eq!(parse_note("bb3"), Some(58.0));
        assert_eq!(parse_note("C-1"), Some(0.0));
        assert_eq!(parse_note("clap"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("q"), Some(0.25));
        assert_eq!(parse_duration("q."), Some(0.375));
        assert_eq!(parse_duration("h+e"), Some(0.625));
        assert_eq!(parse_duration("3/16"), Some(0.1875));
        assert!((parse_duration("e3").unwrap() - 1.0 / 12.0).abs() < 1e-12);
        assert_eq!(parse_duration("z"), None);
    }

    #[test]
    fn bar_check_reports_error() {
        let src = "pattern a\n  C4:q D4 E4 |\n";
        let (song, diags) = parse(src);
        assert!(song.is_none());
        assert!(diags[0].message.contains("bar check failed"), "{}", diags[0].message);
    }

    #[test]
    fn grid_and_melodic_patterns() {
        let src = r#"
tempo 100
pattern beat grid=1/16
  kick  x...x...x...x...
  snare ....x.......X...
pattern mel
  C4:q [E4 G4]:h@120 r:q | D4:w |
"#;
        let (song, diags) = parse(src);
        assert!(diags.is_empty(), "{diags:?}");
        let song = song.unwrap();
        assert_eq!(song.patterns[0].events.len(), 6);
        assert_eq!(song.patterns[0].length, 1.0);
        let mel = &song.patterns[1];
        assert_eq!(mel.events.len(), 4);
        assert_eq!(mel.length, 2.0);
        assert_eq!(mel.events[3].start, 1.0);
    }

    #[test]
    fn mixer_features_and_audio_tracks() {
        let src = r#"
instrument kit drums
instrument pad synth
  osc supersaw detune=0.5 mix=0.7
instrument piano sampler
  load "logic:piano.exs"
  map tom=43 crash=C#3
pattern beat grid=1/4
  kick x.x.
pattern chord
  [C4 E4 G4]:w |
track drums
  instrument kit
  play beat x2
track pad
  instrument pad
  eq lowcut=150 high=+3 highcut=16k
  chorus mix=0.4 depth=5ms
  sidechain drums depth=0.8 release=250ms
  play chord x2
track stem
  audio "stems/bass.wav" offset=0.5s
  play bars=5-8 x2
"#;
        let (song, diags) = parse(src);
        assert!(diags.is_empty(), "{diags:?}");
        let song = song.unwrap();
        let pad = &song.tracks[1];
        assert_eq!(pad.eq.as_ref().unwrap().highcut_hz, 16_000.0);
        assert_eq!(pad.sidechain.as_ref().unwrap().release, 0.25);
        let InstrumentKind::Sampler(piano) = &song.instruments[2].kind else { panic!() };
        assert_eq!(piano.drum_map, vec![(DrumKind::Tom, 43), (DrumKind::Crash, 49)]);

        let timeline = crate::arrange(&song).unwrap();
        let duck = timeline.tracks[1].duck.as_ref().unwrap();
        assert_eq!(duck.times.len(), 4, "two kicks per bar over two bars");
        let clips = &timeline.tracks[2].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].source_start, 0.5 + 4.0 * 2.0);
        assert_eq!(clips[1].at, 8.0);
    }

    #[test]
    fn supersaw_rejects_unison_options() {
        let (_, diags) = parse("instrument a synth\n  osc supersaw voices=7\n");
        assert!(diags[0].message.contains("does not apply to supersaw"));
    }

    #[test]
    fn unknown_option_suggests() {
        let src = "instrument a synth\n  filter lowpass cutof=100\n";
        let (_, diags) = parse(src);
        assert_eq!(diags[0].hint.as_deref(), Some("did you mean 'cutoff'?"));
    }
}
