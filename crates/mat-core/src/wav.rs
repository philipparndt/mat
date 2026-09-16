use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use crate::dsp::Rng;
use crate::render::Audio;

#[derive(Debug, Clone, Copy)]
pub enum BitDepth {
    Int16,
    Int24,
    Float32,
}

impl BitDepth {
    pub fn bits(self) -> u16 {
        match self {
            BitDepth::Int16 => 16,
            BitDepth::Int24 => 24,
            BitDepth::Float32 => 32,
        }
    }

    /// Bytes one frame of stereo takes.
    pub fn bytes_per_frame(self) -> usize {
        self.bits() as usize / 8 * 2
    }
}

/// The dither of an integer WAV: TPDF noise under the last bit, the same
/// sequence every time, so a file written twice is the same file.
struct Dither {
    rng: Rng,
    max: f32,
}

impl Dither {
    fn new(depth: BitDepth) -> Option<Self> {
        match depth {
            BitDepth::Float32 => None,
            _ => Some(Dither { rng: Rng::new(0x5EED), max: ((1i64 << (depth.bits() - 1)) - 1) as f32 }),
        }
    }

    fn quantize(&mut self, s: f32) -> i32 {
        let dither = self.rng.unit() - self.rng.unit();
        (s * self.max + dither).round().clamp(-self.max - 1.0, self.max) as i32
    }
}

pub fn write_wav(path: &Path, audio: &Audio, depth: BitDepth) -> Result<(), hound::Error> {
    let (bits, format) = match depth {
        BitDepth::Int16 => (16, hound::SampleFormat::Int),
        BitDepth::Int24 => (24, hound::SampleFormat::Int),
        BitDepth::Float32 => (32, hound::SampleFormat::Float),
    };
    let spec = hound::WavSpec { channels: 2, sample_rate: audio.sample_rate, bits_per_sample: bits, sample_format: format };
    let mut writer = hound::WavWriter::create(path, spec)?;
    let frames = audio.left.iter().zip(&audio.right);
    match Dither::new(depth) {
        None => {
            for (l, r) in frames {
                writer.write_sample(*l)?;
                writer.write_sample(*r)?;
            }
        }
        Some(mut dither) => {
            for (l, r) in frames {
                writer.write_sample(dither.quantize(*l))?;
                writer.write_sample(dither.quantize(*r))?;
            }
        }
    }
    writer.finalize()
}

/// A WAV written while it is still being rendered — see `crate::stream`.
///
/// Every stretch appended leaves a complete, readable WAV of everything
/// written so far: the RIFF and data sizes are brought up to date with it, so
/// a player that opens the file between two stretches hears what is there and
/// nothing else. The dither is the dither [`write_wav`] would have laid down,
/// carried from stretch to stretch, so the finished file holds the samples an
/// ordinary render writes.
pub struct WavStream {
    file: BufWriter<File>,
    depth: BitDepth,
    frames: usize,
    dither: Option<Dither>,
    /// Where the data chunk's size field is, and where its samples begin.
    size_field: u64,
    data_offset: u64,
    /// Where each stretch began, and the dither as it stood there, so the end
    /// can be written again when the render turns out to keep less than was
    /// given out (see [`WavStream::finish`]).
    marks: Vec<(usize, Option<Rng>)>,
}

impl WavStream {
    /// Starts a WAV that will be written a stretch at a time.
    ///
    /// The header is the header [`write_wav`] would have written — it is
    /// written by the same library, for no samples, and its two size fields
    /// are brought up to date as the file grows. So a finished streamed render
    /// is not merely the same samples as an ordinary one: it is the same file.
    pub fn create(path: &Path, sample_rate: u32, depth: BitDepth) -> std::io::Result<WavStream> {
        let format = match depth {
            BitDepth::Float32 => hound::SampleFormat::Float,
            _ => hound::SampleFormat::Int,
        };
        let spec = hound::WavSpec { channels: 2, sample_rate, bits_per_sample: depth.bits(), sample_format: format };
        let failed = |e: hound::Error| std::io::Error::other(e.to_string());
        hound::WavWriter::create(path, spec).and_then(|w| w.finalize()).map_err(failed)?;

        let header = std::fs::read(path)?;
        let size_field = chunk(&header, b"data").ok_or_else(|| std::io::Error::other("the header has no data chunk"))?;
        let data_offset = size_field + 4;
        let mut file = BufWriter::new(std::fs::OpenOptions::new().read(true).write(true).open(path)?);
        file.seek(SeekFrom::Start(data_offset))?;
        Ok(WavStream { file, depth, frames: 0, dither: Dither::new(depth), marks: Vec::new(), size_field, data_offset })
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.depth.bytes_per_frame()
    }

    /// Where the samples begin, for a reader that goes at the bytes itself.
    pub fn data_offset(&self) -> u64 {
        self.data_offset
    }

    /// Appends a stretch and leaves the file a readable WAV of everything
    /// written so far.
    pub fn append(&mut self, left: &[f32], right: &[f32]) -> std::io::Result<()> {
        self.marks.push((self.frames, self.dither.as_ref().map(|d| d.rng.clone())));
        self.write_frames(left, right)?;
        self.frames += left.len();
        self.sizes()
    }

    fn write_frames(&mut self, left: &[f32], right: &[f32]) -> std::io::Result<()> {
        let mut bytes = Vec::with_capacity(left.len() * self.depth.bytes_per_frame());
        for (l, r) in left.iter().zip(right) {
            for s in [*l, *r] {
                match &mut self.dither {
                    None => bytes.extend_from_slice(&s.to_le_bytes()),
                    Some(dither) => {
                        let v = dither.quantize(s).to_le_bytes();
                        bytes.extend_from_slice(&v[..self.depth.bits() as usize / 8]);
                    }
                }
            }
        }
        self.file.write_all(&bytes)
    }

    /// Brings the RIFF and data sizes up to what has been written.
    fn sizes(&mut self) -> std::io::Result<()> {
        let data = (self.frames * self.depth.bytes_per_frame()) as u32;
        let at = self.file.stream_position()?;
        self.file.seek(SeekFrom::Start(4))?;
        self.file.write_all(&(self.data_offset as u32 - 8 + data).to_le_bytes())?;
        self.file.seek(SeekFrom::Start(self.size_field))?;
        self.file.write_all(&data.to_le_bytes())?;
        self.file.seek(SeekFrom::Start(at))?;
        self.file.flush()
    }

    /// The render is over and kept `audio`, which is what was given out with
    /// its trailing near-silence cut and its last samples faded.
    ///
    /// `changed_from` is the first frame of `audio` that may differ from what
    /// was appended — the front of that fade. The end is written again from
    /// the start of whichever stretch that frame falls in, so that the dither
    /// carries on from where it stood, and the file is cut to `audio`.
    /// Everything before that stretch is left alone: it was final when it was
    /// written.
    pub fn finish(&mut self, audio: &Audio, changed_from: usize) -> std::io::Result<()> {
        let frames = audio.left.len();
        let target = changed_from.min(frames);
        let (from, rng) = match self.marks.iter().rev().find(|(at, _)| *at <= target) {
            Some((at, rng)) => (*at, rng.clone()),
            None => (0, Dither::new(self.depth).map(|d| d.rng)),
        };
        self.dither = rng.map(|rng| Dither { rng, max: ((1i64 << (self.depth.bits() - 1)) - 1) as f32 });
        self.file.seek(SeekFrom::Start(self.data_offset + (from * self.depth.bytes_per_frame()) as u64))?;
        self.write_frames(&audio.left[from..], &audio.right[from..])?;
        self.frames = frames;
        self.sizes()?;
        let end = self.data_offset + (frames * self.depth.bytes_per_frame()) as u64;
        self.file.get_ref().set_len(end)
    }
}

/// Where a chunk's size field is in a RIFF header, if the chunk is there.
fn chunk(header: &[u8], id: &[u8; 4]) -> Option<u64> {
    let mut at = 12;
    while at + 8 <= header.len() {
        let size = u32::from_le_bytes(header[at + 4..at + 8].try_into().ok()?) as usize;
        if &header[at..at + 4] == id {
            return Some(at as u64 + 4);
        }
        at += 8 + size + (size & 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(frames: usize) -> Audio {
        let left: Vec<f32> = (0..frames).map(|i| 0.7 * (i as f32 * 0.01).sin()).collect();
        let right = left.iter().map(|s| -s * 0.6).collect();
        Audio { sample_rate: 48_000, left, right }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mat-wav-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch folder");
        dir.join(name)
    }

    /// A WAV written a stretch at a time holds the samples one written in a
    /// single call holds — dither and all — however the stretches fall, and
    /// however much of the end the render turns out to drop.
    #[test]
    fn a_streamed_wav_holds_the_samples_a_written_one_holds() {
        for depth in [BitDepth::Int16, BitDepth::Int24, BitDepth::Float32] {
            for kept in [7_000usize, 9_000, 9_216, 9_300, 9_913] {
                let whole = audio(9_913);
                let (streamed, written) = (scratch("streamed.wav"), scratch("written.wav"));
                let mut out = WavStream::create(&streamed, whole.sample_rate, depth).expect("a file");
                let mut at = 0;
                for length in [1usize, 1023, 4096, 4096, 4096] {
                    let to = (at + length).min(whole.left.len());
                    out.append(&whole.left[at..to], &whole.right[at..to]).expect("a stretch");
                    at = to;
                    let samples = read(&streamed, depth);
                    assert_eq!(samples.len(), to * 2, "the header says what is there");
                }
                let final_audio = Audio { sample_rate: whole.sample_rate, left: whole.left[..kept].to_vec(), right: whole.right[..kept].to_vec() };
                // As a render does it: the last 2400 frames are the fade, so
                // everything from there on may have changed since it was
                // appended — including, for `kept` just past a stretch
                // boundary, samples written a stretch ago.
                out.finish(&final_audio, kept.saturating_sub(2400)).expect("the end");
                write_wav(&written, &final_audio, depth).expect("a file");
                assert_eq!(read(&streamed, depth), read(&written, depth), "{} bits, {kept} frames kept", depth.bits());
                assert!(std::fs::read(&streamed).unwrap() == std::fs::read(&written).unwrap(), "the same file, {} bits, {kept} frames kept", depth.bits());
            }
        }
    }

    fn read(path: &Path, depth: BitDepth) -> Vec<i64> {
        let mut reader = hound::WavReader::open(path).expect("a readable wav");
        match depth {
            BitDepth::Float32 => reader.samples::<f32>().map(|s| s.expect("a sample").to_bits() as i64).collect(),
            _ => reader.samples::<i32>().map(|s| s.expect("a sample") as i64).collect(),
        }
    }
}
