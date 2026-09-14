//! Parametric equalizer built from RBJ cookbook biquads.

use serde::{Deserialize, Serialize};

use super::SAMPLE_RATE;

pub const MAX_BANDS: usize = 10;
pub const MAX_CHANNELS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandKind {
    LowShelf,
    Peaking,
    HighShelf,
    LowPass,
    HighPass,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub kind: BandKind,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
    pub enabled: bool,
}

impl EqBand {
    pub fn peaking(freq: f32) -> Self {
        Self { kind: BandKind::Peaking, freq, gain_db: 0.0, q: 1.0, enabled: true }
    }
}

/// A flat 5-band starting point: low shelf, three peaks, high shelf.
pub fn default_bands() -> Vec<EqBand> {
    vec![
        EqBand { kind: BandKind::LowShelf, freq: 100.0, gain_db: 0.0, q: 0.707, enabled: true },
        EqBand::peaking(400.0),
        EqBand::peaking(1500.0),
        EqBand::peaking(4000.0),
        EqBand { kind: BandKind::HighShelf, freq: 10000.0, gain_db: 0.0, q: 0.707, enabled: true },
    ]
}

#[derive(Debug, Clone, Copy, Default)]
struct Coefs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coefs {
    const IDENTITY: Coefs = Coefs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    fn design(band: &EqBand) -> Coefs {
        if !band.enabled {
            return Self::IDENTITY;
        }
        let fs = SAMPLE_RATE as f64;
        let freq = (band.freq as f64).clamp(10.0, fs * 0.49);
        let q = (band.q as f64).max(0.05);
        let a = 10f64.powf(band.gain_db as f64 / 40.0);
        let w0 = std::f64::consts::TAU * freq / fs;
        let (sin, cos) = (w0.sin(), w0.cos());
        let alpha = sin / (2.0 * q);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let (b0, b1, b2, a0, a1, a2) = match band.kind {
            BandKind::Peaking => {
                (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a)
            }
            BandKind::LowShelf => (
                a * ((a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha),
                (a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha,
            ),
            BandKind::HighShelf => (
                a * ((a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha),
                (a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha,
            ),
            BandKind::LowPass => {
                ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            BandKind::HighPass => {
                ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
        };
        Coefs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct State {
    z1: f64,
    z2: f64,
}

pub struct Equalizer {
    coefs: [Coefs; MAX_BANDS],
    states: [[State; MAX_BANDS]; MAX_CHANNELS],
    active: usize,
}

impl Equalizer {
    pub fn new(bands: &[EqBand]) -> Self {
        let mut eq = Self {
            coefs: [Coefs::IDENTITY; MAX_BANDS],
            states: [[State::default(); MAX_BANDS]; MAX_CHANNELS],
            active: 0,
        };
        eq.set_bands(bands);
        eq
    }

    /// Recomputes coefficients; filter state is kept so changes don't click.
    pub fn set_bands(&mut self, bands: &[EqBand]) {
        self.active = bands.len().min(MAX_BANDS);
        for (i, band) in bands.iter().take(MAX_BANDS).enumerate() {
            self.coefs[i] = Coefs::design(band);
        }
    }

    /// Processes interleaved audio in place.
    pub fn process(&mut self, buf: &mut [f32], channels: usize) {
        let channels = channels.min(MAX_CHANNELS);
        for frame in buf.chunks_exact_mut(channels) {
            for (ch, sample) in frame.iter_mut().enumerate() {
                let mut x = *sample as f64;
                for (c, s) in self.coefs[..self.active].iter().zip(&mut self.states[ch][..self.active]) {
                    let y = c.b0 * x + s.z1;
                    s.z1 = c.b1 * x - c.a1 * y + s.z2;
                    s.z2 = c.b2 * x - c.a2 * y;
                    x = y;
                }
                *sample = x as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_gain(bands: &[EqBand], freq: f32) -> f32 {
        let mut eq = Equalizer::new(bands);
        let mut buf: Vec<f32> =
            (0..48_000).map(|i| (std::f32::consts::TAU * freq * i as f32 / SAMPLE_RATE).sin()).collect();
        eq.process(&mut buf, 1);
        let peak = buf[24_000..].iter().fold(0f32, |m, s| m.max(s.abs()));
        super::super::lin_to_db(peak)
    }

    #[test]
    fn flat_bands_are_transparent() {
        assert!(sine_gain(&default_bands(), 1000.0).abs() < 0.05);
    }

    #[test]
    fn peaking_boost_hits_target_at_center() {
        let band = EqBand { gain_db: 6.0, ..EqBand::peaking(1000.0) };
        assert!((sine_gain(&[band], 1000.0) - 6.0).abs() < 0.1);
    }

    #[test]
    fn highpass_cuts_low_frequencies() {
        let band = EqBand { kind: BandKind::HighPass, freq: 1000.0, gain_db: 0.0, q: 0.707, enabled: true };
        assert!(sine_gain(&[band], 50.0) < -40.0);
    }
}
