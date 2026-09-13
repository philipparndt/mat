//! Reader for Logic's EXS sampler instruments (`.exs`).
//!
//! An EXS file is a sequence of chunks, each with an 84-byte header
//! (endianness, type, payload size, index, magic, 64-byte name) followed by
//! the payload. We read zones (key/velocity ranges and sample regions),
//! groups (velocity range, round robin, articulation, choke) and sample
//! references (file name and location).

use std::path::Path;

const HEADER: usize = 84;

#[derive(Debug, Clone)]
pub struct Zone {
    pub name: String,
    pub root: u8,
    pub key_low: u8,
    pub key_high: u8,
    pub vel_low: u8,
    pub vel_high: u8,
    /// False for zones that play at their original pitch on every key.
    pub pitched: bool,
    pub oneshot: bool,
    pub reverse: bool,
    pub fine_cents: i8,
    pub coarse: i8,
    pub pan: i8,
    pub volume_db: i8,
    pub start: u32,
    pub end: u32,
    pub loop_on: bool,
    pub loop_start: u32,
    pub loop_end: u32,
    pub group: u32,
    pub sample: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Group {
    pub name: String,
    pub volume_db: i8,
    pub pan: i8,
    pub mute: bool,
    /// Voices of a group with the same non-zero value cut each other off (hi-hat choke).
    pub exclusive: u8,
    pub vel_low: u8,
    pub vel_high: u8,
    pub release_trigger: bool,
    pub enable_by: EnableBy,
    pub round_robin_pos: u32,
    pub control_number: u8,
    pub control_low: u8,
    pub control_high: u8,
    pub articulation: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EnableBy {
    #[default]
    Always,
    Note,
    RoundRobin,
    Control,
    Bend,
    Channel,
    Articulation,
    Tempo,
}

#[derive(Debug, Clone)]
pub struct SampleRef {
    pub name: String,
    pub frames: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub compressed: bool,
    pub folder: String,
    pub file_name: String,
}

#[derive(Debug, Clone, Default)]
pub struct ExsInstrument {
    pub name: String,
    /// Instrument-wide settings from the parameter chunk.
    pub volume_db: f32,
    pub tune_cents: f32,
    pub zones: Vec<Zone>,
    pub groups: Vec<Group>,
    pub samples: Vec<SampleRef>,
}

struct Reader<'a> {
    data: &'a [u8],
    big_endian: bool,
}

impl Reader<'_> {
    fn u8(&self, at: usize) -> u8 {
        self.data.get(at).copied().unwrap_or(0)
    }
    fn i8(&self, at: usize) -> i8 {
        self.u8(at) as i8
    }
    fn u32(&self, at: usize) -> u32 {
        let Some(b) = self.data.get(at..at + 4) else { return 0 };
        let b = [b[0], b[1], b[2], b[3]];
        if self.big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
    }
    fn text(&self, at: usize, len: usize) -> String {
        let Some(b) = self.data.get(at..(at + len).min(self.data.len())) else { return String::new() };
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).into_owned()
    }
    fn has(&self, at: usize) -> bool {
        at < self.data.len()
    }
}

pub fn read(path: &Path) -> Result<ExsInstrument, String> {
    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse(&data).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn parse(data: &[u8]) -> Result<ExsInstrument, String> {
    let mut inst = ExsInstrument::default();
    let mut pos = 0;
    while pos + HEADER <= data.len() {
        let header = Reader { data: &data[pos..pos + HEADER], big_endian: data[pos] == 0 };
        let magic = header.text(16, 4);
        if !matches!(magic.as_str(), "TBOS" | "SOBT" | "JBOS" | "SOBJ") {
            return Err(format!("not an EXS instrument (unexpected chunk at byte {pos})"));
        }
        let kind = header.u8(3) & 0x0F;
        let mut size = header.u32(4) as usize;
        // Older instruments set a flag bit in the size field.
        if size & 0x8000 != 0 {
            size &= 0x7FFF;
        }
        let name = header.text(20, 64);
        let end = (pos + HEADER + size).min(data.len());
        let r = Reader { data: &data[pos + HEADER..end], big_endian: header.big_endian };
        match kind {
            0x00 => inst.name = name,
            0x01 => inst.zones.push(read_zone(&r, name)),
            0x02 => inst.groups.push(read_group(&r, name)),
            0x03 => inst.samples.push(read_sample(&r, name)),
            0x04 => read_params(&r, &mut inst),
            _ => {}
        }
        pos = pos + HEADER + size;
    }
    if inst.zones.is_empty() {
        return Err("instrument has no zones".into());
    }
    Ok(inst)
}

fn read_zone(r: &Reader, name: String) -> Zone {
    let opts = r.u8(0);
    let loop_opts = r.u8(33);
    Zone {
        name,
        oneshot: opts & 1 != 0,
        pitched: opts & 2 == 0,
        reverse: opts & 4 != 0,
        root: r.u8(1),
        fine_cents: r.i8(2),
        pan: r.i8(3),
        volume_db: r.i8(4),
        key_low: r.u8(6),
        key_high: r.u8(7),
        // Without the velocity-range flag the zone answers to every velocity.
        vel_low: if opts & 8 != 0 { r.u8(9) } else { 0 },
        vel_high: if opts & 8 != 0 { r.u8(10) } else { 127 },
        start: r.u32(12),
        end: r.u32(16),
        loop_start: r.u32(20),
        loop_end: r.u32(24),
        loop_on: loop_opts & 1 != 0,
        coarse: r.i8(80),
        group: r.u32(88),
        sample: r.u32(92),
    }
}

fn read_group(r: &Reader, name: String) -> Group {
    let options = r.u8(3);
    let extended = r.has(84);
    Group {
        name,
        volume_db: r.i8(0),
        pan: r.i8(1),
        mute: options & 16 != 0,
        exclusive: r.u8(4),
        vel_low: r.u8(5),
        vel_high: match r.u8(6) {
            0 => 127,
            v => v,
        },
        release_trigger: r.u8(73) > 0,
        enable_by: if extended {
            match r.u8(84) {
                1 => EnableBy::Note,
                2 => EnableBy::RoundRobin,
                3 => EnableBy::Control,
                4 => EnableBy::Bend,
                5 => EnableBy::Channel,
                6 => EnableBy::Articulation,
                7 => EnableBy::Tempo,
                _ => EnableBy::Always,
            }
        } else {
            EnableBy::Always
        },
        round_robin_pos: if extended { r.u32(80) } else { 0 },
        control_number: r.u8(85),
        control_low: r.u8(86),
        control_high: r.u8(87),
        articulation: r.u8(91),
    }
}

/// Parameter chunk: a count, one byte per parameter id, then 16-bit values.
fn read_params(r: &Reader, inst: &mut ExsInstrument) {
    const MASTER_VOLUME: u8 = 0x07;
    const COARSE_TUNE: u8 = 0x0e;
    const FINE_TUNE: u8 = 0x0f;
    let count = r.u32(0) as usize;
    if 4 + count * 3 > r.data.len() {
        return;
    }
    let mut coarse = 0.0;
    let mut fine = 0.0;
    for i in 0..count {
        let at = 4 + count + 2 * i;
        let b = [r.u8(at), r.u8(at + 1)];
        let value = if r.big_endian { i16::from_be_bytes(b) } else { i16::from_le_bytes(b) } as f32;
        match r.u8(4 + i) {
            MASTER_VOLUME => inst.volume_db = value,
            COARSE_TUNE => coarse = value,
            FINE_TUNE => fine = value,
            _ => {}
        }
    }
    inst.tune_cents = coarse * 100.0 + fine;
}

fn read_sample(r: &Reader, name: String) -> SampleRef {
    let file_name = if r.has(336) { r.text(336, 256) } else { String::new() };
    SampleRef {
        frames: r.u32(4),
        sample_rate: r.u32(8),
        channels: r.u32(16),
        compressed: r.u32(36) > 0,
        folder: r.text(80, 256),
        file_name: if file_name.is_empty() { name.clone() } else { file_name },
        name,
    }
}
