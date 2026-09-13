use std::path::Path;

use crate::dsp::Rng;
use crate::render::Audio;

#[derive(Debug, Clone, Copy)]
pub enum BitDepth {
    Int16,
    Int24,
    Float32,
}

pub fn write_wav(path: &Path, audio: &Audio, depth: BitDepth) -> Result<(), hound::Error> {
    let (bits, format) = match depth {
        BitDepth::Int16 => (16, hound::SampleFormat::Int),
        BitDepth::Int24 => (24, hound::SampleFormat::Int),
        BitDepth::Float32 => (32, hound::SampleFormat::Float),
    };
    let spec = hound::WavSpec { channels: 2, sample_rate: audio.sample_rate, bits_per_sample: bits, sample_format: format };
    let mut writer = hound::WavWriter::create(path, spec)?;
    let mut rng = Rng::new(0x5EED);
    let frames = audio.left.iter().zip(&audio.right);
    match depth {
        BitDepth::Float32 => {
            for (l, r) in frames {
                writer.write_sample(*l)?;
                writer.write_sample(*r)?;
            }
        }
        BitDepth::Int16 | BitDepth::Int24 => {
            let max = ((1i64 << (bits - 1)) - 1) as f32;
            let mut quantize = |s: f32| {
                // TPDF dither.
                let dither = rng.unit() - rng.unit();
                (s * max + dither).round().clamp(-max - 1.0, max) as i32
            };
            for (l, r) in frames {
                writer.write_sample(quantize(*l))?;
                writer.write_sample(quantize(*r))?;
            }
        }
    }
    writer.finalize()
}
