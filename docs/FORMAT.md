# The `.song` format

A song is a plain text file made of **blocks**. A block starts with a keyword at
the beginning of a line; its settings are the indented lines below it.
`#` starts a comment (except inside a note name like `C#4`).

```
title "My Song"
tempo 120          # quarter notes per minute
meter 4/4
section chorus bars=17-32   # optional, named bar ranges (exported to game engines)
include "kits/drums.song"   # optional, another file's blocks, read here

instrument <name> <synth|drums|tb303|sampler|clap|au>
  ...

pattern <name> [grid=<step>] [bars=<n>]
  ...

track <name>
  instrument <name>
  ...

master
  ...
```

Run `mat check song.song` after every edit. Errors show the file, line and
column, and usually a hint.

## Includes

```
include "kit.song"            # next to this file
include "parts/bass.song"     # in a folder below it
include "../shared/fx.song"
```

`include "<file>"` reads another file where the line is, as if its lines were
written in its place: instruments, presets, patterns, tracks, sections, song
settings like `tempo`, and its own includes. Everything behaves as it would in
one file — a second `tempo` wins over the first, and a pattern defined in both
files is defined twice. An `include` line takes no indented body, and an
included file's first indented lines do not continue the block above the
`include`.

* **Paths** are relative to the folder of the file the `include` is written
  in, and must be quoted. So are the files an included file names — samples,
  `load`, `patch`, `audio` — so a kit brings its samples along wherever it is
  included from (`logic:`, `samples:` and the other shortcuts are unchanged).
* **A file included twice** anywhere in the song is read the first time and
  skipped after that, so two parts can both include a shared kit.
* **A cycle** — a file that includes itself, directly or through others — is an
  error on the `include` line that closes it, and so is a file that cannot be
  read.

`mat check` and `mat render` read the includes of the song they are given, and
report each problem under the path of the file it is in. `mat render --stems`
lists every file the song was read from under `sources` in `manifest.json`,
the song first, so an editor knows a save of any of them needs a new render.
The render cache is keyed by what the files say, not where: a comment or a
moved block in an included file renders nothing again. See `examples/include/`.

## Patterns

### Note patterns

Notes are written as `<pitch>[:<duration>][@<velocity>]`.

| Element   | Syntax | Examples |
|-----------|--------|----------|
| Pitch     | letter + optional `#`/`b` + octave (`C4` = middle C) | `C4` `F#3` `Bb2` |
| Chord     | pitches in brackets | `[C4 E4 G4]:h` |
| Rest      | `r` or `_` | `r:q` |
| Drum hit  | `kick` `snare` `clap` `hat` `openhat` `tom` `rim` `crash` `ride`; scratch moves `baby` `fwd` `back` `scribble` `chirp` `transform` | `kick:q` |
| Duration  | `w` whole, `h` half, `q` quarter, `e` eighth, `s` 16th, `t` 32nd | `q` |
|           | dotted, triplet, fraction, sum | `q.` `e3` `3/16` `h+e` |
| Velocity  | 1–127 (default 100) | `@90` |
| Accent    | `!` at the end (tb303; also `X` in grids) | `C2:s!` |
| Slide     | `~` at the end: glide into the next note (tb303) | `C2:s~` |
| Bar check | `\|` fails if the bar before it is not exactly one bar long | `\|` |
| Group     | `(…)x<n>` plays what is inside n times, as if written out (see [Loops](#loops)) | `(C4 D4)x2` |

**The duration carries over** to later notes until you give a new one.
Velocity does not carry over. Example:

```
pattern verse
  A4:q A4:e A4 A4:q A4:e A4 | A4:q D4 F4 A4 |
```

### Grid patterns

Good for drums and ostinatos. Every row starts with a drum name or a pitch,
followed by one cell per step. All rows must have the same number of steps.

```
pattern beat grid=1/16
  kick  X.....x.X.....x.
  snare ....X.......X...
  hat   x.o.x.o.x.o.x.o.
```

Cells: `x` hit (100), `X` accent (127), `o` soft (60), `1`–`9` velocity,
`=` holds the previous hit one more step, `.` `-` `_` rest. Spaces and `|` are ignored.

### Sustain pedal

`pattern theme pedal` holds every note until the end of its bar, like a pedal
lifted on each chord change. Meant for piano patterns.

### Pattern length

A pattern is rounded up to whole bars, unless you set `bars=<n>`.

## Instruments

### Presets

```
instrument pad preset dream-pad      # start from a built-in preset ...
  filter lowpass cutoff=600          # ... and override or add settings
instrument kit preset house-kit
master preset house
```

`mat presets` lists them: pads, leads, plucks, basses, a 303, sampled drum kits,
Logic's Steinway and strings, and master chains per genre.

### `synth`: subtractive synthesizer

```
instrument lead synth
  osc saw voices=3 spread=7            # one line per oscillator
  osc square octave=-1 level=0.3
  noise 0.05
  filter lowpass cutoff=800 res=0.2 env=3 keytrack=0.5 drive=0.3
  amp  attack=10ms decay=0.3 sustain=0.8 release=0.2
  fenv attack=1ms  decay=0.4 sustain=0.2 release=0.2
  vibrato rate=5 depth=10 delay=0.3
```

| Setting | Options |
|---|---|
| `osc <sine\|triangle\|saw\|square>` | `level` 0–4, `octave` ±4, `semi` ±24, `detune` cents, `voices` 1–16 (unison), `spread` cents, `width` 0–1 |
| `osc supersaw` | JP-8000 style: `detune` 0–1 (0.5 ≈ classic trance), `mix` 0–1 (center vs. detuned saws), plus `level`, `octave`, `semi`, `width` |
| `osc sine fm=… fmratio=… fmenv=…` | FM on any oscillator: a sine modulator at `fmratio` × pitch, `fm` index in radians, `fmenv` extra index that follows the filter envelope (bells, e-pianos, FM bass) |
| `osc square pw=0.2` | pulse width 0.05–0.95; modulate it with `lfo pw` |
| `noise <level>` | 0–1 |
| `filter <lowpass\|highpass\|bandpass>` | `cutoff` Hz (`1.2k` works), `res` 0–1, `env` octaves of filter-envelope sweep, `keytrack` 0–1, `drive` 0–1. Several `filter` lines chain in series; a line with the mode of an existing one replaces that stage (that's how you override a preset's filter); `filter off` removes them all |
| `amp` / `fenv` | `attack` `decay` `release` in seconds (or `ms`), `sustain` 0–1 |
| `vibrato` | `rate` Hz, `depth` cents, `delay` seconds |
| `lfo <filter\|pitch\|pan\|amp\|width\|pw\|fm>` | `rate` Hz, `depth` (filter: octaves, pitch: cents, pw: 0–0.45, fm: radians, others 0–1), `fade` seconds, `phase` 0–1 (random per note if omitted); one line per LFO |
| `glide <time>` | portamento from the previous note's pitch |
| `penv depth=12 decay=80ms` | pitch envelope: starts `depth` semitones away and decays to the note |
| `drift <cents>` | random detune per note, like drifting analog oscillators |

### `drums`: synthesized drum kit

```
instrument kit drums
  kick decay=1.3 tune=-2 gain=-1
```

Voices: `kick snare clap hat openhat tom rim crash ride`, each with `gain` (dB),
`tune` (semitones) and `decay` (multiplier). Open hats are cut off by the next hat.

### `tb303`: acid bass line synthesizer

```
instrument acid tb303
  wave saw          # or square
  cutoff 0.3        # the knobs are 0..1, like the panel
  resonance 0.8
  envmod 0.5
  decay 0.4
  accent 0.8
  drive 0.3         # overdrive after the VCA
  gate 0.5          # fraction of each note the gate is held
  slide 60ms
```

Monophonic, like the hardware. Accented notes (`!`) are louder and open the filter;
consecutive accents build up. A slide (`~`) holds the gate and glides to the next
note without retriggering the envelopes. Knobs can move over time with `sweep`
(see track effects).

### `samples`: your own samples, cut from audio files

```
instrument kit samples
  kick    "stems/Drums.wav" at=41.52s  length=0.42s
  clap    "stems/Drums.wav" at=110.55s length=0.22s gain=+4
  note G2 "stems/Bass.wav"  at=122.7s  length=1.8s loop=0.5s-1.7s keys=C1-B3 vel=1-127
  amp release=30ms
  velocity 12
```

Drum lines use the drum names; `note <root>` lines are pitched across `keys=`
(default: all keys) from the note the region was recorded at. Options:
`at` and `length` in the file, `loop` relative to the region start, `keys`, `vel`,
`gain` (dB), `tune` (semitones). Regions fade out over their last 10 ms.

### `scratch`: turntable scratching

```
instrument dj scratch
  sample "vocal.wav" at=0.2s length=0.6s   # the bit of record under the needle ...
  source lead bars=49-49                   # ... or another track of this song (its dry audio),
                                           #     or `source mix bars=8-8` for everything but scratches
  speed 1.2                                # how far it travels per move
  pitch keep                               # time-stretch: same pitch at any speed (default: follow, like vinyl)
  grain 1/16                               # with pitch keep: ≤ 60 ms smooth grains (tonal), longer = beat slices (drums)
  gain -3
```

Write moves like drums, in grid or note patterns; each hit's duration is the
length of the move: `baby` (forward and back), `fwd`, `back`, `scribble` (fast
back-and-forth), `chirp` (fader cuts at every reversal), `transform` (fader stutter).

```
pattern cuts grid=1/8
  baby      x...x...
  transform ......x.
```

### `sampler`: sampled instruments (Logic / GarageBand `.exs`)

```
instrument piano sampler
  load "logic:01 Acoustic Pianos/Steinway Grand Piano 2.exs"
  gain -3
  amp attack=0 release=0.4
  velocity 18          # dB between softest and loudest note
  articulation 2       # e.g. staccato in "Trumpets+"
  map tom=43           # drum names to other notes than General MIDI
```

Played by the built-in sampler on every platform: velocity layers, round robin,
articulations, hi-hat choke and loops, with band-limited resampling. Reads WAV,
AIFF and CAF samples, including Logic's large consolidated sample files.
Path shortcuts:

* `logic:` → `/Library/Application Support/Logic/Sampler Instruments/`
* `garageband:` → GarageBand's sampler instruments

Drum hits use General MIDI notes; fix kits that differ with `map`.
`mat inspect "logic:…/SoCal Kit.exs"` lists what an instrument contains.

### `clap`: plugin instruments (Surge XT and other CLAP plugins)

```
instrument stab clap
  plugin "Surge XT"                                  # name in the CLAP folders, or a path
  patch "surge:Polysynths/Megasynth 1.fxp"
  param "A Amp EG Release" 0.1                       # plain parameter value
  gain -3
```

Runs on every platform where the plugin is installed. Patch shortcuts:
`surge:` (factory patches) and `surge-3rdparty:`. List a plugin's parameters with
`mat plugin-params "Surge XT" --filter cutoff`. Plugin tracks render one at a time,
so they are slower than built-in instruments.

### `au`: Audio Unit instruments (macOS)

```
instrument lead au
  component aumu samp appl   # type, subtype, maker: copy them from `mat-au list`
  gain -3
```

Rendered by the `mat-au` host (`swift build -c release --package-path swift`).
`load` accepts `.aupreset`, `.sf2`/`.dls` (with `program <0-127>`), and `gm`
for the General MIDI sound bank built into macOS. `mat-au list` shows installed instruments.

## Tracks

```
track melody
  instrument lead
  gain -6          # dB
  pan 0.2          # -1 (left) .. 1 (right)
  reverb 0.3       # send amount 0..1
  delay 0.1        # send amount 0..1
  at 5             # jump to bar 5
  play verse x2 transpose=2 vel=0.9
  rest 4           # skip 4 bars
  mute
  layer drums      # stem group for `mat render --stems` (default: the track name)
```

`play` places patterns one after another. `at` and `rest` move the position.
`repeat 4 { … }` plays the steps inside it again and again (see [Loops](#loops)).

### Groove

```
swing 0.58                 # top level: every track; 0.5 = straight, 0.66 = triplet feel
  swing 0.62 grid=1/8      # on a track: overrides the song; grid defaults to 1/16
  humanize time=8ms vel=10 # random timing (± time) and velocity (± vel of 127) per note
```

Humanize is deterministic per note, so a render always sounds the same.

### Takes

```
seed 7                     # top level: which take of everything random the song gets
  seed 3                   # on a track: this track's take instead
```

Everything random in a note — oscillator drift, unison spread, LFO phases,
drum noise, humanize — comes from the track's name, the `seed`, and the note
itself: its time, its pitch, and which of several identical notes it is. So a
note sounds the same however the rest of the song is edited, and changing a
`seed` is how to hear another take. Renaming a track changes its take.

### Track effects

```
  eq lowcut=150 low=-2 lowfreq=200 mid=+1 midfreq=1k high=+3 highfreq=6k highcut=16k
  comp threshold=-12 ratio=4 attack=5ms release=120ms makeup=3 mode=feedback
  chorus mix=0.5 rate=0.7 depth=4ms
  phaser rate=0.3 depth=0.7 stages=6 feedback=0.4 mix=0.5
  sidechain drums depth=0.8 attack=5ms release=250ms on=kick
```

```
  sweep cutoff from=0.2 to=0.7 bars=9-16     # tb303 knobs: cutoff, resonance, envmod, decay, accent, drive
  sweep cutoff from=300 to=6000 bars=25-32   # synth: cutoff in Hz (moves logarithmically), res 0..1
  sweep decay from=0.2 to=1.5 bars=5-12      # synth: amp decay and release in seconds, taken at the start of each note
  sweep gain from=-14 to=0 bars=25-32        # any track: a volume curve in dB (swells, fades)
```

A sweep moves through its bars and then holds the end value.

`sidechain <track>` ducks this track on every hit of another track: the kick
if that track has one, or the drum named with `on=`. A muted track still triggers,
so a silent "ghost kick" track works too. The order is EQ, compressor, chorus, phaser, ducking,
then gain, pan and the sends. `comp` on a track is the same compressor as on the
master; `mode=feedback` detects after the gain stage (gentler, pumps musically),
the default is feedforward.

### Audio tracks

```
track vocals
  audio "stems/Vocals.wav" offset=0.52s   # where bar 1 starts in the file
  gain -2
  play bars=17-24 x2                      # bars of the file, at the song tempo
  play all                                # the whole file, bar 1 aligned
```

Put the `audio` line before `play`. `play bars=` advances the position like a
pattern; `play all` does not.

## Loops

Two kinds of loop keep a song short. Both are written out when the song is
read, so a loop sounds exactly like the notes or steps it stands for, and
renders the same samples.

### Repeat groups, in patterns

```
pattern acid
  (A1:s A1 A2! C2~)x3 E1:s G1 A1 C2 |
pattern beat grid=1/16
  kick (X...x...)x2
  hat  (x.)x8
```

`(…)x<n>` plays what is inside the brackets n times in a row, as if written
out. It works in note lines and in grid rows, and groups may nest and hold
chords and bar lines: `((C4:s D4)x2 [E4 G4]:h. |)x4`.

* **Durations carry** into and out of a group as they would written out:
  in `C4:q (D4 E4:e)x2 F4`, the first `D4` is a quarter, the second an
  eighth, and so is `F4`.
* **The bar check and the pattern's length** read the written-out line, and
  so do a grid's row lengths: `kick (x...)x4` is 16 steps.
* A group opens and closes on one line. `x0`, a missing count after `)`, an
  unclosed `(` and a `)` that closes nothing are errors.

### Repeat blocks, in tracks

```
track bass
  instrument acid
  repeat 4 {
    play verse
    repeat 2 {
      play fill
    }
    rest 1
  }
```

`repeat <n> {` plays the steps up to its `}` n times, one after another; the
`}` goes on a line of its own. Inside go `play` (also `play … x2`, and an audio
track's `play bars=…`), `rest` and other `repeat` blocks. `at` is an error
there, because it would jump back to the same bar on every pass, and so are
the track's settings. `repeat 0` and an empty block are errors too.

An editor that shows where lines are heard lights a group's note at the token
it is written at on every pass, and a `repeat` line across all of its passes.
See `examples/loops.song`.

## Master

```
master
  gain 3
  sidechain drums threshold=-24 ratio=6 attack=2ms release=150ms darken=2
                                                        # keyed compression: the drums track (or layer) pushes
                                                        # everything else down on every hit; darken closes a
                                                        # lowpass by that many octaves per 12 dB of reduction
  eq high=+1.5 highfreq=9k                              # same options as a track eq
  width 1.15                                            # stereo width: 0 mono, 1 as mixed, up to 2
  reverb size=0.8 decay=0.7 damping=0.4 predelay=20ms shimmer=0.3 lowcut=200 highcut=8k   # or: reverb off
  delay time=3/16 feedback=0.35 tone=3k mod=3ms rate=0.4   # time is a note value; mod/rate chorus the echoes
  saturation 0.3                                        # tape-style, 0..1
  comp threshold=-14 ratio=3 attack=10ms release=150ms makeup=2   # bus compressor, or: comp off
  clip -4                                               # soft clipper before the limiter: shaves kick peaks for loud masters
  limiter ceiling=-1 release=80ms                       # or: limiter off
```

Order on the master: sidechain, gain, eq, width, saturation, compressor, clip, limiter.

## Rendering for games

```sh
mat render song.song --stems out/          # one file per layer + out/manifest.json
mat render loop.song --loop --stems out/   # seamless loop: whole bars, tails folded into the start
```

`manifest.json` lists tempo, bar length, sections (with start and end in seconds)
and the layer files, so a game engine can switch layers on bar boundaries, and
under `sources` the absolute paths of the song and every file it includes.

The layers come out of one render of the whole song: every track is rendered
once, as it is in the mix, and each layer is its tracks' share — with their
sends into the delay and reverb, the master sidechain keyed from the whole
song, and the master's gain, EQ and width. They are all the mix's length, and
**playing them together is the mix, sample for sample**, the master's
saturation, compressor, clipper and limiter included.

Those four are not linear, so they cannot be split — but what they do to the
mix at one sample is, in the end, a number the mix is multiplied by there. That
number is `mastered / unmastered` at that sample, per channel, and every stem
is written through it. Summing the stems then gives `g·(stem₁ + stem₂ + …)`,
which is `g·unmastered`, which is the mix. Nothing is left for a player to do:
the sum is already under the limiter's ceiling.

`manifest.json` says so under `mixing`: `stems_through_master` is `true`,
`sum_peak_db` is the mix's own peak, and `pre_master_peak_db` is what the sum
would have peaked at without the curve — how hard the master was working.

```sh
mat render song.song --stems out/ --stems-pre-master   # the linear part only
```

With `--stems-pre-master` the stems are what they were before: the master's
linear stages and no more, summing to the mix *before* its dynamics. A player
that wants the mastered loudness then puts its own limiter on the sum, and
`master` in the manifest tells it what the song's would have done. `applied`
and `skipped` under `mixing` name the stages either way.

## Rendering while editing

```sh
mat render song.song --stems out/ --cache .mat-cache/   # keep each layer between renders
```

With `--cache`, each layer is kept under a hash of everything that shapes it:
its tracks and their notes, the tracks keying the master sidechain, the
master's delay, reverb, sidechain, gain, EQ and width, the sample rate, and the
size and date of every file those read. Rendering again renders only the
layers whose hash changed and reads the rest back, and a stem already written
for a layer is hard-linked into `out/` instead of written again. An edit to one
pattern of a four-minute, ten-layer song renders in about a second instead of
about five. A cached render is the same samples as an uncached one.

A *stem* is kept under the layer's hash and the master's gain curve together:
an edit to any layer changes the mix, and so the curve, and so every stem. The
layers themselves are unaffected — they are what they were before the master —
so an edit still renders only the layers it touched, and only the writing of
the stems is done again.

Each layer's `key` and whether it was `cached` are in `manifest.json`. A cache
file no render has used for 30 minutes is deleted by the next render. The cache
is large: a four-minute song of ten layers holds about a gigabyte and a half.

## Rendering while it plays

```sh
mat render song.song --stream                                  # the file grows; play it at once
mat render song.song --stream --stems out/ --cache .mat-cache/
```

`--stream` renders the song in order of time and writes it as it goes, so a
four-minute song can be playing a tenth of a second after it was asked for
instead of six seconds later. Beside the output it keeps
`<output>.stream.json`, which says how much of the file can be read:

```json
{
  "file": "song.wav",
  "sample_rate": 48000, "channels": 2, "bits": 24,
  "data_offset": 68, "bytes_per_frame": 6,
  "frames_written": 786432, "seconds_written": 16.384,
  "bars_written": 8, "bar_seconds": 1.875, "tempo": 128,
  "seconds_total": 240.0, "bars_total": 128,
  "finished": false
}
```

Poll that file. It is written whole, under a temporary name and renamed, after
the samples it counts, so it never claims more of the WAV than is there; the
WAV's own header says the same, so a player that only reads the WAV is right
too. It ends `"finished": true`.

**How long the whole thing will be.** `seconds_total` and `bars_total` say so in
the very first write, before a sample has been rendered — so a timeline can be
laid out once, at its real length, instead of being guessed from what has
arrived and rescaled as more does. They are the song's own length: its last
sound rounded up to a whole bar, which is the measure `bars_written` counts in.
With `--bars from-to` they are that window, which is what the file will hold.

They are the *music's* length, not the file's: a reverb or delay tail goes on
ringing past the last bar, so the finished `seconds_written` is a little more
than `seconds_total` (neon: 228.4 s written against 226.9 s of song), and more
than a little when a short window is followed by a long tail (four bars of neon:
11.8 s written against 7.6 s of window). A timeline is the song's length; the
audio after it is the end of the song still sounding.

**It is one render, not two.** Everything that carries from one sample to the
next — a filter's state, the delay line, the reverb tank, a compressor's
envelope, an LFO's phase, the limiter's look-ahead — carries across the
boundaries between the stretches, so there is no seam at one. The finished file
is the file `mat render` writes for the same song, byte for byte: every example
that renders reproducibly at all comes out identical both ways. (`harbour.song`
and `drunken-sailor-trance-surge.song` are not reproducible from one run to the
next, streamed or not: the plugin is not. Two plain renders of harbour are the
same file up to 31.476 s and then differ over four fifths of their samples; a
streamed one is the same file up to 31.476 s and then differs by as much. Over
a window where the plugin does reproduce — `--bars 1-12` of harbour, `--bars
1-6` of the surge song — a streamed render is a plain one byte for byte.) So
this is not `--bars`, which renders a window of the
song from silence and cannot carry a tail into it; nothing is missing from a
streamed render.

**When the first sound arrives.** In well under a tenth of a second for most
songs: `examples/neon.song`, four minutes and ten layers, is playing 0.07 s
after it was asked for, where the whole render takes five to seven seconds.
One kind of track cannot be made a bar at a time and is rendered in full before
the first stretch, so a song with one waits for it: a scratch track, which cuts
its record out of a file or out of another track, together with the tracks it
cuts from (`examples/undertow-b.song` waits 1.4 s for the drum track its record
is cut from). Everything else is made when its bar comes — synths, drum kits,
samplers, audio files, tb303s, CLAP plugins, Audio Unit tracks.

**What it costs.** Nothing much. It is the same work, spread a stretch at a
time rather than a layer at a time, and a stretch has every layer in it to
spread across where the ordinary render runs three side by side. Three runs
each on a machine at load 35-40: neon 7.0 s ordinary against 5.2 s streamed,
`undertow.song` 13.6 s against 12.5 s, `dream.song` 12.5 s against 12.3 s.

`--stems` and `--cache` work as they always do. The stems and `manifest.json`
are written at the end, from the same layers, and are the same files an
ordinary render writes. A streamed render reads and fills the *same* layer
cache as an ordinary one, under the same keys, so an editor can stream one
render and not the next.

`--bars` works too, and is mostly redundant now: a streamed whole render is
playing sooner than a stretch used to be, and it is the whole song.

`--stream` writes a `.wav` — a FLAC or an AAC is encoded from the finished
render, so there is nothing to write until it is done — and does not go with
`--loop`, which folds the song's tail back into its start.

**The end moves.** A render's last act is to cut the trailing near-silence and
fade the 50 ms before it. So the last stretch and a little before it are
written again at the end, and the file can end up shorter than it was a moment
earlier — by silence, and never by more than that. Everything before it was
final when it was written, and stays as it was.

## Rendering part of a song

```sh
mat render song.song --bars 33-40                      # eight bars, in a fraction of the time
mat render song.song --bars 9                          # one bar
mat render song.song --bars 33-40 --loop --stems out/  # those bars as a loop, with stems
```

`--bars <from>-<to>` renders only those bars. They are counted from 1 and both
are included; a single number is one bar. A bar outside the song, or a range
that runs backwards, is an error that says how long the song is. It works with
`--stems`, `--cache`, `--loop` and every output format.

It is much faster than rendering the whole song, because everything outside
those bars is thrown away before a voice is synthesised: eight bars of a
four-minute song render in about half a second where the whole takes six. An
editor can play the bars somebody is working on at once, and swap in the whole
song when it lands. `--stream` above does better at that, and does it to the
whole song.

**What a stretch carries in.** Everything that *starts* inside it sounds as the
song does at those bars: the same notes, the same takes, the same track and
master settings, the same sidechain ducking, and the value a sweep that began
earlier has reached. Two bars in front of the stretch are rendered as well and
then dropped, so a note that begins just before it is heard ringing at the
start.

**What it does not.** Whatever was sounding when that lead-in began: a note
that started before it, a reverb or delay tail, a tb303's filter and slide
state, a scratch track's record of another track. An audio file is cut to the
stretch, with the usual 5 ms fade at a cut, and the first 5 ms of the stretch
fade in — it begins in the middle of the music, and a file whose first sample
is far from zero clicks. A stretch is a stretch of the song, not the song
played from bar 33.

So a stretch of a song without reverb or delay is the whole render's own
samples at those bars, to the last bit. With them it is the same music at the
same loudness in a reverb of the same size: measured on `examples/neon.song`,
bars 33-40 track the whole render to within 0.2 dB every half second, and what
is not carried in is 12 dB under the music.

`--loop` with `--bars` loops over exactly the bars asked for, rather than over
the song's length rounded up to a bar.

In `manifest.json`, `bars` is `[from, to]` as the whole song numbers them —
`[1, 121]` for a whole render — and `partial` says which of the two this is.
Everything else in the manifest, and every stem, is counted from the first of
those bars: `seconds` is the stretch, and `sections` are the ones it is in,
clipped to it and keeping the song's own bar numbers. The cache keys a layer by
its bars as well, so a stretch and the whole song are never the same layer in
it, whichever is rendered first.
