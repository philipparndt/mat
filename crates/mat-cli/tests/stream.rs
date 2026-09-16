//! `mat render --stream` as an editor sees it: a file that grows, a small
//! JSON that says how much of it can be read, and an ordinary render at the
//! end of it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Long enough to take a second or two, with a reverb and a delay whose tails
/// have to carry across every boundary, and a master that limits.
const SONG: &str = "tempo 126
meter 4/4
instrument lead synth
  osc saw voices=5 spread=16
  drift 8
  lfo pitch rate=5 depth=8
instrument pad synth
  osc square voices=3 spread=10
instrument kit drums
pattern tune
  C4:e D4 E4 F4 G4 A4 B4 C5 | C5:e B4 A4 G4 F4 E4 D4 C4 |
pattern chords
  [C3 E3 G3]:h [F3 A3 C4]:h |
pattern beat grid=1/16
  kick  X...x...X...x...
  hat   x.x.x.x.x.x.x.x.
  snare ....X.......X...
track lead
  instrument lead
  reverb 0.4
  delay 0.3
  play tune x48
track pad
  instrument pad
  layer beds
  reverb 0.5
  play chords x48
track drums
  instrument kit
  layer drums
  play beat x96
master
  gain 1
  limiter ceiling=-1 release=80ms
";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mat-stream-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    dir
}

/// The frames a WAV on disk actually holds, by its own header.
fn readable(path: &Path) -> Option<(usize, usize)> {
    let bytes = std::fs::read(path).ok()?;
    let mut at = 12;
    let mut block_align = 0usize;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        if &bytes[at..at + 4] == b"fmt " {
            block_align = u16::from_le_bytes(bytes[at + 8 + 12..at + 8 + 14].try_into().ok()?) as usize;
        }
        if &bytes[at..at + 4] == b"data" {
            // Only what is really there: a header can say more than the file
            // holds, and this is the check that would catch it.
            let held = (bytes.len() - (at + 8)).min(size);
            return Some((held / block_align.max(1), at + 8));
        }
        at += 8 + size + (size & 1);
    }
    None
}

fn samples(path: &Path, frames: usize) -> Vec<u8> {
    let (_, data) = readable(path).expect("a readable wav");
    let bytes = std::fs::read(path).expect("the file");
    bytes[data..data + frames * 6].to_vec()
}

/// The JSON never claims more than can be read, it only grows, it ends
/// `finished: true`, and what it claimed on the way is what the finished file
/// holds there.
#[test]
fn the_json_never_claims_more_of_the_wav_than_is_there() {
    let dir = scratch("grows");
    let song = dir.join("stream.song");
    std::fs::File::create(&song).expect("a song").write_all(SONG.as_bytes()).expect("written");
    let wav = dir.join("streamed.wav");
    let status = dir.join("streamed.stream.json");
    let _ = std::fs::remove_file(&wav);
    let _ = std::fs::remove_file(&status);

    let mut child = Command::new(env!("CARGO_BIN_EXE_mat"))
        .args(["render".as_ref(), song.as_os_str(), "-o".as_ref(), wav.as_os_str(), "--stream".as_ref()])
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("mat runs");

    // Every claim seen while it rendered, with the samples that backed it.
    let mut seen: Vec<(usize, Vec<u8>)> = Vec::new();
    loop {
        if let Ok(text) = std::fs::read_to_string(&status)
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
        {
            let claimed = json["frames_written"].as_u64().expect("frames_written") as usize;
            let finished = json["finished"].as_bool().expect("finished");
            assert_eq!(json["channels"].as_u64(), Some(2));
            assert_eq!(json["bytes_per_frame"].as_u64(), Some(6));
            if !finished && claimed > 0 {
                let (there, _) = readable(&wav).expect("a readable wav");
                let over = std::fs::read_to_string(&status)
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .is_some_and(|j| j["finished"].as_bool() == Some(true));
                // Unless the render ended between the two reads: its last act
                // is to cut the trailing silence, and the file does shrink.
                if !over {
                    assert!(there >= claimed, "the json claims {claimed} frames and the file holds {there}");
                    if seen.last().is_none_or(|(last, _)| *last != claimed) {
                        assert!(seen.last().is_none_or(|(last, _)| *last < claimed), "the json went backwards");
                        seen.push((claimed, samples(&wav, claimed.min(48_000))));
                    }
                }
            }
        }
        if let Some(status) = child.try_wait().expect("a child") {
            assert!(status.success(), "mat render --stream failed");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(3));
    }

    assert!(seen.len() >= 2, "the render was never seen part-way: {} claims", seen.len());
    let text = std::fs::read_to_string(&status).expect("the json");
    let json: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(json["finished"].as_bool(), Some(true), "the render ended unfinished");
    let claimed = json["frames_written"].as_u64().expect("frames_written") as usize;
    let (there, _) = readable(&wav).expect("a readable wav");
    assert_eq!(there, claimed, "the finished json claims {claimed} frames and the file holds {there}");
    assert!(claimed > 40 * 48_000, "a song of some length: {claimed} frames");

    // What was played while it rendered is what the finished file holds —
    // every claim, at the samples it claimed.
    for (at, bytes) in &seen {
        let frames = at.min(&48_000);
        assert_eq!(&samples(&wav, *frames), bytes, "the first {frames} frames changed after they were given out");
    }

    // And the finished file is the file an ordinary render writes.
    let ordinary = dir.join("ordinary.wav");
    let run = Command::new(env!("CARGO_BIN_EXE_mat"))
        .args(["render".as_ref(), song.as_os_str(), "-o".as_ref(), ordinary.as_os_str()])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("mat runs");
    assert!(run.success());
    assert!(std::fs::read(&wav).expect("streamed") == std::fs::read(&ordinary).expect("ordinary"), "a streamed render is not the ordinary one");
    let _ = std::fs::remove_dir_all(&dir);
}

/// What streaming cannot do, said in a way that says what to do instead.
#[test]
fn streaming_says_what_it_cannot_write() {
    let dir = scratch("refused");
    let song = dir.join("refused.song");
    std::fs::File::create(&song).expect("a song").write_all(SONG.as_bytes()).expect("written");
    let says = |args: &[&std::ffi::OsStr]| -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_mat")).args(args).output().expect("mat runs");
        assert!(!out.status.success(), "it should have been refused");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let flac = dir.join("no.flac");
    let wav = dir.join("no.wav");
    let said = says(&["render".as_ref(), song.as_os_str(), "-o".as_ref(), flac.as_os_str(), "--stream".as_ref()]);
    assert!(said.contains(".wav"), "{said}");
    let said = says(&["render".as_ref(), song.as_os_str(), "-o".as_ref(), wav.as_os_str(), "--stream".as_ref(), "--loop".as_ref()]);
    assert!(said.contains("--loop"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}
