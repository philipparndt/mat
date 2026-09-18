---
name: analyze-sample
description: Measure an audio file (a reference track, a loop, a recording) and rebuild it as a .song file. Use when the user points at a WAV/MP3/M4A and asks to recreate it, to find out what it is made of, what tempo/key/notes it has, or why a render does not sound like it.
---

Rebuild a sample as a `.song` by **measuring** it, not by guessing. You cannot
hear; numbers and pictures are the whole of your evidence, so the workflow is:
measure the sample, build a song, measure the render the same way, compare the
two, change one thing, measure again. Say plainly what is measured and what is
guessed, and let the user's ears settle anything the numbers cannot.

## Protect the sample first

**Never render a song whose default output could be the sample.** `mat render
x.song` writes `x.wav` beside it — that has already destroyed a user's file.

1. Copy the sample into the scratchpad and make it read-only:
   `cp sample.wav "$SCRATCH/safe/" && chmod 444 "$SCRATCH/safe/sample.wav"`
2. Put the song somewhere else than the sample (e.g. `private/rebuilds/`), and
   never name it after the sample.
3. Always render with an explicit `-o`, into the scratchpad.
4. Analyse the read-only copy, never the original.

## Tools

Python with numpy, scipy, librosa, soundfile, matplotlib, plus `ffmpeg`/
`ffprobe`. If librosa is missing, make a venv in the scratchpad and install it
(that takes a few minutes, so do it once). Call the venv's python by its full
path — a symlink to it breaks the venv.

Write the analysis as small scripts in the scratchpad and keep them: every
measurement is run twice, once on the sample and once on each render.

## 1. What the file is

```
ffprobe -hide_banner file.wav
ffmpeg -hide_banner -i file.wav -af ebur128=peak=true -f null - 2>&1 | grep -E "I:|LRA:|Peak:"
```

Loudness says what the master must do: -9 LUFS with 0.4 LU of range is a
brickwalled club master, -16 LUFS with 8 LU is a mix with dynamics left.

## 2. Pictures before numbers

One figure with a log-frequency spectrogram, a CQT (note axis), and per-band
RMS over time. Look at it. It shows the structure (where parts enter and
leave), whether the content is percussive (vertical) or tonal (horizontal),
and where the energy sits.

## 3. The grid

Do not trust a beat tracker alone. Fit the grid to the sample's own onsets:

- Take the envelope of a band that carries the pulse (20–120 Hz for a kick,
  3–12 kHz for its click; the click gives a sharper grid than the body).
- Find peaks, then fit `t = a*k + b` over the peak indices. The residual tells
  you whether the fit is real: a few ms is a grid, 40 ms is a wrong one.
- Cross-check with a comb search: for each BPM and offset, sum the envelope at
  the grid points and keep the best. This survives missing hits.
- Sanity-check the tempo against the envelope's autocorrelation. A "bar"
  period of 1.9512 s is 123 BPM; hits every 0.244 s are 8ths at 123.

Beware: detectors lock onto whatever is loudest, and that can differ between
the sample and your render. Pin the grid explicitly for your own renders
(the first beat is at 0) instead of re-detecting it.

## 4. The pattern

With a grid, everything becomes a table. Average over all bars:

- **Level per 16th step per band** — this is the drum pattern: kick on 0/4/8/12,
  hats on the offbeats, and so on.
- **Level per beat-in-bar** — backbeats, fills.
- **Level per bar** — the arrangement: what enters at bar 9, what builds.
- **An envelope over one beat in 20 ms columns per band** — the shape of a
  kick, of a duck, of a rumble blooming between kicks.

Guard against leakage: a hit a few ms early lands in the previous slot and
inflates it. Cut the last ~15 ms off each slot, or align slots to the hit.

## 5. The sounds

- **Pitch:** `librosa.yin` on a lowpassed copy for a kick's pitch drop or a
  bass note; a CQT for anything polyphonic. Print the trajectory in ms, not an
  average: a kick that falls 190 → 45 Hz in 60 ms is a different instrument
  from one that sits at 45 Hz.
- **Harmonics:** FFT a steady part, list each harmonic's level relative to the
  first. A saw falls about 6 dB per octave; a flat series with notches means
  distortion, detuning or a phaser; missing even harmonics mean a square.
- **Noise vs. tone:** `librosa.effects.hpss`, then compare energies. Spectral
  flatness per band separates hiss from pitch.
- **Notes:** transcribe per step with a CQT, and verify by template matching —
  render each candidate note with your patch, then fit the sample's spectrum
  as a non-negative combination of those templates (`scipy.optimize.nnls`).
  It catches the notes a peak-picker gets wrong.
- **Stereo:** L/R correlation and side/mid ratio **per band**. Club masters are
  mono at the bottom and wide in the mids; that is two layers in the song, not
  one, because mat has no per-track width.

## 6. Build, then measure the render the same way

Write the `.song`, render with `-o`, and run the same comparison on both. A
compact report that prints reference and render side by side is worth more
than any single number:

- band shares (% of total energy per band)
- L/R correlation per band
- level per 16th, per band
- the envelope over a beat, per band
- pitch and harmonic list of the main tonal element
- LUFS, LRA, true peak

Change one thing at a time, and when two settings trade against each other
(brightness against pulse depth, sub against body), render a small grid of
variants and pick by the numbers.

## 7. What to tell the user

- What the sample is: tempo, bars, key/notes, instruments, loudness, stereo.
- How close the rebuild is, as a table of measured differences.
- What is a guess (anything inaudible under something else, anything at the
  edges of the file) and what is impossible (their exact samples, a mastered
  recording's exact character).
- Ask them to listen: build an A/B file — reference, silence, render, at
  matched loudness — and let them tell you what is off. Their ears outrank
  every number here.

## Rules of thumb

- A hard-ducked low layer that blooms between kicks is a rumble, not a long
  kick: look at where the sub peaks inside the beat.
- Anything below ~30 Hz is inaudible and only eats headroom; if the sample has
  none there, the rebuild should not either.
- A loudness gap usually means missing clipping/limiting, not missing gain:
  check the crest factor before turning anything up.
- If band shares match but it still sounds wrong, the difference is in the
  timbre of single hits: compare single-hit envelopes and harmonic lists, not
  averages over the whole file.
