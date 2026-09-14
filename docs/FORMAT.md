# The `.song` format

A song is a plain text file made of **blocks**. A block starts with a keyword at
the beginning of a line; its settings are the indented lines below it.
`#` starts a comment (except inside a note name like `C#4`).

```
title "My Song"
tempo 120          # quarter notes per minute
meter 4/4
section chorus bars=17-32   # optional, named bar ranges (exported to game engines)

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

Run `mat check song.song` after every edit. Errors show the line and column,
and usually a hint.

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
and the layer files, so a game engine can switch layers on bar boundaries.

The layers come out of one render of the whole song: every track is rendered
once, as it is in the mix, and each layer is its tracks' share — with their
sends into the delay and reverb, the master sidechain keyed from the whole
song, and the master's gain, EQ and width. They are all the mix's length and
sum to it sample for sample, except that the master's saturation, compressor,
clipper and limiter are not applied to a layer (they are not linear, so they
cannot be split). `manifest.json` says so under `mixing`, and carries the
`master` settings so a player can apply its own limiter to the sum.

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

Each layer's `key` and whether it was `cached` are in `manifest.json`. A cache
file no render has used for 30 minutes is deleted by the next render. The cache
is large: a four-minute song of ten layers holds about a gigabyte and a half.
