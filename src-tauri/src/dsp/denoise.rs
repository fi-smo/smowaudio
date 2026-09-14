//! DeepFilterNet3 neural noise suppression.

use df::tract::{DfParams, DfTract, ReduceMask, RuntimeParams};
use ndarray::{ArrayView2, ArrayViewMut2};
use serde::{Deserialize, Serialize};

/// DeepFilterNet3 runs at 48 kHz with a 10 ms hop.
pub const FRAME: usize = 480;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DenoiseSettings {
    pub enabled: bool,
    /// Maximum noise attenuation in dB. 100 = remove everything it can;
    /// lower values keep some room tone and sound more natural.
    pub strength_db: f32,
    /// Extra post-filter for residual noise between words (0 = off, ~0.02 = strong).
    pub post_filter: f32,
    /// Use the DeepFilterNet3 low-latency model: no lookahead (~20 ms less delay), slightly
    /// less clean, more CPU.
    pub low_latency: bool,
}

impl Default for DenoiseSettings {
    fn default() -> Self {
        Self { enabled: true, strength_db: 100.0, post_filter: 0.0, low_latency: false }
    }
}

pub struct Denoiser {
    model: DfTract,
    settings: DenoiseSettings,
    scratch: [f32; FRAME],
}

impl Denoiser {
    /// Loads the embedded model. Takes a noticeable moment, so call it off the audio path.
    pub fn new(settings: DenoiseSettings) -> anyhow::Result<Self> {
        let runtime = RuntimeParams::default_with_ch(1)
            .with_atten_lim(settings.strength_db)
            .with_thresholds(-15.0, 35.0, 35.0)
            .with_post_filter(settings.post_filter)
            .with_mask_reduce(ReduceMask::MAX);
        let params = if settings.low_latency {
            DfParams::from_bytes(include_bytes!("../../../vendor/DeepFilterNet/models/DeepFilterNet3_ll_onnx.tar.gz"))?
        } else {
            DfParams::default()
        };
        let model = DfTract::new(params, &runtime)?;
        anyhow::ensure!(model.hop_size == FRAME, "unexpected DeepFilterNet hop size {}", model.hop_size);
        Ok(Self { model, settings, scratch: [0.0; FRAME] })
    }

    /// Algorithmic delay: half the 20 ms analysis window plus the model's lookahead frames.
    pub fn latency_ms(&self) -> f32 {
        10.0 + self.model.lookahead as f32 * 10.0
    }

    pub fn set(&mut self, settings: DenoiseSettings) {
        if settings.strength_db != self.settings.strength_db {
            self.model.set_atten_lim(settings.strength_db);
        }
        if settings.post_filter != self.settings.post_filter {
            self.model.set_pf_beta(settings.post_filter);
        }
        self.settings = settings;
    }

    /// Denoises exactly one `FRAME` of mono audio in place.
    pub fn process(&mut self, frame: &mut [f32]) {
        if !self.settings.enabled || frame.len() != FRAME {
            return;
        }
        self.scratch.copy_from_slice(frame);
        let noisy = ArrayView2::from_shape((1, FRAME), &self.scratch[..]).expect("frame shape");
        let enhanced = ArrayViewMut2::from_shape((1, FRAME), frame).expect("frame shape");
        if self.model.process(noisy, enhanced).is_err() {
            frame.copy_from_slice(&self.scratch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_model_loads_and_suppresses_white_noise() {
        let mut denoiser = Denoiser::new(DenoiseSettings::default()).expect("model loads");
        let mut seed = 0x1234_5678u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed as f32 / u32::MAX as f32 - 0.5) * 0.2
        };

        let (mut energy_in, mut energy_out) = (0f64, 0f64);
        for i in 0..200 {
            let mut frame = [0f32; FRAME];
            frame.iter_mut().for_each(|s| *s = noise());
            let before: f64 = frame.iter().map(|s| (*s as f64).powi(2)).sum();
            denoiser.process(&mut frame);
            // Skip the first second while the model's recurrent state settles.
            if i >= 100 {
                energy_in += before;
                energy_out += frame.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
            }
        }
        let reduction_db = 10.0 * (energy_in / energy_out.max(1e-12)).log10();
        assert!(reduction_db > 20.0, "only {reduction_db:.1} dB of noise removed");
    }

    #[test]
    fn low_latency_model_has_less_delay_and_runs_in_real_time() {
        let standard = Denoiser::new(DenoiseSettings::default()).expect("standard model loads");
        let mut fast = Denoiser::new(DenoiseSettings { low_latency: true, ..Default::default() }).expect("low-latency model loads");
        println!("model delay: standard {} ms, low-latency {} ms", standard.latency_ms(), fast.latency_ms());
        assert!(fast.latency_ms() < standard.latency_ms());

        let mut frame = [0f32; FRAME];
        let start = std::time::Instant::now();
        for i in 0..300 {
            frame.iter_mut().enumerate().for_each(|(j, s)| *s = ((i * FRAME + j) as f32 * 0.37).sin() * 0.1);
            fast.process(&mut frame);
        }
        let per_frame_ms = start.elapsed().as_secs_f64() * 1000.0 / 300.0;
        println!("low-latency model: {per_frame_ms:.2} ms of CPU per 10 ms frame");
        assert!(per_frame_ms < 5.0, "too slow for real time: {per_frame_ms:.2} ms per frame");
    }
}
