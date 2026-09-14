//! Stable hashes: the same bytes give the same number on every machine, in
//! every build, for ever. What the seeds and the render cache are keyed by.
//!
//! Not `std::hash`: its `DefaultHasher` is documented as free to change
//! between Rust releases, and a cache key or a note's take that moved with the
//! compiler would be a song that sounds different after an update.

use std::collections::HashMap;

/// FNV-1a, 64 bits.
#[derive(Clone, Copy)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    pub fn bytes(mut self, bytes: &[u8]) -> Self {
        for byte in bytes {
            self.0 ^= *byte as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self
    }

    pub fn u64(self, value: u64) -> Self {
        self.bytes(&value.to_le_bytes())
    }

    pub fn str(self, text: &str) -> Self {
        // The length first, so "ab" + "c" and "a" + "bc" are different keys.
        self.u64(text.len() as u64).bytes(text.as_bytes())
    }

    pub fn finish(self) -> u64 {
        mix(self.0)
    }
}

/// SplitMix64's finaliser: spreads nearby inputs apart, so seeds 1 and 2 are
/// not two neighbouring takes.
pub fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Mixed into a track's seed for humanize, so its timing and velocity draws
/// are not the same numbers its oscillators start from.
pub const HUMANIZE: u64 = 0x6875_6d61_6e69_7a65;

/// A track's seed: its name, and the `seed` it or the song chose.
pub fn track_seed(name: &str, seed: u64) -> u64 {
    Fnv::default().str(name).u64(seed).finish()
}

/// Gives each note of a track a seed of its own: the track's, the note's time
/// to the nanosecond, its pitch, and how many identical notes came before it.
pub struct NoteIdentities {
    track: u64,
    seen: HashMap<(i64, u32), u64>,
}

impl NoteIdentities {
    pub fn new(track: u64) -> Self {
        NoteIdentities { track, seen: HashMap::new() }
    }

    pub fn next(&mut self, start: f64, midi: f32) -> u64 {
        let key = ((start * 1e9).round() as i64, midi.to_bits());
        let occurrence = self.seen.entry(key).or_insert(0);
        let seed = Fnv::default().u64(self.track).u64(key.0 as u64).u64(key.1 as u64).u64(*occurrence).finish();
        *occurrence += 1;
        seed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written down once, so a change to the hash is a failing test and not a
    /// song that quietly sounds different.
    const PINNED_DRUMS_SEED: &str = "c4df6cb88372480f";

    #[test]
    fn the_hash_does_not_move() {
        assert_eq!(format!("{:016x}", track_seed("drums", 0)), PINNED_DRUMS_SEED);
        assert_ne!(track_seed("drums", 0), track_seed("drums", 1));
        assert_ne!(track_seed("ab", 0), track_seed("a", 0));
    }

    #[test]
    fn a_note_is_identified_by_what_and_when_not_by_its_position() {
        let mut a = NoteIdentities::new(7);
        let first = a.next(1.0, 60.0);
        let mut b = NoteIdentities::new(7);
        let _inserted_before = b.next(0.5, 62.0);
        assert_eq!(b.next(1.0, 60.0), first, "a note added earlier does not change this one");
        assert_ne!(a.next(1.0, 60.0), first, "a second identical note is a different take");
    }
}
