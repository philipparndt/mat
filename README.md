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

## Install

```sh
brew tap philipparndt/mat
brew trust philipparndt/mat
brew install mat
```

`mat` and the Audio Unit host `mat-au`, for Apple silicon and Intel, signed and
notarised, with the sample library `samples:` points at. From a checkout,
`make install` builds `mat` into `~/.cargo/bin` instead, and it uses the
checkout's library.

## Usage

```sh
cargo build --release

./target/release/mat check  examples/drunken-sailor.song   # validate
./target/release/mat render examples/drunken-sailor.song   # -> examples/drunken-sailor.wav
./target/release/mat render examples/drunken-sailor.song -o sailor.flac   # lossless, about 2/3 of the WAV
./target/release/mat render examples/drunken-sailor.song -o sailor.m4a    # AAC, 256 kbit/s (--bitrate)
./target/release/mat render examples/drunken-sailor.song --bars 9-16      # only those bars, in a fraction of the time
./target/release/mat render examples/drunken-sailor.song --stream         # written as it renders, to play at once
./target/release/mat play   examples/drunken-sailor.song   # render and play
./target/release/mat export examples/drunken-sailor.song   # arranged timeline as JSON
./target/release/mat pack   examples/neon.song             # -> examples/neon.zip: the song and every file it reads
./target/release/mat translate examples/drunken-sailor.song # how the mix carries to a Sonos, a car, a phone
./target/release/mat inspect "logic:01 Acoustic Pianos/Steinway Grand Piano 2.exs"
./target/release/mat presets                                # built-in instrument and master presets
```

`mat export` is the song as `mat` arranged it: per track its instrument,
settings and notes in seconds, and its `regions` — one per `play` line, however
often it repeats, with the pattern's name, `start` and `end`, the length of one
`pass`, `repeat`, `transpose`, and the `file` and `line` it was written on and
the pattern was defined on. Each note's `region` is the index of the one it came
from. It is what an editor draws a song's patterns and notes from.

`mat pack` puts a song and everything it reads into one zip, to hand on or
keep: its included files, its samples and audio, its own `.exs` instruments with
their samples, and the samples it takes from mat's library. The zip is one
folder named after the song, and the paths in its text point into it — a file
from outside the song's folder goes into `external/`, a library sample into
`mat-samples/`, and a preset that reads library samples is written out in the
song. Only paths that have to change are changed. What comes with software
installed where the song is played — Logic's and GarageBand's libraries, the
General MIDI bank, Surge, Audio Unit and CLAP plugins — is left as written and
listed in the pack's `README.txt`. A sample that is not there stops the pack,
with where it is named.

### Checking a mix on other devices

A mix that is perfect on headphones can fall apart on one speaker: the width
cancels, the sub-bass is not played, the bass protection pumps. `mat translate`
puts a song — or any WAV, AIFF or CAF — through models of other devices and
says what each one loses:

```sh
mat translate song.song                                  # measure, on every device
mat translate song.song --to sonos-five,car --bars 33-40 # two devices, eight bars: seconds
mat translate song.song --on airpods-max --play          # loop it, and switch devices while it plays
mat translate song.song --on airpods-max --out heard/    # original.wav and a WAV per device, at one loudness
mat translate song.song --volume loud --json heard.json  # bass protection at work; the numbers as JSON
mat translate --list                                     # the devices
```

It prints where the mix's energy is and what summing left and right costs each
band (`mono loss`: 0 dB is mono, -3 dB is two unrelated sides, more than that is
width made of phase, which one speaker cancels); then, per device, each band's
level against the mix, and for a song each layer against the rest of the mix —
a layer at -8 has sunk 8 dB into it, `gone` is not played at all. Lines with
`!` say what to do something about.

`--on` names what you are listening on. Its own colour is taken out of what you
hear, so it is not added to the simulated device's, and a simulated speaker
reaches both ears, as it does in a room. Headphones that play everything are
the right thing to listen on; what a laptop cannot play, no simulation on it
will. The devices are models — a roll-off, a response, a width, a bass limiter,
road noise — near enough to say whether a bass line survives a small speaker,
and no substitute for playing the song on one.

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
  oversampled subtractive synth (24 dB ladder, legato, FM on any oscillator), six-operator FM synth, distortion, drum voices, TB-303, EXS sampler with sinc resampling, scratch turntable, CLAP plugin host, audio tracks, offline renderer, playback device models (`mat translate`).
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
| `examples/lantern.song` | progressive trance: a 16-saw pluck whose filter and decay open over each build, mono low layer, held sine notes into the drops, clean tuned kick with a click, offbeat-16th bass (built-in synths only) |
| `examples/ember/ember.song` | Lantern after dark: melodic techno at 124 BPM, Am F Dm E, a kick that breaks its stride every eighth bar, two kits pulling against each other, Logic's string ensemble over the breakdown; in files with `include`, arranged with `repeat` loops (needs Logic content) |
| `examples/drive/drive.song` | Vector Drive (`examples/drive.song`) split into files with `include`, its repeats written as loops; renders the same samples (built-in synths only) |

## Adaptive game music

Write the song in layers (`layer` on tracks) and sections (`section name bars=a-b`),
then `mat render song.song --loop --stems out/` gives one seamless loop per layer
plus a manifest with tempo, bar length and sections for the game engine. The
layers are cut from one render of the song, so they are the same take as the
mix and add up to it — up to the master's saturation, compressor, clipper and
limiter, which the manifest's `mixing` and `master` entries describe so the
engine can put a limiter of its own on the sum.
Stems take the output's format: `-o song.flac` writes FLAC stems. For AAC
stems, check that the engine honours the encoder delay the .m4a records, or
the loops will not be seamless.

FLAC and M4A are written by macOS `afconvert` (Apple's encoders).

`private/` is gitignored and meant for songs that must not be committed.

## Releasing

```sh
make sign-check                        # the Developer ID and the notarytool profile are there
make release                           # dist/mat-<version>.tar.gz, signed and notarised; nothing published
make release-publish VERSION=0.2.0     # the whole of it: version, tag, build, notarise, GitHub release, tap
make tap VERSION=0.2.0                 # just point philipparndt/homebrew-mat at a published release again
make tap VERSION=0.2.0 TAP_PRINT=1     # show the formula, push nothing
```

`release-publish` wants `docs/release-notes-<version>.md` and a clean tree
before it does anything. It stamps the version into `Cargo.toml`, tags, builds
universal binaries from the tag, signs them with the Developer ID and the
hardened runtime, has Apple notarise them (and checks the tickets are served),
uploads the archive and its checksum, and then writes `Formula/mat.rb` in the
tap. The steps and why they are in that order are in `scripts/`.
