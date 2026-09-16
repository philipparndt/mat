//! Core of "music as text": parse `.song` files, arrange them into a
//! timeline and render them to audio.

pub mod arrange;
pub mod bars;
pub mod clap_host;
pub mod diag;
pub mod hash;
pub mod dsp;
pub mod encode;
pub mod import;
pub mod instruments;
/// Public for the language server, which reads a file being typed as lines
/// of tokens with their spans — the parser's view before it has an opinion.
pub mod lexer;
pub mod midi;
pub mod model;
pub mod parser;
pub mod placement;
pub mod presets;
pub mod render;
pub mod render_cache;
pub mod sampler;
pub mod stream;
pub mod wav;

pub use arrange::{Timeline, arrange, resolve_paths};
pub use bars::BarRange;
pub use diag::{Diagnostic, Severity};
pub use parser::{Parsed, Source, parse, parse_file, parse_with};
pub use render::{Audio, render};

/// Parses and arranges a song in one step.
pub fn compile(source: &str) -> Result<(Timeline, Vec<Diagnostic>), Vec<Diagnostic>> {
    let (song, mut diags) = parse(source);
    let Some(song) = song else { return Err(diags) };
    match arrange(&song) {
        Ok(timeline) => Ok((timeline, diags)),
        Err(errors) => {
            diags.extend(errors);
            Err(diags)
        }
    }
}
