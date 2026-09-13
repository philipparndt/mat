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

use std::f32::consts::PI;

use crate::arrange::{Sweep, TimedNote};
use crate::dsp::StereoClip;
use crate::dsp::biquad::Biquad;
use crate::dsp::filter::OnePole;
use crate::dsp::osc::Oscillator;
use crate::dsp::midi_to_hz;
use crate::model::{Tb303Def, Tb303Param, Waveform};

const OVERSAMPLE: usize = 2;
const VOICE_GAIN: f32 = 0.5;

struct Knobs<'a> {
    def: &'a Tb303Def,
    sweeps: Vec<(Tb303Param, &'a Sweep)>,
}

impl Knobs<'_> {
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

pub fn render(def: &Tb303Def, notes: &[TimedNote], sweeps: &[Sweep], sample_rate: f32) -> StereoClip {
    let knobs = Knobs { def, sweeps: sweeps.iter().filter_map(|s| Tb303Param::from_name(&s.param).map(|p| (p, s))).collect() };
    let mut notes: Vec<&TimedNote> = notes.iter().collect();
    notes.sort_by(|a, b| a.start.total_cmp(&b.start));
    let Some(first) = notes.first() else { return StereoClip { offset: 0, left: Vec::new(), right: Vec::new() } };

    let sr = sample_rate;
    let osr = sr * OVERSAMPLE as f32;
    let start_sample = (first.start * sr as f64) as usize;
    let end_time = notes.iter().map(|n| n.start + n.duration).fold(0.0, f64::max) + 1.5;
    let total = (end_time * sr as f64) as usize - start_sample;

    // Per note: trigger sample, gate-off sample, and whether it is slid into.
    struct Event {
        on: usize,
        off: usize,
        midi: f32,
        accent: bool,
        legato: bool,
    }
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

    let mut osc = Oscillator::new(0.0);
    let mut stages = [Stage::default(); 4];
    let mut feedback_hp = OnePole::new(120.0, osr);
    let mut decimate = [Biquad::lowpass(sr * 0.45, 0.54, osr), Biquad::lowpass(sr * 0.45, 1.31, osr)];
    let mut dc_block = OnePole::new(15.0, sr);

    let slide_coef = (-1.0 / (def.slide_time.max(0.005) * osr)).exp();
    let vca_attack = 1.0 / (0.003 * osr);
    let vca_decay = (-1.0 / (2.5 * osr)).exp();
    let vca_release = (-1.0 / (0.008 * osr)).exp();
    let accent_decay = (-1.0 / (0.2 * osr)).exp();
    let sweep_charge = 1.0 - (-1.0 / (0.03 * osr)).exp();
    let sweep_discharge = (-1.0 / (0.5 * osr)).exp();

    let mut pitch = events[0].midi;
    let mut target = pitch;
    let mut gate = false;
    let mut vca = 0.0f32;
    let mut meg = 0.0f32; // main filter envelope
    let mut accent_env = 0.0f32;
    let mut accent_level = 0.0f32;
    let mut sweep_cap = 0.0f32;
    let mut meg_decay = 0.0f32;
    let mut next = 0usize;
    let mut current_off = 0usize;
    let mut y4 = 0.0f32;

    let mut left = Vec::with_capacity(total);
    for i in 0..total {
        let sample = start_sample + i;
        let t = sample as f64 / sr as f64;
        while next < events.len() && events[next].on <= sample {
            let e = &events[next];
            target = e.midi;
            if !e.legato {
                pitch = e.midi;
                meg = 1.0;
                if e.accent {
                    accent_env = 1.0;
                }
            }
            accent_level = if e.accent { 1.0 } else { 0.0 };
            // Accented notes use the short decay, like the hardware.
            let decay_s = if e.accent { 0.2 } else { 0.2 + knobs.get(Tb303Param::Decay, t) * 1.8 };
            meg_decay = (-1.0 / (decay_s / 4.6 * osr)).exp();
            gate = true;
            current_off = e.off;
            next += 1;
        }
        if gate && sample >= current_off {
            gate = false;
        }

        let cutoff_knob = knobs.get(Tb303Param::Cutoff, t);
        let resonance = knobs.get(Tb303Param::Resonance, t);
        let env_mod = knobs.get(Tb303Param::EnvMod, t);
        let accent_amount = knobs.get(Tb303Param::Accent, t);
        let drive = knobs.get(Tb303Param::Drive, t);
        let k = resonance * 4.1;

        let mut out = 0.0f32;
        for _ in 0..OVERSAMPLE {
            pitch = target + (pitch - target) * slide_coef;
            let hz = midi_to_hz(pitch);
            let dt = (hz / osr).min(0.45);
            let raw = osc.next(if def.square { Waveform::Square } else { Waveform::Saw }, dt);
            // The 303 square is a shaped saw, not a symmetric pulse.
            let wave = if def.square { (raw * 1.4 + 0.1).tanh() } else { raw };

            meg *= meg_decay;
            accent_env *= accent_decay;
            let accent_drive = accent_env * accent_level * accent_amount;
            sweep_cap = if accent_drive > sweep_cap { sweep_cap + (accent_drive - sweep_cap) * sweep_charge } else { sweep_cap * sweep_discharge };

            vca = if gate { (vca + vca_attack).min(1.0) * if vca >= 1.0 { vca_decay } else { 1.0 } } else { vca * vca_release };

            let octaves = cutoff_knob * 4.3 + env_mod * 3.2 * meg + accent_amount * (1.2 * accent_drive + 3.0 * sweep_cap);
            let fc = (260.0 * 2f32.powf(octaves)).min(osr * 0.24);
            let g = (PI * fc / osr).tan();
            let gg = g / (1.0 + g);

            // Resonance loses bass on the real unit; the highpass in the loop mimics that.
            let fb = feedback_hp.highpass(y4);
            let x = ((wave * 0.9) - k * fb).tanh();
            let mut y = x;
            for st in &mut stages {
                y = st.process(y, gg);
            }
            y4 = y;
            let filtered = y * (1.0 + resonance * 0.8);

            let level = vca * (1.0 + accent_level * accent_amount * 0.6 * (0.4 + 0.6 * accent_env));
            let mut s = filtered * level;
            if drive > 0.0 {
                let d = 1.0 + drive * 12.0;
                s = (s * d).tanh() / d.tanh().max(1e-3) * (1.0 - drive * 0.35);
            }
            for f in &mut decimate {
                s = f.process(s);
            }
            out = s;
        }
        let s = dc_block.highpass(out) * VOICE_GAIN;
        left.push(s);
    }

    // Trim the silent tail.
    while left.len() > 1 && left.last().is_some_and(|s| s.abs() < 1e-5) {
        left.pop();
    }
    StereoClip { offset: start_sample, right: left.clone(), left }
}
