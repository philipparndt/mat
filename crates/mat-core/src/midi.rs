//! Standard MIDI File reader (formats 0 and 1): notes with times in seconds.

#[derive(Debug, Clone, PartialEq)]
pub struct MidiNote {
    pub start: f64,
    pub end: f64,
    pub key: u8,
    pub velocity: u8,
    pub channel: u8,
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.data.get(self.pos).ok_or("unexpected end of MIDI data")?;
        self.pos += 1;
        Ok(b)
    }

    fn varlen(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let b = self.byte()?;
            value = (value << 7) | (b & 0x7F) as u32;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("invalid variable-length number".into())
    }

    fn skip(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.data.len());
    }
}

pub fn read(data: &[u8]) -> Result<Vec<MidiNote>, String> {
    if data.len() < 14 || &data[0..4] != b"MThd" {
        return Err("not a MIDI file".into());
    }
    let tracks = u16::from_be_bytes([data[10], data[11]]) as usize;
    let division = u16::from_be_bytes([data[12], data[13]]);
    if division & 0x8000 != 0 {
        return Err("SMPTE time division is not supported".into());
    }
    let ticks_per_quarter = division as f64;

    // (tick, track, event) collected from all tracks, then timed with the tempo map.
    let mut tempo_changes: Vec<(u64, f64)> = Vec::new();
    let mut raw_notes: Vec<(u64, u64, u8, u8, u8)> = Vec::new();
    let mut pos = 14 + u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize - 6;
    for _ in 0..tracks {
        if pos + 8 > data.len() || &data[pos..pos + 4] != b"MTrk" {
            break;
        }
        let len = u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]) as usize;
        let end = (pos + 8 + len).min(data.len());
        let mut c = Cursor { data: &data[..end], pos: pos + 8 };
        let mut tick = 0u64;
        let mut status = 0u8;
        let mut open: Vec<(u8, u8, u64, u8)> = Vec::new(); // channel, key, start, velocity
        while c.pos < end {
            tick += c.varlen()? as u64;
            let mut b = c.byte()?;
            if b < 0x80 {
                // Running status: this byte is the first data byte.
                c.pos -= 1;
                b = status;
            } else if b < 0xF0 {
                status = b;
            }
            match b {
                0xFF => {
                    let kind = c.byte()?;
                    let len = c.varlen()? as usize;
                    if kind == 0x51 && len == 3 {
                        let us = ((c.byte()? as u32) << 16) | ((c.byte()? as u32) << 8) | c.byte()? as u32;
                        tempo_changes.push((tick, us as f64 / 1_000_000.0));
                    } else {
                        c.skip(len);
                    }
                }
                0xF0 | 0xF7 => {
                    let len = c.varlen()? as usize;
                    c.skip(len);
                }
                0x80..=0xEF => {
                    let channel = b & 0x0F;
                    match b & 0xF0 {
                        0x80 | 0x90 => {
                            let key = c.byte()?;
                            let vel = c.byte()?;
                            let on = b & 0xF0 == 0x90 && vel > 0;
                            if let Some(i) = open.iter().position(|(ch, k, _, _)| *ch == channel && *k == key) {
                                let (_, _, start, v) = open.remove(i);
                                raw_notes.push((start, tick, key, v, channel));
                            }
                            if on {
                                open.push((channel, key, tick, vel));
                            }
                        }
                        0xC0 | 0xD0 => c.skip(1),
                        _ => c.skip(2),
                    }
                }
                _ => return Err(format!("unexpected MIDI status byte {b:#04x}")),
            }
        }
        for (channel, key, start, vel) in open {
            raw_notes.push((start, tick, key, vel, channel));
        }
        pos = end;
    }

    tempo_changes.sort_by_key(|(t, _)| *t);
    let seconds = |tick: u64| -> f64 {
        let mut time = 0.0;
        let mut last_tick = 0u64;
        let mut spq = 0.5; // 120 BPM default
        for &(t, s) in &tempo_changes {
            if t >= tick {
                break;
            }
            time += (t - last_tick) as f64 / ticks_per_quarter * spq;
            last_tick = t;
            spq = s;
        }
        time + (tick - last_tick) as f64 / ticks_per_quarter * spq
    };
    let mut notes: Vec<MidiNote> = raw_notes
        .into_iter()
        .map(|(s, e, key, velocity, channel)| MidiNote { start: seconds(s), end: seconds(e), key, velocity, channel })
        .collect();
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.key.cmp(&b.key)));
    Ok(notes)
}
