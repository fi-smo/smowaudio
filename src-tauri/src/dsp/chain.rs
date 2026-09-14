//! Processing chains for the virtual mic and the output channels.

use serde::{Deserialize, Serialize};

use super::biquad::{default_bands, EqBand, Equalizer};
use super::compressor::{Compressor, CompressorSettings};
use super::denoise::{DenoiseSettings, Denoiser, FRAME};
use super::gate::{GateSettings, NoiseGate};
use super::limiter::Limiter;
use super::{db_to_lin, lin_to_db};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqSettings {
    pub enabled: bool,
    /// Preset the bands came from ("Custom" once edited by hand). Shown in the UI only.
    pub preset: String,
    pub bands: Vec<EqBand>,
}

impl Default for EqSettings {
    fn default() -> Self {
        Self { enabled: false, preset: "Flat".into(), bands: default_bands() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MicSettings {
    pub denoise: DenoiseSettings,
    pub gate: GateSettings,
    pub eq: EqSettings,
    pub compressor: CompressorSettings,
    pub gain_db: f32,
    pub muted: bool,
    /// Play the processed mic in your own headphones ("listen to my mic").
    pub monitor: bool,
}

impl Default for MicSettings {
    fn default() -> Self {
        Self {
            denoise: DenoiseSettings::default(),
            gate: GateSettings::default(),
            eq: EqSettings::default(),
            compressor: CompressorSettings::default(),
            gain_db: 0.0,
            muted: false,
            monitor: false,
        }
    }
}

/// Live readings for the UI.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct MicMeters {
    pub input_db: f32,
    pub output_db: f32,
    pub gain_reduction_db: f32,
    pub gate_open: bool,
    pub denoise_ready: bool,
    /// Delay added by the noise removal model, 0 when not loaded.
    pub denoise_latency_ms: f32,
    /// How much noise removal is taking out right now, in dB (smoothed).
    pub noise_reduction_db: f32,
    /// Mic limiter gain reduction (dB, <= 0).
    pub limiter_db: f32,
}

/// Noise removal -> gate -> EQ -> compressor -> output gain.
///
/// Denoising runs first so the gate sees a clean signal and can use a low threshold
/// without cutting off quiet syllables.
pub struct MicChain {
    settings: MicSettings,
    denoiser: Option<Denoiser>,
    gate: NoiseGate,
    eq: Equalizer,
    compressor: Compressor,
    limiter: Limiter,
    pub meters: MicMeters,
}

impl MicChain {
    pub fn new(settings: MicSettings) -> Self {
        Self {
            denoiser: None,
            gate: NoiseGate::new(settings.gate),
            eq: Equalizer::new(&settings.eq.bands),
            compressor: Compressor::new(settings.compressor),
            limiter: Limiter::new(-1.0, 80.0),
            meters: MicMeters::default(),
            settings,
        }
    }

    pub fn attach_denoiser(&mut self, mut denoiser: Denoiser) {
        denoiser.set(self.settings.denoise);
        self.meters.denoise_latency_ms = denoiser.latency_ms();
        self.denoiser = Some(denoiser);
        self.meters.denoise_ready = true;
    }

    pub fn set(&mut self, settings: MicSettings) {
        if settings.denoise.low_latency != self.settings.denoise.low_latency && self.denoiser.is_some() {
            // Switching models means loading a new network; the mic pauses briefly while it loads.
            self.denoiser = None;
            self.meters.denoise_ready = false;
            match Denoiser::new(settings.denoise) {
                Ok(d) => {
                    self.meters.denoise_latency_ms = d.latency_ms();
                    self.denoiser = Some(d);
                    self.meters.denoise_ready = true;
                }
                Err(e) => log::error!("switching noise removal model: {e:#}"),
            }
        } else if let Some(d) = self.denoiser.as_mut() {
            d.set(settings.denoise);
        }
        self.gate.set(settings.gate);
        if settings.eq.bands != self.settings.eq.bands {
            self.eq.set_bands(&settings.eq.bands);
        }
        self.compressor.set(settings.compressor);
        self.settings = settings;
    }

    /// Processes one 10 ms mono frame in place.
    pub fn process(&mut self, frame: &mut [f32; FRAME]) {
        self.meters.input_db = peak_db(frame);

        let before = rms_db(frame);
        match self.denoiser.as_mut() {
            Some(d) if self.settings.denoise.enabled => {
                d.process(frame);
                let removed = (before - rms_db(frame)).max(0.0);
                self.meters.noise_reduction_db += (removed - self.meters.noise_reduction_db) * 0.2;
            }
            _ => self.meters.noise_reduction_db = 0.0,
        }
        self.gate.process(frame);
        if self.settings.eq.enabled {
            self.eq.process(frame, 1);
        }
        self.compressor.process(frame);

        let gain = if self.settings.muted { 0.0 } else { db_to_lin(self.settings.gain_db) };
        for s in frame.iter_mut() {
            *s *= gain;
        }
        self.limiter.process(frame, 1);

        self.meters.output_db = peak_db(frame);
        self.meters.limiter_db = self.limiter.gain_reduction_db();
        self.meters.gain_reduction_db = self.compressor.gain_reduction_db();
        self.meters.gate_open = !self.settings.gate.enabled || self.gate.is_open();
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelSettings {
    pub volume: f32,
    pub muted: bool,
    pub eq: EqSettings,
}

impl Default for ChannelSettings {
    fn default() -> Self {
        Self { volume: 1.0, muted: false, eq: EqSettings::default() }
    }
}

/// Per-output-channel processing: EQ and volume on interleaved stereo.
pub struct ChannelChain {
    settings: ChannelSettings,
    eq: Equalizer,
    current_gain: f32,
    /// Left and right peak of the last block, in dBFS.
    pub peak_db: [f32; 2],
}

impl ChannelChain {
    pub fn new(settings: ChannelSettings) -> Self {
        Self { eq: Equalizer::new(&settings.eq.bands), current_gain: 0.0, peak_db: [-120.0; 2], settings }
    }

    pub fn set(&mut self, settings: ChannelSettings) {
        if settings.eq.bands != self.settings.eq.bands {
            self.eq.set_bands(&settings.eq.bands);
        }
        self.settings = settings;
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if self.settings.eq.enabled {
            self.eq.process(buf, 2);
        }
        let target = if self.settings.muted { 0.0 } else { self.settings.volume.clamp(0.0, 2.0) };
        // Ramp volume across the block to avoid zipper noise when sliders move.
        let step = (target - self.current_gain) / (buf.len().max(1) as f32);
        for s in buf.iter_mut() {
            self.current_gain += step;
            *s *= self.current_gain;
        }
        self.current_gain = target;
        self.peak_db = stereo_peak_db(buf);
    }
}

fn peak_db(buf: &[f32]) -> f32 {
    lin_to_db(buf.iter().fold(0f32, |m, s| m.max(s.abs())))
}

fn rms_db(buf: &[f32]) -> f32 {
    lin_to_db((buf.iter().map(|s| s * s).sum::<f32>() / buf.len().max(1) as f32).sqrt())
}

/// Left and right peak of interleaved stereo, in dBFS.
pub fn stereo_peak_db(buf: &[f32]) -> [f32; 2] {
    let (mut left, mut right) = (0f32, 0f32);
    for frame in buf.chunks_exact(2) {
        left = left.max(frame[0].abs());
        right = right.max(frame[1].abs());
    }
    [lin_to_db(left), lin_to_db(right)]
}
