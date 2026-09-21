# mat 0.1.0

The first release: a song written as plain text, by hand or with an AI,
rendered to audio from the command line. What was built over 57 commits is now
one `brew install` away, for Apple silicon and Intel, signed and notarised, with
the sample library the built-in kits play.

## Writing a song

**A `.song` file is blocks of text**: instruments, patterns of notes or drum
grids, tracks that play them, and a master chain. Durations carry from note to
note, `|` checks that every bar is a bar, and `(…)x4` in a pattern and
`repeat 4 { … }` in a track keep a long song short. `include "kit.song"` splits
a song across files: sounds in one, beats in another, parts in a folder. The
whole language is in [docs/FORMAT.md](https://github.com/philipparndt/mat/blob/v0.1.0/docs/FORMAT.md).

**Groove and takes.** `swing`, `humanize`, and a `seed` that picks which take of
everything random a song gets — drift, unison, LFO phases. A note sounds the
same however the rest of the song is edited, so a render always sounds the same
until you ask for another take.

## Sounds

**Eight kinds of instrument.** A subtractive synth (supersaw, unison, a
saturating 24 dB ladder, legato and glide, FM on any oscillator); a six-operator
FM synth for e-pianos, bells and 80s basses; a synthesized drum kit; a TB-303
with accents, slides and knob sweeps; samples cut from your own audio files;
Logic and GarageBand `.exs` instruments; a turntable that scratches a sample or
another track of the song; and plugins — CLAP (Surge XT) everywhere, Audio Units
on macOS through `mat-au`, which ships alongside.

**About sixty presets**, from pads, leads, plucks and basses to sampled kits
and master chains per genre: `mat presets`. A preset is a starting point — the
lines under it change what they name.

**Effects** on every track — EQ, compressor, distortion, chorus, phaser,
sidechain ducking, and sweeps of any knob over bars — and on the master a keyed
sidechain, EQ, width, plate and shimmer reverb, delay, saturation, a bus
compressor, a clipper and a look-ahead limiter.

## Rendering

**WAV, FLAC or AAC**, picked by the output's extension. `--bars 33-40` renders
eight bars in well under a second; `--stream` writes the song as it renders, so
it plays a tenth of a second after it was asked for; `mat play` plays it on the
speakers the same way.

**Stems that are the mix.** `--stems` writes one file per layer, cut from one
render and put through the master's own gain curve, so playing them together
is the mix sample for sample. With `--loop` and `section`s, and the manifest
beside them, that is adaptive game music. `--cache` keeps each layer between
renders, so an edit renders only what it touched.

## Knowing what you made

**`mat check`** after every edit: errors with the file, line and column, and
usually a hint. **`mat export`** is the arranged song as JSON — every note in
seconds, and the pattern and line it came from.

**`mat translate`** says how a mix carries away from the headphones it was made
on: a Sonos, a car, a phone, a laptop, a club. Per device, what each band loses,
what its bass protection does, what road noise covers, and which layer sinks or
disappears — and with `--play`, the song in a loop, switching between the
devices while it plays.

**`mat pack`** puts a song and every file it reads — includes, samples, its own
instruments, the library samples it uses — into one zip that renders on another
machine.

## Editors

**`mat lsp`** is a language server for `.song` files: diagnostics as you type,
across included files; completion of keywords, settings and the song's own
names; hover that explains the word under the cursor; go to definition; and
`mat/timeline`, which says where in the song every line is heard. A tree-sitter
grammar for highlighting is in `tree-sitter-song/`.

## What is not in it

`mat-capture`, which records the audio of other apps, is not in the download:
it needs an app bundle of its own for the recording permission, and
`make capture` builds it from a checkout. Logic's instruments, GarageBand's and
Surge XT are used where they are installed and are not included.

`mat` runs on macOS 11 or newer; Audio Unit instruments, through `mat-au`,
need macOS 14.2.
