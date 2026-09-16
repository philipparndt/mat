//! TB-303 style bass line synthesizer.
//!
//! One monophonic voice runs over the whole track, like the hardware:
//! * saw or square oscillator (PolyBLEP) with slide: legato notes glide with an
//!   RC-like curve and do not retrigger the envelopes,
//! * a nonlinear 4-pole ladder (tanh saturation, highpassed resonance feedback)
//!   running at twice the sample rate,
//! * the filter envelope (decay knob), a short accent envelope and the accent
//!   "sweep" capacitor, which builds up over consecutive accented notes,
//! * a VCA with a slow decay while the gate is held, plus optional drive.
//!
//! **A stretch at a time.** The voice is one loop over every sample of the
//! track, and everything it carries — the oscillator's phase, the ladder's four
//! stages, the slide, the envelopes, the accent capacitor — is in [`Player`].
//! So the loop can be stopped between two samples and gone on with later, and
//! [`crate::stream`] renders an acid line as its bars come round rather than
//! all of it before the first one. Stopping changes nothing: the state a
//! sample sees is the state the sample before it left, wherever the stop was.

use std::f32::consts::PI;

use crate::arrange::{Sweep, TimedNote};
use crate::dsp::StereoClip;
use crate::dsp::biquad::Biquad;
use crate::dsp::filter::OnePole;
use crate::dsp::midi_to_hz;
use crate::dsp::osc::Oscillator;
use crate::model::{Tb303Def, Tb303Param, Waveform};

const OVERSAMPLE: usize = 2;
const VOICE_GAIN: f32 = 0.5;
/// Under this a sample is silence, and a run of it at the end of the track is
/// not written.
const SILENT: f32 = 1e-5;

struct Knobs {
    def: Tb303Def,
    sweeps: Vec<(Tb303Param, Sweep)>,
}

impl Knobs {
    /// Knob value at time `t`: the latest sweep that has started wins and
    /// holds its end value afterwards.
    fn get(&self, p: Tb303Param, t: f64) -> f32 {
        let mut value = self.def.get(p);
        for (param, s) in &self.sweeps {
            if *param != p || t < s.start {
                continue;
            }
            let x = if s.end > s.start { ((t - s.start) / (s.end - s.start)).clamp(0.0, 1.0) as f32 } else { 1.0 };
            value = s.from + (s.to - s.from) * x;
        }
        value.clamp(0.0, 1.0)
    }
}

/// One step of the ladder: a trapezoidal one-pole lowpass.
#[derive(Default, Clone, Copy)]
struct Stage {
    s: f32,
}

impl Stage {
    #[inline]
    fn process(&mut self, x: f32, g: f32) -> f32 {
        let v = (x - self.s) * g;
        let y = v + self.s;
        self.s = y + v;
        y
    }
}

/// Per note: trigger sample, gate-off sample, and whether it is slid into.
struct Event {
    on: usize,
    off: usize,
    midi: f32,
    accent: bool,
    legato: bool,
}

/// The voice, and every sample of it that has been made so far.
pub struct Player {
    knobs: Knobs,
    events: Vec<Event>,
    sr: f32,
    osr: f32,
    /// The first sample of the track: where its first note starts.
    start_sample: usize,
    /// How many samples the voice runs for, the tail included.
    total: usize,
    /// How many of them have been made.
    made: usize,
    /// One past the last sample that was not silence. The render keeps this
    /// many samples — the silence after it is the tail that is trimmed — and
    /// it is only known once the voice has run out.
    loud: usize,

    square: bool,
    osc: Oscillator,
    stages: [Stage; 4],
    feedback_hp: OnePole,
    decimate: [Biquad; 2],
    dc_block: OnePole,

    slide_coef: f32,
    vca_attack: f32,
    vca_decay: f32,
    vca_release: f32,
    accent_decay: f32,
    sweep_charge: f32,
    sweep_discharge: f32,

    pitch: f32,
    target: f32,
    gate: bool,
    vca: f32,
    meg: f32,
    accent_env: f32,
    accent_level: f32,
    sweep_cap: f32,
    meg_decay: f32,
    next: usize,
    current_off: usize,
    y4: f32,
}

impl Player {
    /// The voice a track's notes ask for, ready to run. `None` when the track
    /// has no notes and so no sound.
    pub fn new(def: &Tb303Def, notes: &[TimedNote], sweeps: &[Sweep], sample_rate: f32) -> Option<Player> {
        let knobs = Knobs {
            def: def.clone(),
            sweeps: sweeps.iter().filter_map(|s| Tb303Param::from_name(&s.param).map(|p| (p, s.clone()))).collect(),
        };
        let mut notes: Vec<&TimedNote> = notes.iter().collect();
        notes.sort_by(|a, b| a.start.total_cmp(&b.start));
        let first = notes.first()?;

        let sr = sample_rate;
        let osr = sr * OVERSAMPLE as f32;
        let start_sample = (first.start * sr as f64) as usize;
        let end_time = notes.iter().map(|n| n.start + n.duration).fold(0.0, f64::max) + 1.5;
        let total = (end_time * sr as f64) as usize - start_sample;

        let mut events: Vec<Event> = Vec::with_capacity(notes.len());
        for (i, n) in notes.iter().enumerate() {
            let on = (n.start * sr as f64) as usize;
            let next = notes.get(i + 1);
            // A slide holds the gate until the next note starts.
            let slides_on = n.slide && next.is_some_and(|m| m.start <= n.start + n.duration * 1.01 + 1e-6);
            let off = if slides_on { (next.unwrap().start * sr as f64) as usize + 1 } else { ((n.start + n.duration * def.gate as f64) * sr as f64) as usize };
            let legato = i > 0 && notes[i - 1].slide && events.last().is_some_and(|e: &Event| e.off > on);
            events.push(Event { on, off: off.max(on + 1), midi: n.midi + def.tune, accent: n.accent || n.velocity >= 0.94, legato });
        }

        let pitch = events[0].midi;
        Some(Player {
            knobs,
            sr,
            osr,
            start_sample,
            total,
            made: 0,
            loud: 0,
            square: def.square,
            osc: Oscillator::new(0.0),
            stages: [Stage::default(); 4],
            feedback_hp: OnePole::new(120.0, osr),
            decimate: [Biquad::lowpass(sr * 0.45, 0.54, osr), Biquad::lowpass(sr * 0.45, 1.31, osr)],
            dc_block: OnePole::new(15.0, sr),
            slide_coef: (-1.0 / (def.slide_time.max(0.005) * osr)).exp(),
            vca_attack: 1.0 / (0.003 * osr),
            vca_decay: (-1.0 / (2.5 * osr)).exp(),
            vca_release: (-1.0 / (0.008 * osr)).exp(),
            accent_decay: (-1.0 / (0.2 * osr)).exp(),
            sweep_charge: 1.0 - (-1.0 / (0.03 * osr)).exp(),
            sweep_discharge: (-1.0 / (0.5 * osr)).exp(),
            pitch,
            target: pitch,
            gate: false,
            vca: 0.0,
            meg: 0.0,
            accent_env: 0.0,
            accent_level: 0.0,
            sweep_cap: 0.0,
            meg_decay: 0.0,
            next: 0,
            current_off: 0,
            y4: 0.0,
            events,
        })
    }

    /// Where the track's first sample is.
    pub fn start_sample(&self) -> usize {
        self.start_sample
    }

    /// Whether the voice has run out: there is nothing left to make.
    pub fn finished(&self) -> bool {
        self.made >= self.total
    }

    /// How many samples the render keeps — the silent tail is not written.
    /// Only true once [`Player::finished`].
    pub fn kept(&self) -> usize {
        self.loud.max(1).min(self.made)
    }

    /// The voice up to frame `to`, counted from the start of the render, as a
    /// clip that begins where the last one ended. Empty once it has run out.
    pub fn render_to(&mut self, to: usize) -> StereoClip {
        let want = to.saturating_sub(self.start_sample + self.made).min(self.total - self.made);
        let offset = self.start_sample + self.made;
        let mut left = Vec::with_capacity(want);
        for _ in 0..want {
            left.push(self.step());
        }
        StereoClip { offset, right: left.clone(), left }
    }

    /// One sample of the voice, and the state it leaves behind for the next.
    fn step(&mut self) -> f32 {
        let sample = self.start_sample + self.made;
        let t = sample as f64 / self.sr as f64;
        while self.next < self.events.len() && self.events[self.next].on <= sample {
            let e = &self.events[self.next];
            self.target = e.midi;
            if !e.legato {
                self.pitch = e.midi;
                self.meg = 1.0;
                if e.accent {
                    self.accent_env = 1.0;
                }
            }
            self.accent_level = if e.accent { 1.0 } else { 0.0 };
            // Accented notes use the short decay, like the hardware.
            let decay_s = if e.accent { 0.2 } else { 0.2 + self.knobs.get(Tb303Param::Decay, t) * 1.8 };
            self.meg_decay = (-1.0 / (decay_s / 4.6 * self.osr)).exp();
            self.gate = true;
            self.current_off = e.off;
            self.next += 1;
        }
        if self.gate && sample >= self.current_off {
            self.gate = false;
        }

        let cutoff_knob = self.knobs.get(Tb303Param::Cutoff, t);
        let resonance = self.knobs.get(Tb303Param::Resonance, t);
        let env_mod = self.knobs.get(Tb303Param::EnvMod, t);
        let accent_amount = self.knobs.get(Tb303Param::Accent, t);
        let drive = self.knobs.get(Tb303Param::Drive, t);
        let k = resonance * 4.1;

        let mut out = 0.0f32;
        for _ in 0..OVERSAMPLE {
            self.pitch = self.target + (self.pitch - self.target) * self.slide_coef;
            let hz = midi_to_hz(self.pitch);
            let dt = (hz / self.osr).min(0.45);
            let raw = self.osc.next(if self.square { Waveform::Square } else { Waveform::Saw }, dt);
            // The 303 square is a shaped saw, not a symmetric pulse.
            let wave = if self.square { (raw * 1.4 + 0.1).tanh() } else { raw };

            self.meg *= self.meg_decay;
            self.accent_env *= self.accent_decay;
            let accent_drive = self.accent_env * self.accent_level * accent_amount;
            self.sweep_cap = if accent_drive > self.sweep_cap {
                self.sweep_cap + (accent_drive - self.sweep_cap) * self.sweep_charge
            } else {
                self.sweep_cap * self.sweep_discharge
            };

            self.vca = if self.gate {
                (self.vca + self.vca_attack).min(1.0) * if self.vca >= 1.0 { self.vca_decay } else { 1.0 }
            } else {
                self.vca * self.vca_release
            };

            let octaves = cutoff_knob * 4.3 + env_mod * 3.2 * self.meg + accent_amount * (1.2 * accent_drive + 3.0 * self.sweep_cap);
            let fc = (260.0 * 2f32.powf(octaves)).min(self.osr * 0.24);
            let g = (PI * fc / self.osr).tan();
            let gg = g / (1.0 + g);

            // Resonance loses bass on the real unit; the highpass in the loop mimics that.
            let fb = self.feedback_hp.highpass(self.y4);
            let x = ((wave * 0.9) - k * fb).tanh();
            let mut y = x;
            for st in &mut self.stages {
                y = st.process(y, gg);
            }
            self.y4 = y;
            let filtered = y * (1.0 + resonance * 0.8);

            let level = self.vca * (1.0 + self.accent_level * accent_amount * 0.6 * (0.4 + 0.6 * self.accent_env));
            let mut s = filtered * level;
            if drive > 0.0 {
                let d = 1.0 + drive * 12.0;
                s = (s * d).tanh() / d.tanh().max(1e-3) * (1.0 - drive * 0.35);
            }
            for f in &mut self.decimate {
                s = f.process(s);
            }
            out = s;
        }
        let s = self.dc_block.highpass(out) * VOICE_GAIN;
        self.made += 1;
        // Written the way the trim reads it: it stops at the first sample from
        // the end that is not under `SILENT`, and a NaN is not under it.
        if s.is_nan() || s.abs() >= SILENT {
            self.loud = self.made;
        }
        s
    }
}

/// The whole track at once, as an ordinary render asks for it.
pub fn render(def: &Tb303Def, notes: &[TimedNote], sweeps: &[Sweep], sample_rate: f32) -> StereoClip {
    let Some(mut player) = Player::new(def, notes, sweeps, sample_rate) else {
        return StereoClip { offset: 0, left: Vec::new(), right: Vec::new() };
    };
    let mut clip = player.render_to(usize::MAX);
    // Trim the silent tail.
    clip.left.truncate(player.kept());
    clip.right.truncate(player.kept());
    clip
}
