//! Real-time DSP building blocks. Everything here processes 48 kHz f32 audio and
//! never allocates inside `process`.

pub mod biquad;
pub mod chain;
pub mod compressor;
pub mod denoise;
pub mod gate;
pub mod limiter;

pub const SAMPLE_RATE: f32 = 48_000.0;

#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
pub fn lin_to_db(lin: f32) -> f32 {
    20.0 * lin.max(1e-9).log10()
}

/// One-pole smoothing coefficient for a time constant in milliseconds.
#[inline]
pub fn time_coef(ms: f32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * SAMPLE_RATE)).exp()
    }
}
