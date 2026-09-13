//! Minimal PCM readers for WAV, AIFF/AIFC and CAF that can read an arbitrary
//! frame range without loading the whole file. Logic's consolidated sample
//! files are several hundred megabytes, and a song only needs a few regions.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Int,
    Float,
}

#[derive(Debug, Clone)]
pub struct AudioFile {
    pub path: PathBuf,
    pub sample_rate: f64,
    pub channels: usize,
    pub frames: u64,
    bits: u16,
    encoding: Encoding,
    big_endian: bool,
    data_offset: u64,
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}
fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// 80-bit IEEE extended float (AIFF sample rate).
fn extended_to_f64(b: &[u8]) -> f64 {
    let exponent = (((b[0] as i32) & 0x7F) << 8) | b[1] as i32;
    let mantissa = u64::from_be_bytes([b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9]]);
    if exponent == 0 && mantissa == 0 {
        return 0.0;
    }
    let sign = if b[0] & 0x80 != 0 { -1.0 } else { 1.0 };
    sign * mantissa as f64 * 2f64.powi(exponent - 16383 - 63)
}

impl AudioFile {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut head = [0u8; 12];
        file.read_exact(&mut head).map_err(|_| format!("{}: file too short", path.display()))?;
        let result = match &head[0..4] {
            b"RIFF" if &head[8..12] == b"WAVE" => Self::wav(&mut file),
            b"FORM" if &head[8..12] == b"AIFF" || &head[8..12] == b"AIFC" => Self::aiff(&mut file, &head[8..12] == b"AIFC"),
            b"caff" => Self::caf(&mut file, file_len),
            _ => Err("unsupported audio format (expected WAV, AIFF or CAF)".to_string()),
        };
        let mut af = result.map_err(|e| format!("{}: {e}", path.display()))?;
        af.path = path.to_path_buf();
        let bytes_per_frame = (af.bits as u64 / 8) * af.channels as u64;
        let available = file_len.saturating_sub(af.data_offset) / bytes_per_frame.max(1);
        if af.frames == 0 || af.frames > available {
            af.frames = available;
        }
        Ok(af)
    }

    fn empty() -> Self {
        Self {
            path: PathBuf::new(),
            sample_rate: 0.0,
            channels: 0,
            frames: 0,
            bits: 0,
            encoding: Encoding::Int,
            big_endian: false,
            data_offset: 0,
        }
    }

    fn wav(file: &mut File) -> Result<Self, String> {
        let mut af = Self::empty();
        let mut have_fmt = false;
        let mut chunk = [0u8; 8];
        while file.read_exact(&mut chunk).is_ok() {
            let size = le32(&chunk[4..8]) as u64;
            let body_start = file.stream_position().map_err(|e| e.to_string())?;
            match &chunk[0..4] {
                b"fmt " => {
                    let mut fmt = vec![0u8; size.min(64) as usize];
                    file.read_exact(&mut fmt).map_err(|e| e.to_string())?;
                    let mut tag = le16(&fmt[0..2]);
                    if tag == 0xFFFE && fmt.len() >= 26 {
                        tag = le16(&fmt[24..26]);
                    }
                    af.encoding = match tag {
                        1 => Encoding::Int,
                        3 => Encoding::Float,
                        other => return Err(format!("unsupported WAV encoding {other}")),
                    };
                    af.channels = le16(&fmt[2..4]) as usize;
                    af.sample_rate = le32(&fmt[4..8]) as f64;
                    af.bits = le16(&fmt[14..16]);
                    have_fmt = true;
                }
                b"data" => {
                    af.data_offset = body_start;
                    if have_fmt {
                        af.frames = size / ((af.bits as u64 / 8) * af.channels as u64).max(1);
                        return Ok(af);
                    }
                }
                _ => {}
            }
            file.seek(SeekFrom::Start(body_start + size + size % 2)).map_err(|e| e.to_string())?;
        }
        Err("WAV file has no audio data".into())
    }

    fn aiff(file: &mut File, compressed_form: bool) -> Result<Self, String> {
        let mut af = Self::empty();
        af.big_endian = true;
        let mut chunk = [0u8; 8];
        let mut have_comm = false;
        while file.read_exact(&mut chunk).is_ok() {
            let size = be32(&chunk[4..8]) as u64;
            let body_start = file.stream_position().map_err(|e| e.to_string())?;
            match &chunk[0..4] {
                b"COMM" => {
                    let mut comm = vec![0u8; size.min(64) as usize];
                    file.read_exact(&mut comm).map_err(|e| e.to_string())?;
                    af.channels = be16(&comm[0..2]) as usize;
                    af.frames = be32(&comm[2..6]) as u64;
                    af.bits = be16(&comm[6..8]);
                    af.sample_rate = extended_to_f64(&comm[8..18]);
                    if compressed_form && comm.len() >= 22 {
                        match &comm[18..22] {
                            b"NONE" | b"twos" => {}
                            b"sowt" => af.big_endian = false,
                            b"fl32" | b"FL32" => {
                                af.encoding = Encoding::Float;
                                af.bits = 32;
                            }
                            b"fl64" | b"FL64" => {
                                af.encoding = Encoding::Float;
                                af.bits = 64;
                            }
                            other => return Err(format!("unsupported AIFC compression '{}'", String::from_utf8_lossy(other))),
                        }
                    }
                    have_comm = true;
                }
                b"SSND" => {
                    let mut offset = [0u8; 8];
                    file.read_exact(&mut offset).map_err(|e| e.to_string())?;
                    af.data_offset = body_start + 8 + be32(&offset[0..4]) as u64;
                    if have_comm {
                        return Ok(af);
                    }
                }
                _ => {}
            }
            file.seek(SeekFrom::Start(body_start + size + size % 2)).map_err(|e| e.to_string())?;
        }
        Err("AIFF file has no audio data".into())
    }

    fn caf(file: &mut File, file_len: u64) -> Result<Self, String> {
        let mut af = Self::empty();
        let mut have_desc = false;
        file.seek(SeekFrom::Start(8)).map_err(|e| e.to_string())?;
        let mut chunk = [0u8; 12];
        while file.read_exact(&mut chunk).is_ok() {
            let size = i64::from_be_bytes(chunk[4..12].try_into().unwrap());
            let body_start = file.stream_position().map_err(|e| e.to_string())?;
            match &chunk[0..4] {
                b"desc" => {
                    let mut desc = [0u8; 32];
                    file.read_exact(&mut desc).map_err(|e| e.to_string())?;
                    af.sample_rate = f64::from_be_bytes(desc[0..8].try_into().unwrap());
                    if &desc[8..12] != b"lpcm" {
                        return Err(format!("compressed CAF ('{}') is not supported", String::from_utf8_lossy(&desc[8..12])));
                    }
                    let flags = be32(&desc[12..16]);
                    af.encoding = if flags & 1 != 0 { Encoding::Float } else { Encoding::Int };
                    af.big_endian = flags & 2 == 0;
                    af.channels = be32(&desc[24..28]) as usize;
                    af.bits = be32(&desc[28..32]) as u16;
                    have_desc = true;
                }
                b"data" => {
                    // 4 bytes of edit count precede the audio.
                    af.data_offset = body_start + 4;
                    let data_len = if size < 0 { file_len - af.data_offset } else { size as u64 - 4 };
                    if have_desc {
                        af.frames = data_len / ((af.bits as u64 / 8) * af.channels as u64).max(1);
                        return Ok(af);
                    }
                }
                _ => {}
            }
            if size < 0 {
                break;
            }
            file.seek(SeekFrom::Start(body_start + size as u64)).map_err(|e| e.to_string())?;
        }
        Err("CAF file has no audio data".into())
    }

    /// Reads frames `[start, end)` as stereo float (mono is duplicated).
    /// Frames outside the file read as silence.
    pub fn read_stereo(&self, start: i64, end: i64) -> Result<(Vec<f32>, Vec<f32>), String> {
        let n = (end - start).max(0) as usize;
        let mut left = vec![0.0f32; n];
        let mut right = vec![0.0f32; n];
        let from = start.max(0) as u64;
        let to = (end.max(0) as u64).min(self.frames);
        if to <= from {
            return Ok((left, right));
        }
        let bytes = (self.bits / 8) as usize;
        let frame_bytes = bytes * self.channels;
        let mut buf = vec![0u8; (to - from) as usize * frame_bytes];
        let mut file = File::open(&self.path).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(self.data_offset + from * frame_bytes as u64)).map_err(|e| e.to_string())?;
        file.read_exact(&mut buf).map_err(|e| format!("{}: {e}", self.path.display()))?;

        let skip = (from as i64 - start) as usize;
        for (i, frame) in buf.chunks_exact(frame_bytes).enumerate() {
            let l = self.decode(&frame[0..bytes]);
            let r = if self.channels > 1 { self.decode(&frame[bytes..2 * bytes]) } else { l };
            left[skip + i] = l;
            right[skip + i] = r;
        }
        Ok((left, right))
    }

    #[inline]
    fn decode(&self, b: &[u8]) -> f32 {
        let be = self.big_endian;
        match (self.encoding, self.bits) {
            (Encoding::Int, 8) => (b[0] as i8) as f32 / 128.0,
            (Encoding::Int, 16) => {
                let v = if be { i16::from_be_bytes([b[0], b[1]]) } else { i16::from_le_bytes([b[0], b[1]]) };
                v as f32 / 32768.0
            }
            (Encoding::Int, 24) => {
                let v = if be {
                    i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8
                } else {
                    i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8
                };
                v as f32 / 8_388_608.0
            }
            (Encoding::Int, 32) => {
                let v = if be { i32::from_be_bytes(b.try_into().unwrap()) } else { i32::from_le_bytes(b.try_into().unwrap()) };
                v as f32 / 2_147_483_648.0
            }
            (Encoding::Float, 32) => {
                if be { f32::from_be_bytes(b.try_into().unwrap()) } else { f32::from_le_bytes(b.try_into().unwrap()) }
            }
            (Encoding::Float, 64) => {
                (if be { f64::from_be_bytes(b.try_into().unwrap()) } else { f64::from_le_bytes(b.try_into().unwrap()) }) as f32
            }
            _ => 0.0,
        }
    }
}
