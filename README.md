# Music as Text

Write songs as plain text, by hand or with AI, and render them to audio from
the command line.

```
pattern beat grid=1/16
  kick  X.....x.X.....x.
  snare ....X.......X...

pattern verse
  A4:q A4:e A4 A4:q A4:e A4 | A4:q D4 F4 A4 |
```

See [docs/FORMAT.md](docs/FORMAT.md) for the language.

## Usage

```sh
cargo build --release

./target/release/mat check  examples/drunken-sailor.song   # validate
./target/release/mat render examples/drunken-sailor.song   # -> examples/drunken-sailor.wav
./target/release/mat play   examples/drunken-sailor.song   # render and play
./target/release/mat export examples/drunken-sailor.song   # arranged timeline as JSON
./target/release/mat inspect "logic:01 Acoustic Pianos/Steinway Grand Piano 2.exs"
./target/release/mat presets                                # built-in instrument and master presets
```

Audio Unit instruments (macOS) need the Swift host:

```sh
swift build -c release --package-path swift
./swift/.build/release/mat-au list                          # installed AU instruments
```

Recording other apps (macOS 14.2+), for example a reference track playing in
Music or a browser, needs `MatCapture.app`:

```sh
./swift/bundle-capture.sh                                   # -> swift/.build/MatCapture.app
cap=./swift/.build/MatCapture.app/Contents/MacOS/mat-capture
$cap list                                                   # processes using audio (* = playing)
$cap record ref.wav --app Music --seconds 60                # 32-bit float WAV
$cap record ref.wav --app com.google.Chrome                 # a bundle ID includes the app's helpers
$cap record all.wav --system --exclude Slack                # everything except Slack
$cap stream --app Safari | …                                # raw interleaved f32le on stdout
```

It uses Core Audio process taps, so no audio driver is installed and nothing
needs restarting. Apps are tapped while they play, and an app that isn't
playing yet is picked up as soon as it starts. Recording begins with the first
audio. `--mute` silences the tapped app on the speakers while it records. The
first recording asks for the audio recording permission for MatCapture.app
(not the terminal). The bundle is signed with your Apple Development
certificate so that permission survives rebuilds.

## Architecture

```
.song ──parse──▶ Song ──arrange──▶ Timeline ──render──▶ mix + FX ──▶ WAV
                                       │                    ▲
                                       └──JSON──▶ mat-au ───┘ (stems of AU tracks)
```

* `crates/mat-core`: parser with diagnostics, arrangement, DSP (PolyBLEP
  oscillators, JP-8000 style supersaw, ZDF state variable filter, EQ, chorus,
  sidechain ducking, Dattorro plate reverb, ping-pong delay, look-ahead limiter),
  synth (with FM) and drum voices, TB-303, EXS sampler with sinc resampling, scratch turntable, CLAP plugin host, audio tracks, offline renderer.
* `crates/mat-cli`: the `mat` command.
* `swift/`: `mat-au`, which renders Audio Unit tracks offline into dry stems.
  Mixing and effects always happen in the Rust engine, so synth tracks and
  Audio Unit tracks can be combined in one song. `mat-capture` records the
  audio of other apps.

## Examples

| Song | Sounds |
|---|---|
| `examples/drunken-sailor.song` | built-in synths and drum machine |
| `examples/drunken-sailor-orchestra.song` | Logic's strings, brass and drum kit via the built-in sampler (needs Logic or GarageBand content) |
| `examples/drunken-sailor-trance.song` | trance with supersaws, sidechain pumping, chorus and EQ |
| `examples/drunken-sailor-trance-surge.song` | the same trance with Surge XT patches (needs Surge XT) |
| `examples/acid.song` | TB-303 acid line with accents, slides and knob sweeps |
| `examples/dream.song` | dream dance: Steinway piano with pedal, pads, string swells, house kit, all from presets (needs Logic content) |
| `examples/undertow.song` | melodic techno: rumbling kick, rolling bass, hypnotic arp, bell-pluck hook, 303 bed (presets only) |
| `examples/undertow-b.song` | the same track with different melodies: FM e-piano hook, glassy FM arp, bell counter line, swing |
| `examples/harbour.song` | melodic techno study: Surge XT bass and arp, gliding PWM lead, mallet motif, phaser, shimmer reverb, organic kit (needs Surge XT) |
| `examples/neon.song` | K-pop: 808 trap verses, brass-stab chorus, chanted post-chorus hook, scratches (presets only) |

## Adaptive game music

Write the song in layers (`layer` on tracks) and sections (`section name bars=a-b`),
then `mat render song.song --loop --stems out/` gives one seamless loop per layer
plus a manifest with tempo, bar length and sections for the game engine. The
layers are cut from one render of the song, so they are the same take as the
mix and add up to it — up to the master's saturation, compressor, clipper and
limiter, which the manifest's `mixing` and `master` entries describe so the
engine can put a limiter of its own on the sum.

`private/` is gitignored and meant for songs that must not be committed.
