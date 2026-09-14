//! What the keywords mean, in a line each — the sentences of `docs/FORMAT.md`,
//! shortened for a hover and a completion list.

/// A keyword and its one line.
pub type Doc = (&'static str, &'static str);

pub fn find(entries: &[Doc], word: &str) -> Option<&'static str> {
    entries.iter().find(|(w, _)| *w == word).map(|(_, d)| *d)
}

pub const TOP_LEVEL: &[Doc] = &[
    ("title", "The song's title, in quotes."),
    ("tempo", "Quarter notes per minute."),
    ("meter", "Beats per bar over the beat's note value, like 4/4."),
    ("swing", "Song-wide swing: 0.5 straight, 0.66 triplet feel; `grid=1/8` chooses the grid."),
    ("section", "A named range of bars, `section chorus bars=17-32`, for navigation and game engines."),
    ("instrument", "An instrument: `instrument <name> <synth|drums|tb303|samples|scratch|sampler|clap|au>`, or `preset <name>`."),
    ("pattern", "A pattern of notes or a drum grid: `pattern <name> [grid=1/16] [bars=n] [pedal]`."),
    ("track", "A track: an instrument and the patterns it plays, with gain, pan, sends and effects."),
    ("master", "The master chain: gain, sidechain, eq, width, reverb, delay, saturation, comp, clip, limiter."),
];

pub const INSTRUMENT_KINDS: &[Doc] = &[
    ("synth", "Subtractive synthesizer: oscillators, noise, filters, envelopes, LFOs, glide."),
    ("drums", "Synthesized drum kit: kick, snare, clap, hat, openhat, tom, rim, crash, ride."),
    ("tb303", "Acid bass line synthesizer with accents and slides."),
    ("samples", "Your own samples, cut from audio files by time."),
    ("scratch", "Turntable scratching over a sample or another track's audio."),
    ("sampler", "Logic / GarageBand `.exs` sampler instruments."),
    ("clap", "A CLAP plugin instrument, such as Surge XT."),
    ("au", "An Audio Unit instrument (macOS), rendered by mat-au."),
    ("preset", "Start from a built-in preset, then override or add settings below."),
];

pub const PATTERN_HEADER: &[Doc] = &[
    ("grid=1/16", "A grid pattern: one cell per step, rows starting with a drum or a pitch."),
    ("bars=", "The pattern's length in bars; otherwise rounded up to whole bars."),
    ("pedal", "Hold every note until the end of its bar, like a sustain pedal lifted on each chord."),
];

pub const SYNTH_SETTINGS: &[Doc] = &[
    ("osc", "An oscillator: `osc <sine|triangle|saw|square|supersaw> level= octave= semi= detune= voices= spread= width= pw= fm= fmratio= fmenv=`. One line per oscillator."),
    ("noise", "White noise level, 0 to 1."),
    ("filter", "`filter <lowpass|highpass|bandpass> cutoff= res= env= keytrack= drive=`; several lines chain; `filter off` removes them all."),
    ("amp", "Amplitude envelope: `attack= decay= sustain= release=` in seconds or ms."),
    ("fenv", "Filter envelope: `attack= decay= sustain= release=`."),
    ("vibrato", "`rate=` Hz, `depth=` cents, `delay=` seconds before it sets in."),
    ("lfo", "`lfo <filter|pitch|pan|amp|width|pw|fm> rate= depth= fade= phase=`; one line per LFO."),
    ("glide", "Portamento time from the previous note's pitch."),
    ("penv", "Pitch envelope: starts `depth=` semitones away and decays to the note over `decay=`."),
    ("drift", "Random detune per note, in cents, like drifting analog oscillators."),
    ("gain", "Gain in dB."),
];

pub const DRUMS: &[Doc] = &[
    ("kick", "The kick drum."),
    ("snare", "The snare."),
    ("clap", "A hand clap."),
    ("hat", "A closed hi-hat; it cuts an open hat off."),
    ("openhat", "An open hi-hat, cut off by the next hat."),
    ("tom", "A tom."),
    ("rim", "A rim shot."),
    ("crash", "A crash cymbal."),
    ("ride", "A ride cymbal."),
];

pub const DRUMS_SETTINGS: &[Doc] = &[
    ("kick", "`gain=` dB, `tune=` semitones, `decay=` multiplier."),
    ("snare", "`gain=` dB, `tune=` semitones, `decay=` multiplier."),
    ("clap", "`gain=` dB, `tune=` semitones, `decay=` multiplier."),
    ("hat", "`gain=` dB, `tune=` semitones, `decay=` multiplier."),
    ("openhat", "`gain=` dB, `tune=` semitones, `decay=` multiplier."),
    ("tom", "`gain=` dB, `tune=` semitones, `decay=` multiplier."),
    ("rim", "`gain=` dB, `tune=` semitones, `decay=` multiplier."),
    ("crash", "`gain=` dB, `tune=` semitones, `decay=` multiplier."),
    ("ride", "`gain=` dB, `tune=` semitones, `decay=` multiplier."),
    ("gain", "Gain in dB."),
];

pub const TB303_SETTINGS: &[Doc] = &[
    ("wave", "`saw` or `square`."),
    ("cutoff", "The cutoff knob, 0 to 1."),
    ("resonance", "The resonance knob, 0 to 1."),
    ("envmod", "Envelope modulation, 0 to 1."),
    ("decay", "Envelope decay, 0 to 1."),
    ("accent", "Accent amount, 0 to 1; accented notes (`!`) open the filter."),
    ("drive", "Overdrive after the VCA, 0 to 1."),
    ("gate", "Fraction of each note the gate is held."),
    ("slide", "Slide time, like `60ms`; a `~` after a note glides into the next."),
    ("gain", "Gain in dB."),
];

pub const SAMPLES_SETTINGS: &[Doc] = &[
    ("kick", "`kick \"file.wav\" at=41.52s length=0.42s gain=+4`: a drum cut from a file."),
    ("snare", "A drum cut from a file: `\"file.wav\" at= length= gain= tune=`."),
    ("clap", "A drum cut from a file: `\"file.wav\" at= length= gain= tune=`."),
    ("hat", "A drum cut from a file: `\"file.wav\" at= length= gain= tune=`."),
    ("openhat", "A drum cut from a file: `\"file.wav\" at= length= gain= tune=`."),
    ("tom", "A drum cut from a file: `\"file.wav\" at= length= gain= tune=`."),
    ("rim", "A drum cut from a file: `\"file.wav\" at= length= gain= tune=`."),
    ("crash", "A drum cut from a file: `\"file.wav\" at= length= gain= tune=`."),
    ("ride", "A drum cut from a file: `\"file.wav\" at= length= gain= tune=`."),
    ("note", "`note G2 \"file.wav\" at= length= loop= keys=C1-B3 vel=1-127`: a pitched region, played across `keys=`."),
    ("amp", "Amplitude envelope: `attack= decay= sustain= release=`."),
    ("velocity", "dB between the softest and loudest note."),
    ("gain", "Gain in dB."),
];

pub const SCRATCH_SETTINGS: &[Doc] = &[
    ("sample", "`sample \"vocal.wav\" at=0.2s length=0.6s`: the bit of record under the needle."),
    ("source", "`source <track> bars=49-49`: another track's dry audio as the record, or `source mix bars=8-8`."),
    ("speed", "How far the record travels per move."),
    ("pitch", "`keep` time-stretches so the pitch stays; `follow` (default) is vinyl."),
    ("grain", "With `pitch keep`: grain size, like `1/16`; ≤ 60 ms is smooth, longer is beat slices."),
    ("gain", "Gain in dB."),
];

pub const SAMPLER_SETTINGS: &[Doc] = &[
    ("load", "`load \"logic:01 Acoustic Pianos/Steinway Grand Piano 2.exs\"`; `logic:` and `garageband:` are path shortcuts."),
    ("gain", "Gain in dB."),
    ("amp", "Amplitude envelope: `attack= decay= sustain= release=`."),
    ("velocity", "dB between the softest and loudest note."),
    ("articulation", "Which articulation to play, by number."),
    ("map", "Drum names to notes other than General MIDI: `map tom=43`."),
];

pub const CLAP_SETTINGS: &[Doc] = &[
    ("plugin", "`plugin \"Surge XT\"`: a name in the CLAP folders, or a path."),
    ("patch", "`patch \"surge:Polysynths/Megasynth 1.fxp\"`; `surge:` and `surge-3rdparty:` are shortcuts."),
    ("param", "`param \"A Amp EG Release\" 0.1`: a plain parameter value; list them with `mat plugin-params`."),
    ("gain", "Gain in dB."),
];

pub const AU_SETTINGS: &[Doc] = &[
    ("component", "`component aumu samp appl`: type, subtype, maker from `mat-au list`."),
    ("load", "An `.aupreset`, `.sf2` or `.dls` (with `program <0-127>`), or `gm` for the built-in bank."),
    ("program", "The program number for an `.sf2` or `.dls`."),
    ("gain", "Gain in dB."),
];

pub fn instrument_settings(kind: &str) -> &'static [Doc] {
    match kind {
        "drums" => DRUMS_SETTINGS,
        "tb303" => TB303_SETTINGS,
        "samples" => SAMPLES_SETTINGS,
        "scratch" => SCRATCH_SETTINGS,
        "sampler" => SAMPLER_SETTINGS,
        "clap" => CLAP_SETTINGS,
        "au" => AU_SETTINGS,
        _ => SYNTH_SETTINGS,
    }
}

pub const WAVES: &[Doc] = &[
    ("sine", "A sine wave."),
    ("triangle", "A triangle wave."),
    ("saw", "A sawtooth."),
    ("square", "A square wave; `pw=` sets the pulse width."),
    ("supersaw", "JP-8000 style: `detune=` 0–1 (0.5 is classic trance), `mix=` centre against detuned saws."),
];

pub const FILTER_MODES: &[Doc] = &[
    ("lowpass", "Lets the lows through."),
    ("highpass", "Lets the highs through."),
    ("bandpass", "Lets a band through."),
    ("off", "Removes every filter stage, including a preset's."),
];

pub const LFO_TARGETS: &[Doc] = &[
    ("filter", "Cutoff, `depth=` in octaves."),
    ("pitch", "Pitch, `depth=` in cents."),
    ("pan", "Pan, `depth=` 0–1."),
    ("amp", "Amplitude, `depth=` 0–1."),
    ("width", "Stereo width, `depth=` 0–1."),
    ("pw", "Pulse width, `depth=` 0–0.45."),
    ("fm", "FM index, `depth=` in radians."),
];

pub const SCRATCH_MOVES: &[Doc] = &[
    ("baby", "Forward and back."),
    ("fwd", "Forward."),
    ("back", "Back."),
    ("scribble", "Fast back-and-forth."),
    ("chirp", "Fader cuts at every reversal."),
    ("transform", "Fader stutter."),
];

pub const TRACK_SETTINGS: &[Doc] = &[
    ("instrument", "Which instrument this track plays."),
    ("gain", "Gain in dB."),
    ("pan", "-1 left to 1 right."),
    ("reverb", "Send amount into the master reverb, 0 to 1."),
    ("delay", "Send amount into the master delay, 0 to 1."),
    ("at", "Jump to a bar: `at 5`."),
    ("play", "`play <pattern> [x2] [transpose=] [vel=]`; audio tracks: `play bars=17-24 x2` or `play all`."),
    ("rest", "Skip bars: `rest 4`."),
    ("mute", "Rendered silent; still triggers sidechains."),
    ("layer", "Stem group for `mat render --stems`; defaults to the track name."),
    ("swing", "`swing 0.62 grid=1/8`: overrides the song's swing on this track."),
    ("humanize", "`time=8ms vel=10`: random timing and velocity per note, deterministic per track."),
    ("eq", "`lowcut= low= lowfreq= mid= midfreq= high= highfreq= highcut=`."),
    ("comp", "`threshold= ratio= attack= release= makeup= mode=feedforward|feedback`."),
    ("chorus", "`mix= rate= depth=`."),
    ("phaser", "`rate= depth= stages= feedback= mix=`."),
    ("sidechain", "`sidechain <track> depth= attack= release= on=kick`: duck this track on every hit of another."),
    ("sweep", "`sweep <param> from= to= bars=9-16`: move a knob, a cutoff or the gain over bars, then hold."),
    ("audio", "`audio \"file.wav\" offset=0.52s`: an audio track; put it before `play`."),
];

pub const SWEEP_PARAMETERS: &[Doc] = &[
    ("cutoff", "Filter cutoff: tb303 knob 0–1, or Hz on a synth (moves logarithmically)."),
    ("resonance", "tb303 resonance knob, 0–1."),
    ("res", "Synth filter resonance, 0–1."),
    ("envmod", "tb303 envelope modulation, 0–1."),
    ("decay", "tb303 decay, 0–1."),
    ("accent", "tb303 accent, 0–1."),
    ("drive", "tb303 drive, 0–1."),
    ("gain", "A volume curve in dB, for swells and fades."),
];

pub const MASTER_SETTINGS: &[Doc] = &[
    ("preset", "Start from a built-in master chain."),
    ("gain", "Gain in dB."),
    ("sidechain", "`sidechain <track> threshold= ratio= attack= release= darken=`: keyed compression, the key pushing everything else down."),
    ("eq", "`lowcut= low= lowfreq= mid= midfreq= high= highfreq= highcut=`."),
    ("width", "Stereo width: 0 mono, 1 as mixed, up to 2."),
    ("reverb", "`size= decay= damping= predelay= shimmer= lowcut= highcut=`, or `reverb off`."),
    ("delay", "`time=3/16 feedback= tone= mod= rate=`: a ping-pong delay; `time` is a note value."),
    ("saturation", "Tape-style saturation, 0 to 1."),
    ("comp", "Bus compressor: `threshold= ratio= attack= release= makeup=`, or `comp off`."),
    ("clip", "Soft clipper threshold in dBFS before the limiter, like `clip -4`."),
    ("limiter", "`ceiling= release=`, or `limiter off`."),
];

/// The `key=` options a setting takes, by setting and block kind.
pub fn options(setting: &str, kind: &str) -> &'static [&'static str] {
    match (setting, kind) {
        ("osc", _) => &["level", "octave", "semi", "detune", "voices", "spread", "width", "pw", "fm", "fmratio", "fmenv", "mix"],
        ("filter", _) => &["cutoff", "res", "env", "keytrack", "drive"],
        ("amp", _) | ("fenv", _) => &["attack", "decay", "sustain", "release"],
        ("vibrato", _) => &["rate", "depth", "delay"],
        ("lfo", _) => &["rate", "depth", "fade", "phase"],
        ("penv", _) => &["depth", "decay"],
        ("kick", "drums") | ("snare", "drums") | ("clap", "drums") | ("hat", "drums") | ("openhat", "drums") | ("tom", "drums")
        | ("rim", "drums") | ("crash", "drums") | ("ride", "drums") => &["gain", "tune", "decay"],
        ("kick", "samples") | ("snare", "samples") | ("clap", "samples") | ("hat", "samples") | ("openhat", "samples")
        | ("tom", "samples") | ("rim", "samples") | ("crash", "samples") | ("ride", "samples") => &["at", "length", "gain", "tune"],
        ("note", "samples") => &["at", "length", "loop", "keys", "vel", "gain", "tune"],
        ("sample", "scratch") => &["at", "length"],
        ("source", "scratch") => &["bars"],
        ("map", "sampler") => &["kick", "snare", "clap", "hat", "openhat", "tom", "rim", "crash", "ride"],
        ("eq", _) => &["lowcut", "low", "lowfreq", "mid", "midfreq", "high", "highfreq", "highcut"],
        ("comp", _) => &["threshold", "ratio", "attack", "release", "makeup", "mode"],
        ("chorus", _) => &["mix", "rate", "depth"],
        ("phaser", _) => &["rate", "depth", "stages", "feedback", "mix"],
        ("sidechain", "track") => &["depth", "attack", "release", "on"],
        ("sidechain", "master") => &["threshold", "ratio", "attack", "release", "darken"],
        ("sweep", _) => &["from", "to", "bars"],
        ("humanize", _) => &["time", "vel"],
        ("swing", _) => &["grid"],
        ("audio", _) => &["offset"],
        ("reverb", "master") => &["size", "decay", "damping", "predelay", "shimmer", "lowcut", "highcut"],
        ("delay", "master") => &["time", "feedback", "tone", "mod", "rate"],
        ("limiter", _) => &["ceiling", "release"],
        _ => &[],
    }
}
