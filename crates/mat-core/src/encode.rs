//! Writes rendered audio as WAV, FLAC or AAC in an .m4a, chosen by the file's
//! extension.

use std::path::Path;
use std::process::Command;

use crate::render::Audio;
use crate::wav::{BitDepth, write_wav};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Wav,
    /// Lossless: the samples of the WAV of the same depth, in a half to three
    /// quarters of the space. Integers only, up to 24 bits.
    Flac,
    /// AAC-LC.
    M4a,
}

impl Format {
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match extension.as_str() {
            "wav" => Ok(Format::Wav),
            "flac" => Ok(Format::Flac),
            "m4a" => Ok(Format::M4a),
            _ => Err(format!("{}: unknown audio format, use .wav, .flac or .m4a", path.display())),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::Flac => "flac",
            Format::M4a => "m4a",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Encoding {
    pub format: Format,
    /// Sample depth of WAV and FLAC; AAC is encoded from the float render.
    pub depth: BitDepth,
    /// AAC bit rate in kbit/s, both channels together.
    pub bitrate: u32,
}

impl Encoding {
    pub fn check(&self) -> Result<(), String> {
        if self.format == Format::Flac && matches!(self.depth, BitDepth::Float32) {
            return Err("FLAC holds integers of up to 24 bits: use --bits 16 or 24".into());
        }
        Ok(())
    }

    /// Names what shapes the file's bytes, for caching written files.
    pub fn variant(&self) -> String {
        match self.format {
            Format::Wav | Format::Flac => match self.depth {
                BitDepth::Int16 => "16".into(),
                BitDepth::Int24 => "24".into(),
                BitDepth::Float32 => "32f".into(),
            },
            Format::M4a => format!("{}k", self.bitrate),
        }
    }
}

pub fn write_audio(path: &Path, audio: &Audio, encoding: &Encoding) -> Result<(), String> {
    encoding.check()?;
    match encoding.format {
        Format::Wav => write_wav(path, audio, encoding.depth).map_err(|e| e.to_string()),
        Format::Flac => afconvert(path, audio, encoding.depth, &["-f", "flac", "-d", "flac"]),
        // Constrained VBR at the highest encoder quality. The file records the
        // encoder delay, so players that honour it start and loop without a gap.
        Format::M4a => {
            let bitrate = (encoding.bitrate * 1000).to_string();
            afconvert(path, audio, BitDepth::Float32, &["-f", "m4af", "-d", "aac", "-s", "2", "-q", "127", "-b", &bitrate])
        }
    }
}

/// FLAC and AAC are Apple's encoders, run by macOS `afconvert` on a WAV of
/// the render: the FLAC of the dithered integers, the AAC of the floats.
fn afconvert(path: &Path, audio: &Audio, depth: BitDepth, args: &[&str]) -> Result<(), String> {
    let temporary = std::env::temp_dir().join(format!("mat-{}-{}.wav", std::process::id(), unique()));
    write_wav(&temporary, audio, depth).map_err(|e| e.to_string())?;
    let output = Command::new("afconvert").args(args).arg(&temporary).arg(path).output();
    let _ = std::fs::remove_file(&temporary);
    let output = output.map_err(|e| format!("running afconvert (FLAC and M4A need macOS): {e}"))?;
    if !output.status.success() {
        return Err(format!("afconvert: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(())
}

fn unique() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f64) -> Audio {
        let sample_rate = 48_000;
        let n = (seconds * sample_rate as f64) as usize;
        let left: Vec<f32> = (0..n).map(|i| 0.5 * (i as f32 * 440.0 * std::f32::consts::TAU / sample_rate as f32).sin()).collect();
        let right = left.iter().map(|s| -s).collect();
        Audio { sample_rate, left, right }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mat-encode-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn the_format_is_the_extension() {
        assert_eq!(Format::from_path(Path::new("a/song.FLAC")), Ok(Format::Flac));
        assert_eq!(Format::from_path(Path::new("song.m4a")), Ok(Format::M4a));
        assert!(Format::from_path(Path::new("song.mp3")).is_err());
    }

    fn samples(path: &Path) -> Vec<i32> {
        hound::WavReader::open(path).unwrap().samples::<i32>().map(Result::unwrap).collect()
    }

    #[test]
    fn flac_holds_the_wavs_samples_in_less_space() {
        let audio = tone(1.0);
        let (wav, flac, decoded) = (scratch("tone.wav"), scratch("tone.flac"), scratch("tone-decoded.wav"));
        let encoding = Encoding { format: Format::Flac, depth: BitDepth::Int24, bitrate: 256 };
        write_audio(&flac, &audio, &encoding).unwrap();
        write_audio(&wav, &audio, &Encoding { format: Format::Wav, ..encoding }).unwrap();
        assert!(std::fs::metadata(&flac).unwrap().len() < std::fs::metadata(&wav).unwrap().len() * 3 / 4);
        let status = Command::new("afconvert").args(["-f", "WAVE", "-d", "LEI24"]).arg(&flac).arg(&decoded).status().unwrap();
        assert!(status.success());
        assert_eq!(samples(&decoded), samples(&wav));
        assert!(write_audio(&flac, &audio, &Encoding { depth: BitDepth::Float32, ..encoding }).is_err());
    }

    #[test]
    fn m4a_is_an_mp4_at_about_its_bitrate() {
        let audio = tone(2.0);
        let m4a = scratch("tone.m4a");
        write_audio(&m4a, &audio, &Encoding { format: Format::M4a, depth: BitDepth::Int24, bitrate: 128 }).unwrap();
        let bytes = std::fs::read(&m4a).unwrap();
        assert_eq!(&bytes[4..8], b"ftyp");
        assert!(bytes.len() < 2 * 128_000 / 8 * 2);
    }
}
