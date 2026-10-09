//! Measures how long sound takes to get from an app to the output device, per channel.
//!
//! A few short 2 kHz beeps are played into a channel's cable (the way an app would), while two
//! recordings run: the cable's output side (what Smowaudio reads) and a loopback of the output
//! device (what Windows sends to the speakers). That splits the delay into:
//! - VB-Cable: from the beep entering the queue to the cable's output side;
//! - Smowaudio + Windows: from the cable's output side to the output device, timed between two
//!   recordings with capture timestamps, so it needs no clock estimate at all.
//!
//! Headphones and speakers add their own delay after this point (Bluetooth often 100-250 ms),
//! which Windows can't see.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::stream::{self, TimedRecording};
use super::{device, ComGuard};

const RATE: f64 = 48_000.0;
const TONE_HZ: f64 = 2_000.0;
const BEEPS: usize = 3;
const BEEP_EVERY: usize = 24_000; // 500 ms
const BEEP_LEN: usize = 480; // 10 ms
const BEEP_LEVEL: f32 = 0.12; // about −18 dBFS: clearly detectable, not loud
const FADE: usize = 48; // 1 ms fade in and out, so the beep doesn't click
/// Detection window: 5 ms of the tone.
const WINDOW: usize = 240;
/// Longest delay to look for between stages.
const MAX_STAGE_HNS: f64 = 0.45 * 1e7;

/// One channel's delay, split by stage, in ms.
#[derive(Debug, Clone, Copy)]
pub struct Delay {
    pub cable_ms: f64,
    pub engine_ms: f64,
}

/// The test beep: a sine with 1 ms fades so it doesn't click.
fn beep() -> Vec<f32> {
    let fade = FADE as f64;
    (0..BEEP_LEN)
        .map(|i| {
            let t = i as f64;
            let env = (t / fade).min((BEEP_LEN as f64 - t) / fade).min(1.0);
            ((std::f64::consts::TAU * TONE_HZ * t / RATE).sin() * env) as f32 * BEEP_LEVEL
        })
        .collect()
}

/// Level of the 2 kHz tone in each window starting at every sample (a sliding single-bin DFT).
/// Tuned to the test tone, so music or speech in the same channel barely registers.
fn tone_level(x: &[f32]) -> Vec<f32> {
    if x.len() < WINDOW {
        return Vec::new();
    }
    let w = std::f64::consts::TAU * TONE_HZ / RATE;
    let (mut c, mut s) = (vec![0f64; x.len() + 1], vec![0f64; x.len() + 1]);
    for (i, &v) in x.iter().enumerate() {
        c[i + 1] = c[i] + v as f64 * (w * i as f64).cos();
        s[i + 1] = s[i] + v as f64 * (w * i as f64).sin();
    }
    (0..=x.len() - WINDOW)
        .map(|i| {
            let (cc, ss) = (c[i + WINDOW] - c[i], s[i + WINDOW] - s[i]);
            ((cc * cc + ss * ss).sqrt() * 2.0 / WINDOW as f64) as f32
        })
        .collect()
}

/// Sample indices where a beep starts. The tone level rises over one window as the beep enters
/// it; the onset is where it reaches half its peak, moved back half a window.
fn beep_onsets(x: &[f32]) -> Vec<usize> {
    let level = tone_level(x);
    if level.is_empty() {
        return Vec::new();
    }
    let peak = level.iter().fold(0f32, |m, v| m.max(*v));
    let mut sorted = level.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2];
    let threshold = (peak * 0.35).max(median * 6.0).max(0.004);
    let mut onsets = Vec::new();
    let mut i = 0;
    while i < level.len() {
        if level[i] < threshold {
            i += 1;
            continue;
        }
        // This beep's region and its own peak.
        let start = i;
        let mut end = i;
        while end < level.len() && level[end] >= threshold * 0.5 {
            end += 1;
        }
        let local = level[start..end].iter().fold(0f32, |m, v| m.max(*v));
        let mut j = start;
        while j > 0 && level[j - 1] >= local * 0.5 {
            j -= 1;
        }
        while j < end && level[j] < local * 0.5 {
            j += 1;
        }
        // Half the tone is in the window when it starts half a window before the beep; the
        // 1 ms fade-in shifts that by half the fade.
        onsets.push(j + WINDOW / 2 - FADE / 2);
        // Skip the rest of this beep (and any echo) before looking for the next.
        i = end + (RATE * 0.1) as usize;
    }
    onsets
}

/// QPC times (100 ns units) of the beeps heard in a recording.
fn heard(rec: &TimedRecording) -> Vec<f64> {
    beep_onsets(&rec.samples).into_iter().map(|i| rec.time_of(i)).collect()
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(v[v.len() / 2])
}

/// Measures one channel: beeps go into `sink` (the cable's Input), and are listened for on
/// `source` (its Output, which Smowaudio records) and on `output` (loopback of the speakers).
/// Smowaudio must be running, since its engine carries the beeps from one to the other.
pub fn measure(sink: &str, source: &str, output: &str) -> Result<Delay> {
    let stop = Arc::new(AtomicBool::new(false));
    let recorder = |id: String, loopback: bool| {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _com = ComGuard::new();
            stream::record_timed(&device::by_id(&id)?, loopback, &stop)
        })
    };
    let cable = recorder(source.to_string(), false);
    let speakers = recorder(output.to_string(), true);
    // Give both recordings a moment to start before the first beep.
    std::thread::sleep(Duration::from_millis(250));
    let played = (|| {
        let _com = ComGuard::new();
        stream::render_beeps(&device::by_id(sink)?, BEEPS, BEEP_EVERY, &beep())
    })();
    std::thread::sleep(Duration::from_millis(450));
    stop.store(true, Ordering::Relaxed);
    let cable = cable.join().map_err(|_| anyhow::anyhow!("cable recording crashed"))?.context("recording the cable")?;
    let speakers = speakers.join().map_err(|_| anyhow::anyhow!("output recording crashed"))?.context("recording the output")?;
    let played = played.context("playing the test beeps")?;

    let at_cable = heard(&cable);
    let at_speakers = heard(&speakers);
    if at_cable.is_empty() {
        bail!("the test beeps never came out of the cable");
    }
    if at_speakers.is_empty() {
        bail!("the test beeps reached Smowaudio but not your output (is the channel or Master muted, or its volume at 0?)");
    }
    let (mut cable_ms, mut engine_ms) = (Vec::new(), Vec::new());
    for p in &played {
        let Some(&c) = at_cable.iter().find(|&&t| t >= *p - 0.005e7 && t <= *p + MAX_STAGE_HNS) else { continue };
        let Some(&o) = at_speakers.iter().find(|&&t| t >= c && t <= c + MAX_STAGE_HNS) else { continue };
        cable_ms.push(((c - p) / 1e4).max(0.0));
        engine_ms.push((o - c) / 1e4);
    }
    match (median(cable_ms), median(engine_ms)) {
        (Some(cable_ms), Some(engine_ms)) => Ok(Delay { cable_ms, engine_ms }),
        _ => bail!("couldn't match the beeps between the cable and the output; try again in a quieter moment"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A beep at a known place in noise and a louder off-frequency tone is found to within 2 ms.
    #[test]
    fn finds_beeps_among_other_sound() {
        let b = beep();
        let mut x = vec![0f32; 48_000];
        let mut seed = 1u32;
        for (i, v) in x.iter_mut().enumerate() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5;
            // "Music": a louder 440 Hz tone plus noise.
            *v = 0.3 * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin() + 0.05 * noise;
        }
        for start in [5_000usize, 29_000] {
            for (i, s) in b.iter().enumerate() {
                x[start + i] += s * 0.5; // quieter than it was played, as after a volume fader
            }
        }
        let onsets = beep_onsets(&x);
        assert_eq!(onsets.len(), 2, "onsets: {onsets:?}");
        for (found, expected) in onsets.iter().zip([5_000usize, 29_000]) {
            assert!((*found as i64 - expected as i64).abs() <= 96, "found {found}, expected {expected}");
        }
    }

    #[test]
    fn silence_has_no_beeps() {
        assert!(beep_onsets(&vec![0f32; 24_000]).is_empty());
    }

    /// Real measurement through this PC's cables and the running Smowaudio, every channel.
    #[test]
    #[ignore = "plays beeps through the real channels; Smowaudio must be running"]
    fn measures_every_channel() {
        let _com = ComGuard::new();
        let config = crate::config::Config::load();
        let output = device::resolve_physical(device::Flow::Render, config.output_device.as_deref(), config.previous_default(device::Flow::Render).as_deref())
            .and_then(|d| device::device_id(&d))
            .expect("an output device");
        for (name, ch) in crate::config::CHANNEL_NAMES.iter().zip(&config.channels) {
            let (Some(sink), Some(source)) = (ch.sink.as_deref(), ch.source.as_deref()) else { continue };
            match measure(sink, source, &output) {
                Ok(d) => println!("{name}: VB-Cable {:.1} ms + Smowaudio/Windows {:.1} ms = {:.1} ms", d.cable_ms, d.engine_ms, d.cable_ms + d.engine_ms),
                Err(e) => println!("{name}: {e:#}"),
            }
        }
    }
}
