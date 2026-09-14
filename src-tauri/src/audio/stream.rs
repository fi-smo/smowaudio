//! Event-driven shared-mode WASAPI streams, always 48 kHz float.
//! AUTOCONVERTPCM lets Windows adapt to whatever the device mix format is.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{bail, Result};
use windows::core::{w, Interface, GUID};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    IAudioCaptureClient, IAudioClient, IAudioClient3, IAudioRenderClient, IMMDevice,
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, WaitForSingleObject,
};

const SAMPLE_RATE: u32 = 48_000;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// KSDATAFORMAT_SUBTYPE_IEEE_FLOAT (ksmedia.h).
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
/// Render: 20 ms keeps output latency low; WASAPI wakes us every ~10 ms to top it up.
const RENDER_BUFFER_HNS: i64 = 200_000;
/// Capture: some devices (VB-Audio cables) wake us only every ~30 ms. A buffer smaller than
/// that overflows and silently drops audio. Capture is drained on every wake, so this adds no latency.
const CAPTURE_BUFFER_HNS: i64 = 1_000_000;

thread_local! {
    static PHASE_HOOK: std::cell::RefCell<Option<Box<dyn Fn(&'static str)>>> = const { std::cell::RefCell::new(None) };
}

/// Lets the owner of this thread observe which step a stream is in, so a watchdog can report a
/// thread stuck inside a Windows or driver call (those never return an error to retry on).
pub fn set_phase_hook(hook: impl Fn(&'static str) + 'static) {
    PHASE_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

/// Records the current step for this thread's phase hook, if any.
pub fn phase(name: &'static str) {
    PHASE_HOOK.with(|h| {
        if let Some(hook) = h.borrow().as_ref() {
            hook(name);
        }
    });
}

/// Raises the current thread to the MMCSS "Pro Audio" class while alive.
pub struct ProAudioThread(HANDLE);

impl ProAudioThread {
    pub fn new() -> Self {
        let mut task_index = 0u32;
        let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) }.unwrap_or_default();
        ProAudioThread(handle)
    }
}

impl Drop for ProAudioThread {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
        }
    }
}

struct Opened {
    client: IAudioClient,
    event: HANDLE,
    channels: usize,
    /// Engine period when opened in low-latency mode.
    period_frames: Option<u32>,
}

impl Drop for Opened {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

fn open(device: &IMMDevice, channels: u16, buffer_hns: i64) -> Result<Opened> {
    unsafe {
        phase("activate");
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let block_align = channels * 4;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
            nChannels: channels,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * block_align as u32,
            nBlockAlign: block_align,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        phase("initialize");
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            buffer_hns,
            0,
            &format,
            None,
        )?;
        phase("set event");
        let event = CreateEventW(None, false, false, None)?;
        let opened = Opened { client, event, channels: channels as usize, period_frames: None };
        opened.client.SetEventHandle(opened.event)?;
        Ok(opened)
    }
}

fn wait(event: HANDLE) -> Result<bool> {
    match unsafe { WaitForSingleObject(event, 500) } {
        WAIT_OBJECT_0 => Ok(true),
        // A stalled device (e.g. cable nobody plays into) still lets us check the stop flag.
        _ => Ok(false),
    }
}

/// Captures until `stop` is set, calling `on_data` with interleaved samples and whether
/// Windows reported lost audio (a glitch) just before this packet.
/// Returns Err when the device disappears so the caller can reconnect.
pub fn run_capture(
    device: &IMMDevice,
    channels: u16,
    stop: &AtomicBool,
    mut on_data: impl FnMut(&[f32], bool),
) -> Result<()> {
    let s = open(device, channels, CAPTURE_BUFFER_HNS)?;
    phase("get capture service");
    let capture: IAudioCaptureClient = unsafe { s.client.GetService()? };
    let mut silence = vec![0f32; 48_000 * s.channels];
    phase("start");
    unsafe { s.client.Start()? };
    phase("mmcss");
    let _mmcss = ProAudioThread::new();

    while !stop.load(Ordering::Relaxed) {
        phase("wait");
        wait(s.event)?;
        phase("read");
        loop {
            let packet = unsafe { capture.GetNextPacketSize()? };
            if packet == 0 {
                break;
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)? };
            let len = frames as usize * s.channels;
            let glitch = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                if silence.len() < len {
                    silence.resize(len, 0.0);
                }
                on_data(&silence[..len], glitch);
            } else {
                on_data(unsafe { std::slice::from_raw_parts(data as *const f32, len) }, glitch);
            }
            unsafe { capture.ReleaseBuffer(frames)? };
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::device::{self, Flow};
    use crate::audio::ComGuard;
    use std::sync::Arc;
    use std::time::Duration;

    /// Read-only diagnostic: native mix format, device periods and the latency Windows reports
    /// for streams opened the way the engine opens them.
    #[test]
    #[ignore = "diagnostic for this PC's audio devices; run with --ignored"]
    fn print_device_latency_info() {
        use windows::core::Interface;
        use windows::Win32::Media::Audio::IAudioClient3;
        use windows::Win32::System::Com::CoTaskMemFree;
        let _com = ComGuard::new();
        let hns_ms = |hns: i64| hns as f64 / 10_000.0;
        for flow in [Flow::Render, Flow::Capture] {
            for info in device::list(flow).unwrap() {
                if info.hardware.contains("SteelSeries") || info.hardware.contains("Elgato") {
                    continue;
                }
                let dev = device::by_id(&info.id).unwrap();
                let probe: IAudioClient = unsafe { dev.Activate(CLSCTX_ALL, None).unwrap() };
                let (rate, ch, bits, min_period_frames) = unsafe {
                    let mix = probe.GetMixFormat().unwrap();
                    let f = *mix;
                    let mut min_frames = 0u32;
                    if let Ok(c3) = probe.cast::<IAudioClient3>() {
                        let (mut d, mut fu, mut mx) = (0u32, 0u32, 0u32);
                        let _ = c3.GetSharedModeEnginePeriod(mix, &mut d, &mut fu, &mut min_frames, &mut mx);
                    }
                    CoTaskMemFree(Some(mix as *const _));
                    (f.nSamplesPerSec, f.nChannels, f.wBitsPerSample, min_frames)
                };
                let (mut default_period, mut min_period) = (0i64, 0i64);
                unsafe { probe.GetDevicePeriod(Some(&mut default_period), Some(&mut min_period)).unwrap() };

                let (buffer_hns, channels) = match flow {
                    Flow::Render => (RENDER_BUFFER_HNS, 2),
                    Flow::Capture => (CAPTURE_BUFFER_HNS, if info.is_virtual() { 2 } else { 1 }),
                };
                let stream = match open(&dev, channels, buffer_hns) {
                    Ok(s) => s,
                    Err(e) => {
                        println!("{:?} {}: open failed: {e}", flow, info.name);
                        continue;
                    }
                };
                let latency = unsafe { stream.client.GetStreamLatency().unwrap_or(0) };
                let buffer = unsafe { stream.client.GetBufferSize().unwrap_or(0) };
                println!(
                    "{:?} | {} | mix {} Hz {}ch {}bit | period default {:.1} ms, min {:.1} ms, low-latency min {} frames | stream latency {:.1} ms, buffer {} frames ({:.1} ms)",
                    flow, info.name, rate, ch, bits, hns_ms(default_period), hns_ms(min_period), min_period_frames,
                    hns_ms(latency), buffer, buffer as f64 / 48.0
                );
            }
        }
    }

    /// End-to-end through a real virtual cable: render a tone into "CABLE-D Input" and
    /// check it arrives intact on "CABLE-D Output". Silent for the user.
    #[test]
    #[ignore = "needs VB-Audio Cable D installed; run with --ignored"]
    fn tone_passes_through_virtual_cable() {
        let _com = ComGuard::new();
        let find = |flow| {
            device::list(flow).unwrap().into_iter().find(|d| d.hardware == "VB-Audio Cable D").expect("Cable D").id
        };
        let (render_id, capture_id) = (find(Flow::Render), find(Flow::Capture));
        let stop = Arc::new(AtomicBool::new(false));

        let render_stop = stop.clone();
        let renderer = std::thread::spawn(move || {
            let _com = ComGuard::new();
            let device = device::by_id(&render_id).unwrap();
            let mut phase = 0f32;
            run_render_while(&device, 2, || !render_stop.load(Ordering::Relaxed), None, |buf| {
                for frame in buf.chunks_exact_mut(2) {
                    let s = (phase * std::f32::consts::TAU).sin() * 0.5;
                    phase = (phase + 1000.0 / 48_000.0).fract();
                    frame.fill(s);
                }
            })
            .unwrap();
        });

        let capture_stop = stop.clone();
        let capturer = std::thread::spawn(move || {
            let _com = ComGuard::new();
            let device = device::by_id(&capture_id).unwrap();
            let mut captured = Vec::new();
            run_capture(&device, 2, &capture_stop, |s, _| captured.extend_from_slice(s)).unwrap();
            captured
        });

        std::thread::sleep(Duration::from_millis(1500));
        stop.store(true, Ordering::Relaxed);
        renderer.join().unwrap();
        let captured = capturer.join().unwrap();

        let tail = &captured[captured.len().saturating_sub(24_000)..];
        let peak = tail.iter().fold(0f32, |m, s| m.max(s.abs()));
        println!("captured {} samples, peak {peak:.3}", captured.len());
        assert!(captured.len() > 48_000, "too little audio captured");
        assert!((peak - 0.5).abs() < 0.05, "tone peak {peak}, expected 0.5");
    }
}

/// Test-only tools for measuring real latency through audio devices using QPC timestamps.
#[cfg(test)]
pub mod probe {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{anyhow, Result};
    use windows::Win32::Media::Audio::{IAudioCaptureClient, IAudioClock, IAudioRenderClient, IMMDevice, AUDCLNT_BUFFERFLAGS_SILENT};

    use super::{open, wait, CAPTURE_BUFFER_HNS, RENDER_BUFFER_HNS};
    use crate::audio::{device, ComGuard};

    const BURST_EVERY: u64 = 24_000; // 500 ms
    const BURST_LEN: u64 = 240; // 5 ms
    const HNS_PER_SEC: f64 = 10_000_000.0;

    /// Plays a short 2 kHz burst every 500 ms. Returns the QPC time (100 ns units) at which
    /// each burst's first sample reached the device, using the device clock.
    pub fn render_bursts(device: &IMMDevice, stop: &AtomicBool) -> Result<Vec<f64>> {
        let s = open(device, 2, RENDER_BUFFER_HNS)?;
        let render: IAudioRenderClient = unsafe { s.client.GetService()? };
        let clock: IAudioClock = unsafe { s.client.GetService()? };
        let freq = unsafe { clock.GetFrequency()? } as f64;
        let buffer_frames = unsafe { s.client.GetBufferSize()? };
        unsafe { s.client.Start()? };
        let (mut written, mut pending, mut times) = (0u64, Vec::new(), Vec::new());
        while !stop.load(Ordering::Relaxed) {
            wait(s.event)?;
            let n = buffer_frames - unsafe { s.client.GetCurrentPadding()? };
            if n > 0 {
                let data = unsafe { render.GetBuffer(n)? };
                let buf = unsafe { std::slice::from_raw_parts_mut(data as *mut f32, n as usize * 2) };
                for (i, frame) in buf.chunks_exact_mut(2).enumerate() {
                    let phase = (written + i as u64) % BURST_EVERY;
                    if phase == 0 {
                        pending.push(written + i as u64);
                    }
                    let v = if phase < BURST_LEN {
                        ((std::f64::consts::TAU * 2000.0 * phase as f64 / 48_000.0).sin() * 0.25) as f32
                    } else {
                        0.0
                    };
                    frame.fill(v);
                }
                unsafe { render.ReleaseBuffer(n, 0)? };
                written += n as u64;
            }
            let (mut dev_pos, mut qpc) = (0u64, 0u64);
            unsafe { clock.GetPosition(&mut dev_pos, Some(&mut qpc))? };
            let played_secs = dev_pos as f64 / freq;
            for idx in pending.drain(..) {
                times.push(qpc as f64 + (idx as f64 / 48_000.0 - played_secs) * HNS_PER_SEC);
            }
        }
        Ok(times)
    }

    pub struct Recording {
        /// |left channel| per frame.
        pub levels: Vec<f32>,
        /// (index of first frame in packet, QPC time of that frame in 100 ns units).
        pub packets: Vec<(usize, f64)>,
    }

    impl Recording {
        pub fn peak(&self) -> f32 {
            self.levels.iter().fold(0f32, |m, v| m.max(*v))
        }

        /// QPC time (100 ns units) of frame `i`.
        pub fn time_of(&self, i: usize) -> f64 {
            let k = self.packets.partition_point(|(start, _)| *start <= i).saturating_sub(1);
            self.packets.get(k).map_or(0.0, |(start, qpc)| qpc + (i - start) as f64 / 48_000.0 * HNS_PER_SEC)
        }

        /// Loud stretches as (first frame, last frame, peak): samples above 5% of full scale
        /// (and 20% of the recording's peak), with gaps under 2 ms merged.
        pub fn stretches(&self) -> Vec<(usize, usize, f32)> {
            let threshold = (self.peak() * 0.2).max(0.05);
            let mut out: Vec<(usize, usize, f32)> = Vec::new();
            for (i, &v) in self.levels.iter().enumerate() {
                if v <= threshold {
                    continue;
                }
                match out.last_mut() {
                    Some((_, end, peak)) if i - *end <= 96 => {
                        *end = i;
                        *peak = peak.max(v);
                    }
                    _ => out.push((i, i, v)),
                }
            }
            out
        }

        /// Burst onsets: starts of loud stretches preceded by at least 100 ms of quiet.
        pub fn onsets(&self) -> Vec<f64> {
            let mut previous_end: Option<usize> = None;
            let mut onsets = Vec::new();
            for (start, end, _) in self.stretches() {
                if previous_end.map_or(true, |e| start - e > 4_800) {
                    onsets.push(self.time_of(start));
                }
                previous_end = Some(end);
            }
            onsets
        }

        /// Loudest sample at least 10 ms away from any loud stretch, in dBFS.
        pub fn noise_floor_db(&self) -> f32 {
            let stretches = self.stretches();
            let mut max = 0f32;
            let mut s = 0usize;
            for (i, &v) in self.levels.iter().enumerate() {
                while s < stretches.len() && stretches[s].1 + 480 < i {
                    s += 1;
                }
                let near = stretches.get(s).is_some_and(|(start, end, _)| i + 480 >= *start && i <= end + 480);
                if !near {
                    max = max.max(v);
                }
            }
            20.0 * max.max(1e-9).log10()
        }
    }

    /// Records a capture device's left channel with per-packet QPC timestamps.
    pub fn record(device: &IMMDevice, stop: &AtomicBool) -> Result<Recording> {
        let s = open(device, 2, CAPTURE_BUFFER_HNS)?;
        let capture: IAudioCaptureClient = unsafe { s.client.GetService()? };
        unsafe { s.client.Start()? };
        let mut rec = Recording { levels: Vec::new(), packets: Vec::new() };
        while !stop.load(Ordering::Relaxed) {
            wait(s.event)?;
            while unsafe { capture.GetNextPacketSize()? } > 0 {
                let (mut data, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0u32, 0u32, 0u64);
                unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))? };
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
                rec.packets.push((rec.levels.len(), qpc as f64));
                for j in 0..frames as usize {
                    rec.levels.push(if silent { 0.0 } else { unsafe { (*(data as *const f32).add(j * 2)).abs() } });
                }
                unsafe { capture.ReleaseBuffer(frames)? };
            }
        }
        Ok(rec)
    }

    /// Plays bursts into `render_id` while recording `capture_id`; returns the median delay in ms.
    pub fn measure(render_id: &str, capture_id: &str, secs: f64) -> Result<f64> {
        measure_with(render_id, capture_id, secs, 0.0, || ()).map(|(ms, _)| ms)
    }

    /// Like `measure`, with control over start order:
    /// - `render_first_secs > 0` plays into the device that long before anyone records it,
    ///   to see whether unread audio queues up and stays delayed.
    /// - `after_capture_starts` runs once recording has begun, before bursts are played
    ///   (used to start the engine between the two).
    pub fn measure_with<T>(
        render_id: &str,
        capture_id: &str,
        secs: f64,
        render_first_secs: f64,
        after_capture_starts: impl FnOnce() -> T,
    ) -> Result<(f64, T)> {
        let (mut results, extra) = measure_multi(render_id, &[capture_id], secs, render_first_secs, after_capture_starts)?;
        let (delay, _) = results.remove(0);
        Ok((delay?, extra))
    }

    /// Per-burst delay from each onset on one capture to the next onset on another, in ms.
    /// Both sides use capture timestamps, so no render-clock estimate is involved.
    pub fn onset_to_onset_ms(from: &[f64], to: &[f64]) -> Vec<f64> {
        from.iter()
            .filter_map(|&f| to.iter().find(|&&t| t >= f).map(|&t| (t - f) / 10_000.0))
            .filter(|d| *d < 400.0)
            .collect()
    }

    /// Plays bursts into one device while recording several at once. For each capture (in the
    /// order given) returns the median delay from playback and the raw onset times.
    pub fn measure_multi<T>(
        render_id: &str,
        capture_ids: &[&str],
        secs: f64,
        render_first_secs: f64,
        after_capture_starts: impl FnOnce() -> T,
    ) -> Result<(Vec<(Result<f64>, Vec<f64>)>, T)> {
        let stop = Arc::new(AtomicBool::new(false));
        let render_id = render_id.to_string();
        let spawn_renderer = |stop: Arc<AtomicBool>| {
            std::thread::spawn(move || {
                let _com = ComGuard::new();
                render_bursts(&device::by_id(&render_id)?, &stop)
            })
        };
        let spawn_capturers = |stop: &Arc<AtomicBool>| {
            capture_ids
                .iter()
                .map(|id| {
                    let (id, stop) = (id.to_string(), stop.clone());
                    std::thread::spawn(move || {
                        let _com = ComGuard::new();
                        record(&device::by_id(&id)?, &stop)
                    })
                })
                .collect::<Vec<_>>()
        };

        let (renderer, capturers, extra);
        if render_first_secs > 0.0 {
            renderer = spawn_renderer(stop.clone());
            std::thread::sleep(Duration::from_secs_f64(render_first_secs));
            capturers = spawn_capturers(&stop);
            std::thread::sleep(Duration::from_millis(300));
            extra = after_capture_starts();
        } else {
            capturers = spawn_capturers(&stop);
            std::thread::sleep(Duration::from_millis(300));
            extra = after_capture_starts();
            renderer = spawn_renderer(stop.clone());
        }
        std::thread::sleep(Duration::from_secs_f64(secs));
        stop.store(true, Ordering::Relaxed);
        let played = renderer.join().unwrap()?;
        let results = capturers
            .into_iter()
            .zip(capture_ids)
            .map(|(handle, id)| {
                println!("  capture {id}:");
                match handle.join().unwrap() {
                    Ok(recording) => analyze(&played, recording),
                    Err(e) => (Err(e), Vec::new()),
                }
            })
            .collect();
        Ok((results, extra))
    }

    /// Matches recorded onsets to played bursts. Returns the median delay in ms and the onsets.
    fn analyze(played: &[f64], recording: Recording) -> (Result<f64>, Vec<f64>) {
        let heard = recording.onsets();
        println!(
            "  played {} bursts, captured {} frames in {} packets, peak {:.3}, onsets {}",
            played.len(),
            recording.levels.len(),
            recording.packets.len(),
            recording.peak(),
            heard.len()
        );
        if let (Some(p), Some(h)) = (played.first(), heard.first()) {
            println!("  first played at {:.1} ms, first onset at {:.1} ms (QPC)", p / 10_000.0, h / 10_000.0);
        }
        // Shape of the recording, relative to the first burst played.
        let t0 = played.first().copied().unwrap_or(0.0);
        let events = recording.stretches();
        println!("  {} loud stretches, noise floor between bursts {:.1} dBFS; first 6:", events.len(), recording.noise_floor_db());
        for (start, end, peak) in events.iter().take(6) {
            println!(
                "    at {:+8.1} ms, {:6.1} ms long, peak {:.3}",
                (recording.time_of(*start) - t0) / 10_000.0,
                (end - start) as f64 / 48.0,
                peak
            );
        }
        // Bursts are exactly 500 ms apart in sample time, but each device-clock reading jitters by
        // a device period or more. Fit one start time to all readings, then match every onset
        // to the nearest burst (valid while the delay is under 250 ms).
        let spacing = BURST_EVERY as f64 / 48_000.0 * HNS_PER_SEC;
        let mut starts: Vec<f64> = played.iter().enumerate().map(|(k, p)| p - k as f64 * spacing).collect();
        starts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let Some(&t0) = starts.get(starts.len() / 2) else {
            return (Err(anyhow!("no bursts played")), heard);
        };
        let mut delays: Vec<f64> = heard
            .iter()
            .map(|&h| {
                let k = ((h - t0) / spacing).round().max(0.0);
                (h - (t0 + k * spacing)) / 10_000.0
            })
            .filter(|d| d.abs() < 250.0)
            .collect();
        delays.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let Some(&median) = delays.get(delays.len() / 2) else {
            return (Err(anyhow!("no bursts detected")), heard);
        };
        println!("  {} bursts matched, delays {:.1}..{:.1} ms", delays.len(), delays[0], delays[delays.len() - 1]);
        (Ok(median), heard)
    }
}

/// Whether a device mix format is exactly 48 kHz 32-bit float with `channels` channels.
unsafe fn is_float_48k(format: *const WAVEFORMATEX, channels: u16) -> bool {
    let f = std::ptr::read_unaligned(format);
    if f.nSamplesPerSec != SAMPLE_RATE || f.nChannels != channels || f.wBitsPerSample != 32 {
        return false;
    }
    match f.wFormatTag {
        WAVE_FORMAT_IEEE_FLOAT => true,
        WAVE_FORMAT_EXTENSIBLE => {
            let ext = format as *const WAVEFORMATEXTENSIBLE;
            std::ptr::read_unaligned(std::ptr::addr_of!((*ext).SubFormat)) == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        }
        _ => false,
    }
}

/// Opens a render stream in Windows' low-latency shared mode (the device's smallest engine
/// period) when the device already runs at 48 kHz float, so no format conversion is needed.
/// Returns None when the device doesn't qualify; the caller then uses the regular 20 ms buffer.
fn open_low_latency_render(device: &IMMDevice, channels: u16) -> Result<Option<Opened>> {
    unsafe {
        phase("activate low-latency");
        let Ok(client) = device.Activate::<IAudioClient>(CLSCTX_ALL, None) else { return Ok(None) };
        let Ok(client3) = client.cast::<IAudioClient3>() else { return Ok(None) };
        let Ok(mix) = client.GetMixFormat() else { return Ok(None) };
        let (mut default, mut fundamental, mut min, mut max) = (0u32, 0u32, 0u32, 0u32);
        let init = if is_float_48k(mix, channels) {
            client3
                .GetSharedModeEnginePeriod(mix, &mut default, &mut fundamental, &mut min, &mut max)
                .and_then(|()| client3.InitializeSharedAudioStream(AUDCLNT_STREAMFLAGS_EVENTCALLBACK, min, mix, None))
                .is_ok()
        } else {
            false
        };
        CoTaskMemFree(Some(mix as *const _));
        if !init {
            return Ok(None);
        }
        let event = CreateEventW(None, false, false, None)?;
        let opened = Opened { client, event, channels: channels as usize, period_frames: Some(min) };
        opened.client.SetEventHandle(opened.event)?;
        if cfg!(debug_assertions) {
            eprintln!("[audio] low-latency output: {min}-frame period ({:.1} ms)", min as f64 / 48.0);
        }
        Ok(Some(opened))
    }
}

/// Renders while `keep_going` returns true; `fill` must write every sample of the interleaved buffer. `starved` counts wake-ups where the device had
/// already played everything queued, i.e. an audible gap in the output.
pub fn run_render_while(
    device: &IMMDevice,
    channels: u16,
    keep_going: impl Fn() -> bool,
    starved: Option<&AtomicU64>,
    mut fill: impl FnMut(&mut [f32]),
) -> Result<()> {
    let s = match open_low_latency_render(device, channels)? {
        Some(s) => s,
        None => open(device, channels, RENDER_BUFFER_HNS)?,
    };
    phase("get render service");
    let render: IAudioRenderClient = unsafe { s.client.GetService()? };
    let buffer_frames = unsafe { s.client.GetBufferSize()? };
    if buffer_frames == 0 {
        bail!("render device reported an empty buffer");
    }
    phase("start");
    unsafe { s.client.Start()? };
    phase("mmcss");
    let _mmcss = ProAudioThread::new();

    let mut first_wake = true;
    while keep_going() {
        phase("wait");
        wait(s.event)?;
        phase("fill");
        let padding = unsafe { s.client.GetCurrentPadding()? };
        if padding == 0 && !first_wake {
            if let Some(counter) = starved {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        }
        first_wake = false;
        let frames = match s.period_frames {
            // Low-latency mode: keep two periods queued instead of the whole buffer, otherwise
            // the smaller period saves nothing.
            Some(period) => (2 * period).min(buffer_frames).saturating_sub(padding),
            None => buffer_frames - padding,
        };
        if frames == 0 {
            continue;
        }
        let data = unsafe { render.GetBuffer(frames)? };
        let buf = unsafe { std::slice::from_raw_parts_mut(data as *mut f32, frames as usize * s.channels) };
        fill(buf);
        unsafe { render.ReleaseBuffer(frames, 0)? };
    }
    Ok(())
}
