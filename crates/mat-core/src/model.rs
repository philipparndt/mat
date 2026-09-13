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
}

impl DrumKind {
    pub const ALL: [DrumKind; 9] =
        [Self::Kick, Self::Snare, Self::Clap, Self::Hat, Self::OpenHat, Self::Tom, Self::Rim, Self::Crash, Self::Ride];

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
}

impl Default for Filter {
    fn default() -> Self {
        Self { mode: FilterMode::Off, cutoff_hz: 20_000.0, resonance: 0.0, env_octaves: 0.0, keytrack: 0.0, drive: 0.0 }
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
    pub filter: Filter,
    pub amp: Adsr,
    pub filter_env: Adsr,
    pub vibrato: Vibrato,
    pub lfos: Vec<Lfo>,
    /// Random detune per note in cents, like drifting analog oscillators.
    pub drift_cents: f32,
}

impl Default for SynthDef {
    fn default() -> Self {
        Self {
            oscillators: Vec::new(),
            noise: 0.0,
            filter: Filter::default(),
            amp: Adsr { attack: 0.005, decay: 0.2, sustain: 0.8, release: 0.2 },
            filter_env: Adsr { attack: 0.005, decay: 0.3, sustain: 0.0, release: 0.3 },
            vibrato: Vibrato::default(),
            lfos: Vec::new(),
            drift_cents: 0.0,
        }
    }
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
    pub voices: [DrumVoice; 9],
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
    #[serde(rename = "au")]
    AudioUnit(AudioUnitDef),
    Audio(AudioSource),
    Clap(ClapDef),
    Tb303(Tb303Def),
}

#[derive(Debug, Clone)]
pub struct Instrument {
    pub name: String,
    pub span: Span,
    pub kind: InstrumentKind,
}

#[derive(Debug, Clone)]
pub enum TrackStep {
    Play { pattern: String, span: Span, repeat: u32, transpose: f32, velocity: f32 },
    /// Audio tracks: source bars `from..=to` (1-based), or the whole file.
    PlayAudio { bars: Option<(f64, f64)>, repeat: u32 },
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
    /// Stem group for `mat render --stems`; defaults to the track name.
    pub layer: Option<String>,
    pub eq: Option<EqSettings>,
    pub chorus: Option<ChorusSettings>,
    pub sidechain: Option<SidechainSettings>,
    pub audio: Option<(AudioSource, Span)>,
    pub sweeps: Vec<SweepDef>,
    pub steps: Vec<TrackStep>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReverbSettings {
    pub enabled: bool,
    pub size: f32,
    pub decay: f32,
    pub damping: f32,
    pub predelay_ms: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DelaySettings {
    pub enabled: bool,
    pub time: Whole,
    pub feedback: f32,
    pub tone_hz: f32,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct Master {
    pub gain_db: f32,
    pub comp: Option<CompSettings>,
    /// 0..1 tape-style saturation before the limiter.
    pub saturation: f32,
    pub reverb: ReverbSettings,
    pub delay: DelaySettings,
    pub limiter: LimiterSettings,
}

impl Default for Master {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            comp: None,
            saturation: 0.0,
            reverb: ReverbSettings { enabled: true, size: 0.7, decay: 0.7, damping: 0.4, predelay_ms: 20.0 },
            delay: DelaySettings { enabled: true, time: 3.0 / 16.0, feedback: 0.35, tone_hz: 3000.0 },
            limiter: LimiterSettings { enabled: true, ceiling_db: -1.0, release_ms: 80.0 },
        }
    }
}

/// A named range of bars, for navigation and for game engines.
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub name: String,
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
}

impl Song {
    pub fn bar_length(&self) -> Whole {
        self.meter.0 as f64 / self.meter.1 as f64
    }

    pub fn seconds(&self, w: Whole) -> f64 {
        w * 4.0 * 60.0 / self.tempo
    }
}
