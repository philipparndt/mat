//! Plate reverb after Jon Dattorro, "Effect Design Part 1" (J. AES, 1997),
//! with a modulated tank to avoid metallic ringing.

use std::f32::consts::TAU;

use super::filter::OnePole;
use crate::model::ReverbSettings;

const REF_RATE: f32 = 29761.0;

struct DelayLine {
    buf: Vec<f32>,
    pos: usize,
}

impl DelayLine {
    fn new(max_delay: usize) -> Self {
        Self { buf: vec![0.0; max_delay + 2], pos: 0 }
    }

    /// Sample written `delay` samples ago (1 = last written).
    #[inline]
    fn tap(&self, delay: usize) -> f32 {
        let n = self.buf.len();
        self.buf[(self.pos + n + 1 - delay.clamp(1, n)) % n]
    }

    #[inline]
    fn tap_frac(&self, delay: f32) -> f32 {
        let d = delay.floor() as usize;
        let frac = delay - d as f32;
        let a = self.tap(d.max(1));
        let b = self.tap(d + 1);
        a + (b - a) * frac
    }

    #[inline]
    fn write(&mut self, x: f32) {
        self.pos = (self.pos + 1) % self.buf.len();
        self.buf[self.pos] = x;
    }
}

struct Allpass {
    line: DelayLine,
    len: f32,
}

impl Allpass {
    fn new(len: f32, extra: usize) -> Self {
        Self { line: DelayLine::new(len.ceil() as usize + extra), len }
    }

    #[inline]
    fn process(&mut self, x: f32, g: f32, modulation: f32) -> f32 {
        let z = self.line.tap_frac(self.len + modulation);
        let w = x + g * z;
        self.line.write(w);
        z - g * w
    }
}

pub struct PlateReverb {
    predelay: DelayLine,
    predelay_samples: usize,
    input_hp: OnePole,
    bandwidth: OnePole,
    diffusers: [Allpass; 4],
    // Left and right tank halves.
    ap_mod: [Allpass; 2],
    delay_a: [DelayLine; 2],
    ap: [Allpass; 2],
    delay_b: [DelayLine; 2],
    damp: [f32; 2],
    len_a: [usize; 2],
    len_b: [usize; 2],
    decay: f32,
    damping: f32,
    lfo_phase: f32,
    lfo_step: f32,
    excursion: f32,
    scale: f32,
}

impl PlateReverb {
    pub fn new(settings: &ReverbSettings, sample_rate: f32) -> Self {
        let scale = sample_rate / REF_RATE * (0.5 + 0.5 * settings.size);
        let s = |n: f32| n * scale;
        let excursion = 16.0 * sample_rate / REF_RATE;
        let extra = excursion.ceil() as usize + 2;
        let len_a = [s(4453.0) as usize, s(4217.0) as usize];
        let len_b = [s(3720.0) as usize, s(3163.0) as usize];
        let predelay_samples = (settings.predelay_ms / 1000.0 * sample_rate) as usize;
        Self {
            predelay: DelayLine::new(predelay_samples.max(1)),
            predelay_samples,
            input_hp: OnePole::new(180.0, sample_rate),
            bandwidth: OnePole::new(9000.0, sample_rate),
            diffusers: [
                Allpass::new(s(142.0), 0),
                Allpass::new(s(107.0), 0),
                Allpass::new(s(379.0), 0),
                Allpass::new(s(277.0), 0),
            ],
            ap_mod: [Allpass::new(s(672.0), extra), Allpass::new(s(908.0), extra)],
            delay_a: [DelayLine::new(len_a[0]), DelayLine::new(len_a[1])],
            ap: [Allpass::new(s(1800.0), 0), Allpass::new(s(2656.0), 0)],
            delay_b: [DelayLine::new(len_b[0]), DelayLine::new(len_b[1])],
            damp: [0.0; 2],
            len_a,
            len_b,
            decay: 0.2 + settings.decay.clamp(0.0, 1.0) * 0.77,
            damping: settings.damping.clamp(0.0, 1.0) * 0.8,
            lfo_phase: 0.0,
            lfo_step: 0.9 / sample_rate,
            excursion,
            scale,
        }
    }

    /// Processes a buffer. Input is summed to mono, output is stereo wet signal.
    pub fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let decay_diffusion2 = (self.decay + 0.15).clamp(0.25, 0.5);
        let t = |n: f32, sc: f32| (n * sc) as usize;
        let sc = self.scale;
        for i in 0..out_l.len() {
            let x = 0.5 * (in_l.get(i).copied().unwrap_or(0.0) + in_r.get(i).copied().unwrap_or(0.0));
            let x = self.input_hp.highpass(x);
            let x = if self.predelay_samples > 0 {
                let d = self.predelay.tap(self.predelay_samples);
                self.predelay.write(x);
                d
            } else {
                x
            };
            let mut x = self.bandwidth.lowpass(x);
            x = self.diffusers[0].process(x, 0.75, 0.0);
            x = self.diffusers[1].process(x, 0.75, 0.0);
            x = self.diffusers[2].process(x, 0.625, 0.0);
            x = self.diffusers[3].process(x, 0.625, 0.0);

            self.lfo_phase = (self.lfo_phase + self.lfo_step) % 1.0;
            let lfo = [(self.lfo_phase * TAU).sin(), (self.lfo_phase * TAU).cos()];

            let feedback = [self.delay_b[1].tap(self.len_b[1]), self.delay_b[0].tap(self.len_b[0])];
            for side in 0..2 {
                let mut v = x + feedback[side] * self.decay;
                v = self.ap_mod[side].process(v, -0.7, lfo[side] * self.excursion);
                let delayed = self.delay_a[side].tap(self.len_a[side]);
                self.delay_a[side].write(v);
                self.damp[side] = delayed * (1.0 - self.damping) + self.damp[side] * self.damping;
                let mut w = self.damp[side] * self.decay;
                w = self.ap[side].process(w, decay_diffusion2, 0.0);
                self.delay_b[side].write(w);
            }

            let (da, db) = (&self.delay_a, &self.delay_b);
            let (apl, apr) = (&self.ap[0].line, &self.ap[1].line);
            let l = da[1].tap(t(266.0, sc)) + da[1].tap(t(2974.0, sc)) - apr.tap(t(1913.0, sc)) + db[1].tap(t(1996.0, sc))
                - da[0].tap(t(1990.0, sc))
                - apl.tap(t(187.0, sc))
                - db[0].tap(t(1066.0, sc));
            let r = da[0].tap(t(353.0, sc)) + da[0].tap(t(3627.0, sc)) - apl.tap(t(1228.0, sc)) + db[0].tap(t(2673.0, sc))
                - da[1].tap(t(2111.0, sc))
                - apr.tap(t(335.0, sc))
                - db[1].tap(t(121.0, sc));
            out_l[i] = l * 0.6;
            out_r[i] = r * 0.6;
        }
    }
}
