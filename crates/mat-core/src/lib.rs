//! Core of "music as text": parse `.song` files, arrange them into a
//! timeline and render them to audio.

pub mod arrange;
pub mod clap_host;
pub mod diag;
pub mod dsp;
pub mod import;
pub mod instruments;
mod lexer;
pub mod midi;
pub mod model;
pub mod parser;
pub mod presets;
pub mod render;
pub mod sampler;
pub mod wav;

pub use arrange::{Timeline, arrange, resolve_paths};
pub use diag::{Diagnostic, Severity};
pub use parser::parse;
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
