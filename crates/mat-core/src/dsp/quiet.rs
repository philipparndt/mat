//! Skipping silence in the send effects.
//!
//! A layer of a song is silent for most of it — a hook plays in the choruses,
//! a riser before each drop — and the delay and the plate reverb used to run
//! over every sample of it: 0.75 s per layer on a four-minute song, ten times
//! for ten stems. An effect whose input is zero and whose every buffer and
//! filter has decayed below `QUIET` is cleared to exactly zero; from then on,
//! while the input stays zero, its output is zero and only its LFO phase
//! moves, so a block costs a comparison instead of a thousand trips round the
//! tank. The first non-zero input picks up from a state that differs from the
//! unskipped one by less than `QUIET`, and the same way every time.

/// Samples looked at together: small enough that a skip starts soon after the
/// tail dies, large enough that the check is cheap beside the work it saves.
pub const BLOCK: usize = 1024;

/// Below this, a buffer's contents are taken to be silence: -140 dBFS, under
/// the dither of a 24-bit file. Not lower: a plate's tail takes a second and a
/// half per 20 dB to fall, and a layer that rests eight bars between phrases
/// never reaches -180 dB before its next note, so it would never skip at all.
pub const QUIET: f32 = 1e-7;

/// Whether both inputs are exactly zero over `start..end`; a missing sample
/// is zero, as the effects read it.
#[inline]
pub fn is_silent(in_l: &[f32], in_r: &[f32], start: usize, end: usize) -> bool {
    (start..end).all(|i| in_l.get(i).copied().unwrap_or(0.0) == 0.0 && in_r.get(i).copied().unwrap_or(0.0) == 0.0)
}
