//! The parsed song. Musical time is measured in whole notes (`Whole`).

use serde::Serialize;

use crate::diag::Span;

pub type Whole = f64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DrumKind {
    Kick,
    Snare,
    Clap,
    Hat,
    OpenHat,
    Tom,
    Rim,
    Crash,
    Ride,
    // Scratch moves, played by `scratch` instruments.
    Baby,
    Fwd,
    Back,
    Scribble,
    Chirp,
    Transform,
}

impl DrumKind {
    pub const ALL: [DrumKind; 15] = [
        Self::Kick,
        Self::Snare,
        Self::Clap,
        Self::Hat,
        Self::OpenHat,
        Self::Tom,
        Self::Rim,
        Self::Crash,
        Self::Ride,
        Self::Baby,
        Self::Fwd,
        Self::Back,
        Self::Scribble,
        Self::Chirp,
        Self::Transform,
    ];
    pub const SCRATCHES: [DrumKind; 6] = [Self::Baby, Self::Fwd, Self::Back, Self::Scribble, Self::Chirp, Self::Transform];

    pub fn is_scratch(self) -> bool {
        Self::SCRATCHES.contains(&self)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Kick => "kick",
            Self::Snare => "snare",
            Self::Clap => "clap",
            Self::Hat => "hat",
            Self::OpenHat => "openhat",
            Self::Tom => "tom",
            Self::Rim => "rim",
            Self::Crash => "crash",
            Self::Ride => "ride",
            Self::Baby => "baby",
            Self::Fwd => "fwd",
            Self::Back => "back",
            Self::Scribble => "scribble",
            Self::Chirp => "chirp",
            Self::Transform => "transform",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    /// General MIDI percussion note, used when a drum part is played by a sampler.
    pub fn gm_note(self) -> u8 {
        match self {
            Self::Kick => 36,
            Self::Rim => 37,
            Self::Snare => 38,
            Self::Clap => 39,
            Self::Hat => 42,
            Self::Tom => 45,
            Self::OpenHat => 46,
            Self::Crash => 49,
            Self::Ride => 51,
            Self::Baby => 60,
            Self::Fwd => 61,
            Self::Back => 62,
            Self::Scribble => 63,
            Self::Chirp => 64,
            Self::Transform => 65,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum Pitch {
    /// MIDI note number (60 = C4).
    Note(f32),
    Drum(DrumKind),
}

#[derive(Debug, Clone, Serialize)]
pub struct PatternEvent {
    /// The source line the event was written on, 1-based: where in the text
    /// a note is, for an editor to show where in the song it sounds. Never
    /// part of a render's input, so moving a line changes nothing heard.
    pub line: usize,
    /// Where on that line, as the parser's spans count: the 1-based character
    /// the note's token starts at, and how many characters it is — a grid
    /// row's one cell. For an editor to light the note while it sounds; like
    /// `line`, never part of a render's input.
    pub col: usize,
    pub len: usize,
    pub start: Whole,
    pub duration: Whole,
    pub pitch: Pitch,
    /// 0..1
    pub velocity: f32,
    /// `!` after a note.
    pub accent: bool,
    /// `~` after a note: glide into the next note without retriggering.
    pub slide: bool,
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub name: String,
    pub span: Span,
    pub length: Whole,
    pub events: Vec<PatternEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Waveform {
    Sine,
    Triangle,
    Saw,
    Square,
}

#[derive(Debug, Clone, Serialize)]
pub struct Oscillator {
    pub wave: Waveform,
    pub level: f32,
    pub octave: i32,
    pub semitones: f32,
    pub detune_cents: f32,
    pub voices: u32,
    pub spread_cents: f32,
    pub width: f32,
    /// Set for the JP-8000 style supersaw.
    pub supersaw: Option<SuperSaw>,
    /// Phase modulation by a sine at `fm_ratio` times the pitch; `fm_index`
    /// is the depth in radians and `fm_env` how much the filter envelope
    /// scales it (classic FM plucks and bells).
    pub fm_index: f32,
    pub fm_ratio: f32,
    pub fm_env: f32,
    /// Pulse width of square waves, 0.05..0.95 (0.5 = symmetric).
    pub pw: f32,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SuperSaw {
    /// 0..1, mapped through the nonlinear detune curve of the original.
    pub detune: f32,
    /// 0..1, balance between the center saw and the six detuned ones.
    pub mix: f32,
}

impl Default for Oscillator {
    fn default() -> Self {
        Self {
            wave: Waveform::Saw,
            level: 1.0,
            octave: 0,
            semitones: 0.0,
            detune_cents: 0.0,
            voices: 1,
            spread_cents: 15.0,
            width: 0.8,
            supersaw: None,
            fm_index: 0.0,
            fm_ratio: 1.0,
            fm_env: 0.0,
            pw: 0.5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterMode {
    Off,
    Lowpass,
    Highpass,
    Bandpass,
}

#[derive(Debug, Clone, Serialize)]
pub struct Filter {
    pub mode: FilterMode,
    pub cutoff_hz: f32,
    pub resonance: f32,
    /// Filter envelope depth in octaves.
    pub env_octaves: f32,
    pub keytrack: f32,
    pub drive: f32,
    /// dB per octave: 12 (state variable) or, for a lowpass, 24 (ladder).
    pub slope: u32,
}

impl Default for Filter {
    fn default() -> Self {
        Self { mode: FilterMode::Off, cutoff_hz: 20_000.0, resonance: 0.0, env_octaves: 0.0, keytrack: 0.0, drive: 0.0, slope: 12 }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Adsr {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Vibrato {
    pub rate_hz: f32,
    pub depth_cents: f32,
    pub delay: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LfoTarget {
    Filter,
    Pitch,
    Pan,
    Amp,
    Width,
    /// Pulse width, depth 0..0.45.
    Pw,
    /// FM index, depth in radians.
    Fm,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Lfo {
    pub target: LfoTarget,
    pub rate_hz: f32,
    /// Filter: octaves, pitch: cents, pan/amp/width: 0..1.
    pub depth: f32,
    /// Seconds until the LFO reaches full depth.
    pub fade_in: f32,
    /// Start phase 0..1; `None` picks a random phase per note (free running).
    pub phase: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SynthDef {
    pub oscillators: Vec<Oscillator>,
    pub noise: f32,
    /// Filters in series. A `filter` line with the mode of an existing one replaces it.
    pub filters: Vec<Filter>,
    pub amp: Adsr,
    pub filter_env: Adsr,
    pub vibrato: Vibrato,
    pub lfos: Vec<Lfo>,
    /// Random detune per note in cents, like drifting analog oscillators.
    pub drift_cents: f32,
    /// Portamento: seconds to glide from the previous note's pitch (0 = off).
    pub glide: f32,
    /// Pitch envelope: starts `depth` semitones away and decays to the note.
    pub pitch_env: Option<(f32, f32)>,
    /// Notes that follow one another without a rest are one phrase: the
    /// envelopes and the vibrato carry on instead of starting again.
    pub legato: bool,
}

impl Default for SynthDef {
    fn default() -> Self {
        Self {
            oscillators: Vec::new(),
            noise: 0.0,
            filters: Vec::new(),
            amp: Adsr { attack: 0.005, decay: 0.2, sustain: 0.8, release: 0.2 },
            filter_env: Adsr { attack: 0.005, decay: 0.3, sustain: 0.0, release: 0.3 },
            vibrato: Vibrato::default(),
            lfos: Vec::new(),
            drift_cents: 0.0,
            glide: 0.0,
            pitch_env: None,
            legato: false,
        }
    }
}

/// One operator of an `fm` instrument.
#[derive(Debug, Clone, Serialize)]
pub struct FmOperator {
    /// Frequency as a multiple of the note's, unless `fixed_hz` is set.
    pub ratio: f32,
    pub fixed_hz: Option<f32>,
    pub detune_cents: f32,
    /// 0..1 in steps of 6 dB per 0.125: loudness for a carrier, modulation depth for a modulator.
    pub level: f32,
    /// How much of the level a soft note loses (0..1).
    pub velocity: f32,
    /// How much of the level is lost per octave above C4 (1 = 6 dB).
    pub keyscale: f32,
    pub feedback: f32,
    pub env: Adsr,
    /// The operator this one modulates (index into `operators`), or none for a carrier.
    pub into: Option<usize>,
}

impl Default for FmOperator {
    fn default() -> Self {
        Self { ratio: 1.0, fixed_hz: None, detune_cents: 0.0, level: 1.0, velocity: 0.5, keyscale: 0.0, feedback: 0.0, env: Adsr { attack: 0.002, decay: 0.6, sustain: 0.6, release: 0.25 }, into: None }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct FmDef {
    pub operators: Vec<FmOperator>,
    pub vibrato: Vibrato,
    pub drift_cents: f32,
    /// Detune between the left and the right side in cents, for width.
    pub stereo_cents: f32,
    pub pitch_env: Option<(f32, f32)>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DrumVoice {
    pub gain_db: f32,
    pub tune: f32,
    pub decay: f32,
}

impl Default for DrumVoice {
    fn default() -> Self {
        Self { gain_db: 0.0, tune: 0.0, decay: 1.0 }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DrumKit {
    /// Indexed like `DrumKind::ALL`.
    pub voices: [DrumVoice; 15],
}

impl DrumKit {
    pub fn voice(&self, kind: DrumKind) -> &DrumVoice {
        &self.voices[kind as usize]
    }
}

/// One region of an audio file used as a sample.
#[derive(Debug, Clone, Serialize)]
pub struct SampleZone {
    pub path: String,
    /// Drum name, or `None` for a pitched zone.
    pub drum: Option<DrumKind>,
    /// MIDI note the region was recorded at (pitched zones).
    pub root: f32,
    pub key_low: u8,
    pub key_high: u8,
    pub vel_low: u8,
    pub vel_high: u8,
    /// Region in the file, seconds.
    pub start: f64,
    pub length: Option<f64>,
    /// Loop points relative to the region start, seconds.
    pub loop_range: Option<(f64, f64)>,
    pub gain_db: f32,
    pub tune: f32,
}

/// Samples cut straight out of audio files (stems, recordings), played by
/// the built-in sampler with the [`SamplerDef`] settings.
#[derive(Debug, Clone, Serialize)]
pub struct SamplesDef {
    pub zones: Vec<SampleZone>,
    pub settings: SamplerDef,
}

/// A turntable: one audio region that scratch moves play forward and back.
#[derive(Debug, Clone, Serialize)]
pub struct ScratchDef {
    /// Audio file, or empty when `source_track` is set.
    pub path: String,
    /// Scratch another track's audio instead of a file (bars converted to
    /// `start`/`length` seconds during arrangement).
    pub source_track: Option<String>,
    pub start: f64,
    pub length: Option<f64>,
    /// How far the record travels per move, 1 = normal playback speed on average.
    pub speed: f32,
    /// Keep the original pitch whatever the speed (time-stretch) instead of
    /// pitching up and down like vinyl.
    pub keep_pitch: bool,
    /// Grain length in seconds for `keep_pitch`: short grains (≤ 60 ms) overlap
    /// smoothly for tonal material, longer ones play as slices with transients intact.
    pub grain: f32,
    pub gain_db: f32,
}

/// A sampled instrument played by the built-in sampler (Logic/GarageBand `.exs`).
#[derive(Debug, Clone, Serialize)]
pub struct SamplerDef {
    /// Instrument file, absolute after path resolution.
    pub load: String,
    pub gain_db: f32,
    pub tune: f32,
    pub attack: f32,
    pub release: f32,
    /// Level difference between the softest and the loudest velocity, in dB.
    pub velocity_db: f32,
    /// Articulation to play (e.g. legato or staccato); `None` picks the first one.
    pub articulation: Option<u8>,
    /// Drum names mapped to other MIDI notes than General MIDI.
    pub drum_map: Vec<(DrumKind, u8)>,
}

/// An external Audio Unit instrument. Rendered by the macOS host (`mat-au`),
/// not by the built-in engine.
#[derive(Debug, Clone, Serialize)]
pub struct AudioUnitDef {
    /// Four-char codes: type, subtype, manufacturer. Default: Apple AUSampler.
    pub component: [String; 3],
    /// Instrument file to load (.exs, .sf2, .dls, .aupreset), absolute.
    pub load: Option<String>,
    pub program: Option<u8>,
    pub gain_db: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tb303Param {
    Cutoff,
    Resonance,
    EnvMod,
    Decay,
    Accent,
    Drive,
}

impl Tb303Param {
    pub const NAMES: [&str; 6] = ["cutoff", "resonance", "envmod", "decay", "accent", "drive"];

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "cutoff" => Self::Cutoff,
            "resonance" | "res" => Self::Resonance,
            "envmod" => Self::EnvMod,
            "decay" => Self::Decay,
            "accent" => Self::Accent,
            "drive" => Self::Drive,
            _ => return None,
        })
    }
}

/// TB-303 style bass line synthesizer. Knobs are 0..1 like the original panel.
#[derive(Debug, Clone, Serialize)]
pub struct Tb303Def {
    pub square: bool,
    pub cutoff: f32,
    pub resonance: f32,
    pub env_mod: f32,
    pub decay: f32,
    pub accent: f32,
    pub drive: f32,
    /// Fraction of a note the gate is held (the hardware holds half a step).
    pub gate: f32,
    pub slide_time: f32,
    pub tune: f32,
}

impl Default for Tb303Def {
    fn default() -> Self {
        Self { square: false, cutoff: 0.35, resonance: 0.7, env_mod: 0.5, decay: 0.4, accent: 0.6, drive: 0.0, gate: 0.5, slide_time: 0.06, tune: 0.0 }
    }
}

impl Tb303Def {
    pub fn get(&self, p: Tb303Param) -> f32 {
        match p {
            Tb303Param::Cutoff => self.cutoff,
            Tb303Param::Resonance => self.resonance,
            Tb303Param::EnvMod => self.env_mod,
            Tb303Param::Decay => self.decay,
            Tb303Param::Accent => self.accent,
            Tb303Param::Drive => self.drive,
        }
    }
}

/// Random timing and velocity variation, per track.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Humanize {
    /// Maximum timing offset in seconds (uniform, both directions).
    pub time: f32,
    /// Maximum velocity change, 0..1.
    pub velocity: f32,
}

/// A linear parameter change over a range of bars.
#[derive(Debug, Clone)]
pub struct SweepDef {
    pub param: String,
    pub span: Span,
    pub from: f32,
    pub to: f32,
    pub from_bar: f64,
    pub to_bar: f64,
}

/// A CLAP plugin instrument (for example Surge XT).
#[derive(Debug, Clone, Serialize)]
pub struct ClapDef {
    /// Plugin name (searched in the standard CLAP folders) or path.
    pub plugin: String,
    pub plugin_id: Option<String>,
    /// Preset file, absolute after path resolution.
    pub patch: Option<String>,
    /// Parameters by name, as plain plugin values.
    pub params: Vec<(String, f64)>,
    pub gain_db: f32,
}

/// A recorded audio file (for example a stem) placed on a track.
#[derive(Debug, Clone, Serialize)]
pub struct AudioSource {
    pub path: String,
    /// Seconds into the file where bar 1 starts.
    pub offset: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EqSettings {
    pub lowcut_hz: f32,
    pub low_db: f32,
    pub low_freq_hz: f32,
    pub mid_db: f32,
    pub mid_freq_hz: f32,
    pub high_db: f32,
    pub high_freq_hz: f32,
    pub highcut_hz: f32,
}

impl Default for EqSettings {
    fn default() -> Self {
        Self { lowcut_hz: 0.0, low_db: 0.0, low_freq_hz: 200.0, mid_db: 0.0, mid_freq_hz: 1000.0, high_db: 0.0, high_freq_hz: 6000.0, highcut_hz: 0.0 }
    }
}

/// The shape `distortion` bends a signal through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DistortionMode {
    Soft,
    Hard,
    Fold,
    Fuzz,
}

#[derive(Debug, Clone, Serialize)]
pub struct DistortionSettings {
    /// 0..1: how hard the signal is pushed into the shaper.
    pub drive: f32,
    pub mode: DistortionMode,
    /// Lowpass after the shaper; 0 is none.
    pub tone_hz: f32,
    /// Bit depth of the crusher; 0 is none.
    pub bits: u32,
    /// Sample rate of the crusher; 0 is none.
    pub rate_hz: f32,
    pub mix: f32,
    pub level_db: f32,
}

impl Default for DistortionSettings {
    fn default() -> Self {
        Self { drive: 0.5, mode: DistortionMode::Soft, tone_hz: 6000.0, bits: 0, rate_hz: 0.0, mix: 1.0, level_db: 0.0 }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaserSettings {
    pub rate_hz: f32,
    pub depth: f32,
    pub stages: u32,
    pub feedback: f32,
    pub mix: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChorusSettings {
    pub mix: f32,
    pub rate_hz: f32,
    pub depth_ms: f32,
}

impl Default for ChorusSettings {
    fn default() -> Self {
        Self { mix: 0.5, rate_hz: 0.7, depth_ms: 4.0 }
    }
}

/// Ducks a track whenever another track plays (usually the kick).
#[derive(Debug, Clone)]
pub struct SidechainSettings {
    pub source: String,
    pub span: Span,
    pub drum: Option<DrumKind>,
    pub depth: f32,
    pub attack: f32,
    pub release: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum InstrumentKind {
    Synth(SynthDef),
    Drums(DrumKit),
    Sampler(SamplerDef),
    Samples(SamplesDef),
    Scratch(ScratchDef),
    #[serde(rename = "au")]
    AudioUnit(AudioUnitDef),
    Audio(AudioSource),
    Clap(ClapDef),
    Tb303(Tb303Def),
    Fm(FmDef),
}

#[derive(Debug, Clone)]
pub struct Instrument {
    pub name: String,
    pub span: Span,
    pub kind: InstrumentKind,
}

#[derive(Debug, Clone)]
pub enum TrackStep {
    /// `muted`: `play beat x4 mute` — the block keeps its place and its length
    /// on the timeline and plays nothing, so what follows it stays where it is.
    Play { pattern: String, span: Span, repeat: u32, transpose: f32, velocity: f32, muted: bool },
    /// Audio tracks: source bars `from..=to` (1-based), or the whole file.
    /// `line` is where it was written, 1-based, for an editor.
    PlayAudio { bars: Option<(f64, f64)>, repeat: u32, line: usize, muted: bool },
    Rest { bars: f64 },
    At { bar: f64 },
}

#[derive(Debug, Clone)]
pub struct Track {
    pub name: String,
    pub span: Span,
    pub instrument: Option<(String, Span)>,
    pub gain_db: f32,
    pub pan: f32,
    pub reverb: f32,
    pub delay: f32,
    pub mute: bool,
    /// Swing amount 0.5..0.75 (0.5 = straight); overrides the song's.
    pub swing: Option<f32>,
    /// Grid the swing acts on, in whole notes (default 1/16).
    pub swing_grid: Option<Whole>,
    pub humanize: Option<Humanize>,
    /// `seed <n>` on a track: this track's take, instead of the song's.
    pub seed: Option<u64>,
    pub comp: Option<CompSettings>,
    pub phaser: Option<PhaserSettings>,
    pub distortion: Option<DistortionSettings>,
    /// Stem group for `mat render --stems`; defaults to the track name.
    pub layer: Option<String>,
    pub eq: Option<EqSettings>,
    pub chorus: Option<ChorusSettings>,
    pub sidechain: Option<SidechainSettings>,
    pub audio: Option<(AudioSource, Span)>,
    pub sweeps: Vec<SweepDef>,
    /// Written out: a `repeat` block's steps are here once for every pass.
    pub steps: Vec<TrackStep>,
    /// Where the `repeat` blocks are in `steps`. For an editor to place the
    /// `repeat` line; `arrange` reads only the steps.
    pub repeats: Vec<RepeatBlock>,
}

/// A `repeat N { … }` block of a track, written out: its passes are
/// `steps[first..end]`. A block inside another is here once per outer pass.
#[derive(Debug, Clone, PartialEq)]
pub struct RepeatBlock {
    /// The `repeat` keyword.
    pub span: Span,
    pub first: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReverbSettings {
    pub enabled: bool,
    pub size: f32,
    pub decay: f32,
    pub damping: f32,
    pub predelay_ms: f32,
    /// Octave-up feedback into the tail, 0..1.
    pub shimmer: f32,
    /// Filters on the wet signal (0 = off).
    pub lowcut_hz: f32,
    pub highcut_hz: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DelaySettings {
    pub enabled: bool,
    pub time: Whole,
    pub feedback: f32,
    pub tone_hz: f32,
    /// Delay-time modulation in ms and its rate (chorused echoes).
    pub mod_ms: f32,
    pub mod_rate_hz: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct LimiterSettings {
    pub enabled: bool,
    pub ceiling_db: f32,
    pub release_ms: f32,
}

/// Bus compressor on the master.
#[derive(Debug, Clone, Serialize)]
pub struct CompSettings {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack: f32,
    pub release: f32,
    pub makeup_db: f32,
    /// Feedback topology: the detector listens after the gain stage, which
    /// reacts more gently and pumps musically (vintage style).
    pub feedback: bool,
}

/// Keyed compression on the master: the named track (or layer) is the key,
/// everything else is compressed and, with `darken`, low-passed on every hit.
#[derive(Debug, Clone, Serialize)]
pub struct MasterSidechain {
    pub source: String,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack: f32,
    pub release: f32,
    /// Octaves the lowpass drops per 12 dB of gain reduction (0 = off).
    pub darken: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Master {
    pub gain_db: f32,
    pub sidechain: Option<MasterSidechain>,
    pub eq: Option<EqSettings>,
    /// Stereo width: 1 = as mixed, 0 = mono, up to 2.
    pub width: f32,
    pub comp: Option<CompSettings>,
    /// 0..1 tape-style saturation before the limiter.
    pub saturation: f32,
    /// Soft clipper threshold in dBFS before the limiter (0 = off). Shaves
    /// transient peaks so the limiter can run hotter.
    pub clip_db: f32,
    pub reverb: ReverbSettings,
    pub delay: DelaySettings,
    pub limiter: LimiterSettings,
}

impl Default for Master {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            sidechain: None,
            eq: None,
            width: 1.0,
            clip_db: 0.0,
            comp: None,
            saturation: 0.0,
            reverb: ReverbSettings { enabled: true, size: 0.7, decay: 0.7, damping: 0.4, predelay_ms: 20.0, shimmer: 0.0, lowcut_hz: 0.0, highcut_hz: 0.0 },
            delay: DelaySettings { enabled: true, time: 3.0 / 16.0, feedback: 0.35, tone_hz: 3000.0, mod_ms: 0.0, mod_rate_hz: 0.5 },
            limiter: LimiterSettings { enabled: true, ceiling_db: -1.0, release_ms: 80.0 },
        }
    }
}

/// A named range of bars, for navigation and for game engines.
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub name: String,
    /// The file of the song it is written in (see `Song::sources`) and its
    /// line, 1-based; for an editor, never serialised.
    #[serde(skip)]
    pub file: usize,
    #[serde(skip)]
    pub line: usize,
    pub from_bar: f64,
    pub to_bar: f64,
}

#[derive(Debug, Clone)]
pub struct Song {
    pub title: Option<String>,
    /// Quarter notes per minute.
    pub tempo: f64,
    pub meter: (u32, u32),
    pub instruments: Vec<Instrument>,
    pub patterns: Vec<Pattern>,
    pub tracks: Vec<Track>,
    pub master: Master,
    pub sections: Vec<Section>,
    /// Song-wide swing (0.5 = straight) and its grid.
    pub swing: f32,
    pub swing_grid: Whole,
    /// `seed <n>` at the top: which take of every random thing the song gets —
    /// drift, unison spread, LFO phases, humanize. 0 unless the song says.
    pub seed: u64,
    /// The files the song was read from, by the index a `Span::file` names:
    /// the song itself first, then each file it includes in the order they
    /// were first read. An empty path for a song parsed from text alone.
    /// Never part of a render's input.
    pub sources: Vec<std::path::PathBuf>,
}

impl Song {
    pub fn bar_length(&self) -> Whole {
        self.meter.0 as f64 / self.meter.1 as f64
    }

    pub fn seconds(&self, w: Whole) -> f64 {
        w * 4.0 * 60.0 / self.tempo
    }
}
