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
| Drum hit  | `kick` `snare` `clap` `hat` `openhat` `tom` `rim` `crash` `ride` | `kick:q` |
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

### Pattern length

A pattern is rounded up to whole bars, unless you set `bars=<n>`.

## Instruments

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
| `noise <level>` | 0–1 |
| `filter <lowpass\|highpass\|bandpass\|off>` | `cutoff` Hz (`1.2k` works), `res` 0–1, `env` octaves of filter-envelope sweep, `keytrack` 0–1, `drive` 0–1 |
| `amp` / `fenv` | `attack` `decay` `release` in seconds (or `ms`), `sustain` 0–1 |
| `vibrato` | `rate` Hz, `depth` cents, `delay` seconds |
| `lfo <filter\|pitch\|pan\|amp\|width>` | `rate` Hz, `depth` (filter: octaves, pitch: cents, others 0–1), `fade` seconds, `phase` 0–1 (random per note if omitted); one line per LFO |
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

### Track effects

```
  eq lowcut=150 low=-2 lowfreq=200 mid=+1 midfreq=1k high=+3 highfreq=6k highcut=16k
  chorus mix=0.5 rate=0.7 depth=4ms
  sidechain drums depth=0.8 attack=5ms release=250ms on=kick
```

```
  sweep cutoff from=0.2 to=0.7 bars=9-16     # tb303 knobs: cutoff, resonance, envmod, decay, accent, drive
  sweep cutoff from=300 to=6000 bars=25-32   # synth: cutoff in Hz (moves logarithmically), res 0..1
```

A sweep moves through its bars and then holds the end value.

`sidechain <track>` ducks this track on every hit of another track: the kick
if that track has one, or the drum named with `on=`. A muted track still triggers,
so a silent "ghost kick" track works too. The order is EQ, chorus, ducking,
then gain, pan and the sends.

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
  reverb size=0.8 decay=0.7 damping=0.4 predelay=20ms   # or: reverb off
  delay time=3/16 feedback=0.35 tone=3k                 # time is a note value
  saturation 0.3                                        # tape-style, 0..1
  comp threshold=-14 ratio=3 attack=10ms release=150ms makeup=2   # bus compressor, or: comp off
  limiter ceiling=-1 release=80ms                       # or: limiter off
```

Order on the master: gain, saturation, compressor, limiter.

## Rendering for games

```sh
mat render song.song --stems out/          # one file per layer + out/manifest.json
mat render loop.song --loop --stems out/   # seamless loop: whole bars, tails folded into the start
```

`manifest.json` lists tempo, bar length, sections (with start and end in seconds)
and the layer files, so a game engine can switch layers on bar boundaries.
