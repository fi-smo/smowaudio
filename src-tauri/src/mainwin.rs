//! The native (Slint) main window, being ported view by view from the WebView one in ../ui. It
//! lives on the Slint thread next to the flyout (see flyout.rs) and reads settings, devices,
//! status and meters straight from AppState and the engine. Until every view is ported it's
//! opened with `smowaudio.exe --native-window`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use tauri::{AppHandle, Listener, Manager};

use crate::audio::device::{self, DeviceInfo, Flow};
use crate::config::{Config, CHANNEL_COUNT, CHANNEL_NAMES};
use crate::flyout::{device_missing, meter_fraction, short_name};
use crate::ui::{AppIcon, MainWindow, Px, StripData, Theme};
use crate::{audio, notify_config_changed, AppState};

const MAX_VOLUME: f32 = 1.5;
/// Meters update this often while the window is open, like the old page's polling.
const METER_EVERY: Duration = Duration::from_millis(50);
/// Settings, devices, apps and status are re-read this often (and on every settings change).
const REFRESH_EVERY: Duration = Duration::from_secs(2);
const PEAK_HOLD: Duration = Duration::from_millis(900);
const MIC_STEPS: [&str; 5] = ["denoise", "gate", "eq", "compressor", "limiter"];

thread_local! {
    static MAIN: RefCell<Option<Main>> = const { RefCell::new(None) };
}

struct Main {
    window: MainWindow,
    channels: Rc<VecModel<StripData>>,
    /// Peak hold per strip (Game..Aux, Master, Mic) and side: value and when it was last raised.
    holds: [[(f32, Instant); 2]; CHANNEL_COUNT + 2],
    meter_timer: slint::Timer,
    refresh_timer: slint::Timer,
    /// The channel whose EQ drawer is open.
    eq_open: Option<usize>,
}

/// Height of the Mixer's EQ graph, in logical pixels.
const DRAWER_GRAPH: f32 = 118.0;

/// Opens the native main window (creating it the first time). Call from any thread.
pub fn open(app: &AppHandle) {
    let app = app.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let created = MAIN.with(|m| m.borrow().is_some());
        if !created {
            if let Err(e) = create(&app) {
                crate::append_log(&format!("native main window unavailable: {e}"));
                return;
            }
        }
        show(&app);
    });
}

fn create(app: &AppHandle) -> Result<(), slint::PlatformError> {
    let window = MainWindow::new()?;
    let channels = Rc::new(VecModel::<StripData>::default());
    window.set_channels(ModelRc::from(channels.clone()));
    wire_callbacks(&window, app);
    MAIN.with(|m| {
        *m.borrow_mut() = Some(Main {
            window,
            channels,
            holds: [[(0.0, Instant::now()); 2]; CHANNEL_COUNT + 2],
            meter_timer: slint::Timer::default(),
            refresh_timer: slint::Timer::default(),
            eq_open: None,
        })
    });
    // Settings changed elsewhere (flyout, shortcuts, the old window): show them right away.
    let handle = app.clone();
    app.listen_any("config-changed", move |event| {
        if event.payload().contains("\"main-native\"") {
            return;
        }
        let handle = handle.clone();
        let _ = slint::invoke_from_event_loop(move || refresh_if_visible(&handle));
    });
    Ok(())
}

fn show(app: &AppHandle) {
    refresh(app);
    MAIN.with(|m| {
        let m = m.borrow();
        let Some(m) = m.as_ref() else { return };
        m.window.global::<Theme>().set_dark(!crate::apps_use_light_theme());
        if let Err(e) = m.window.show() {
            crate::append_log(&format!("native main window didn't open: {e}"));
            return;
        }
        m.window.global::<Px>().set_scale(m.window.window().scale_factor());
        let handle = app.clone();
        m.meter_timer.start(slint::TimerMode::Repeated, METER_EVERY, move || update_meters(&handle));
        let handle = app.clone();
        m.refresh_timer.start(slint::TimerMode::Repeated, REFRESH_EVERY, move || refresh_if_visible(&handle));
    });
}

fn wire_callbacks(window: &MainWindow, app: &AppHandle) {
    let handle = app.clone();
    window.on_strip_volume(move |strip, volume| {
        let state = handle.state::<AppState>();
        match strip as usize {
            i if i < CHANNEL_COUNT => {
                let mut s = state.config.lock().channels[i].settings.clone();
                s.volume = volume;
                state.set_channel_settings(i, s);
            }
            CHANNEL_COUNT => {
                let mut s = state.config.lock().master.clone();
                s.volume = volume;
                state.set_master_settings(s);
            }
            _ => {
                let mut mic = state.config.lock().mic.clone();
                mic.gain_db = if volume <= 0.001 { -60.0 } else { (20.0 * volume.log10() * 10.0).round() / 10.0 };
                state.set_mic_settings(mic);
            }
        }
        notify_config_changed(&handle, "main-native");
        refresh(&handle);
    });
    let handle = app.clone();
    window.on_strip_mute(move |strip| {
        let state = handle.state::<AppState>();
        match strip as usize {
            i if i < CHANNEL_COUNT => {
                let mut s = state.config.lock().channels[i].settings.clone();
                s.muted = !s.muted;
                state.set_channel_settings(i, s);
            }
            CHANNEL_COUNT => {
                let mut s = state.config.lock().master.clone();
                s.muted = !s.muted;
                state.set_master_settings(s);
            }
            _ => {
                let mut mic = state.config.lock().mic.clone();
                mic.muted = !mic.muted;
                state.set_mic_settings(mic);
            }
        }
        notify_config_changed(&handle, "main-native");
        refresh(&handle);
    });
    let handle = app.clone();
    window.on_strip_feature(move |strip| {
        let strip = strip as usize;
        if strip < CHANNEL_COUNT {
            // A channel's EQ button opens its drawer, or closes it if it's open.
            MAIN.with(|m| {
                if let Some(m) = m.borrow_mut().as_mut() {
                    m.eq_open = if m.eq_open == Some(strip) { None } else { Some(strip) };
                    m.window.set_eq_selected(-1);
                }
            });
            refresh(&handle);
        } else if strip == CHANNEL_COUNT + 1 {
            // The Mic's chain lives in the Mic view.
            MAIN.with(|m| {
                if let Some(m) = m.borrow().as_ref() {
                    m.window.set_view("mic".into());
                }
            });
        }
    });
    wire_eq(window, app);
    let weak = window.as_weak();
    window.on_buffer_clicked(move || {
        if let Some(w) = weak.upgrade() {
            w.set_view("settings".into());
        }
    });
    let weak = window.as_weak();
    window.on_status_clicked(move || {
        if let Some(w) = weak.upgrade() {
            w.set_view("settings".into());
        }
    });
    let weak = window.as_weak();
    window.on_update_clicked(move || {
        if let Some(w) = weak.upgrade() {
            w.set_view("settings".into());
        }
    });
}

/// The open drawer's channel, if any.
fn eq_open() -> Option<usize> {
    MAIN.with(|m| m.borrow().as_ref().and_then(|m| m.eq_open))
}

/// Changes the open drawer's channel EQ, applies it and redraws the drawer.
fn edit_eq(app: &AppHandle, change: impl FnOnce(&mut crate::dsp::chain::EqSettings)) {
    let Some(ch) = eq_open() else { return };
    let state = app.state::<AppState>();
    let mut settings = state.config.lock().channels[ch].settings.clone();
    change(&mut settings.eq);
    let eq = settings.eq.clone();
    state.set_channel_settings(ch, settings);
    notify_config_changed(app, "main-native");
    MAIN.with(|m| {
        if let Some(m) = m.borrow().as_ref() {
            m.window.set_eq(crate::eqedit::view(&eq, DRAWER_GRAPH));
            if let Some(mut strip) = m.channels.row_data(ch) {
                strip.feature_v = if eq.enabled { eq.preset.as_str().into() } else { "Off".into() };
                m.channels.set_row_data(ch, strip);
            }
        }
    });
}

fn wire_eq(window: &MainWindow, app: &AppHandle) {
    let handle = app.clone();
    window.on_eq_toggled(move |on| edit_eq(&handle, |eq| eq.enabled = on));
    let handle = app.clone();
    window.on_eq_preset(move |name| edit_eq(&handle, |eq| crate::eqedit::apply_preset(eq, &name)));
    let handle = app.clone();
    window.on_eq_pick(move |x, y, w, h, radius| {
        let Some(ch) = eq_open() else { return -1 };
        let eq = handle.state::<AppState>().config.lock().channels[ch].settings.eq.clone();
        crate::eqedit::pick(&eq, x, y, w, h, radius).map_or(-1, |i| i as i32)
    });
    let handle = app.clone();
    window.on_eq_drag(move |i, x, y, w, h| edit_eq(&handle, |eq| crate::eqedit::drag(eq, i as usize, x, y, w, h)));
    let handle = app.clone();
    window.on_eq_widen(move |i, narrower| edit_eq(&handle, |eq| crate::eqedit::widen(eq, i as usize, narrower)));
}

fn refresh_if_visible(app: &AppHandle) {
    let visible = MAIN.with(|m| m.borrow().as_ref().is_some_and(|m| m.window.window().is_visible()));
    if visible {
        refresh(app);
    }
}

/// Apps playing audio, one per exe (its processes merged), sorted by name.
#[derive(Clone)]
pub(crate) struct App {
    pub exe: String,
    pub name: String,
    pub path: String,
    pub pids: Vec<u32>,
    pub active: bool,
    pub assigned_device: Option<String>,
    pub playing_on: Vec<String>,
}

pub(crate) fn group_apps(list: Vec<crate::audio::routing::AudioApp>) -> Vec<App> {
    let mut by_exe: Vec<App> = Vec::new();
    for a in list {
        if let Some(g) = by_exe.iter_mut().find(|g| g.exe == a.exe) {
            g.pids.push(a.pid);
            g.active |= a.active;
            if g.assigned_device.is_none() {
                g.assigned_device = a.assigned_device;
            }
            for id in a.playing_on {
                if !g.playing_on.contains(&id) {
                    g.playing_on.push(id);
                }
            }
        } else {
            by_exe.push(App {
                exe: a.exe,
                name: a.name,
                path: a.path,
                pids: vec![a.pid],
                active: a.active,
                assigned_device: a.assigned_device,
                playing_on: a.playing_on,
            });
        }
    }
    by_exe.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    by_exe
}

/// The channel an app plays on, and whether it was placed there (or just follows the default,
/// Game): its rule, else the cable it's routed to.
pub(crate) fn channel_of(config: &Config, app: &App) -> (usize, bool) {
    if let Some(&ch) = config.app_rules.get(&app.exe) {
        if ch < CHANNEL_COUNT {
            return (ch, true);
        }
    }
    if let Some(i) = config.channels.iter().position(|c| c.sink.is_some() && c.sink == app.assigned_device) {
        return (i, true);
    }
    (0, false)
}

thread_local! {
    static ICONS: RefCell<std::collections::HashMap<String, Option<slint::Image>>> = RefCell::new(Default::default());
}

/// The app's own icon as an image for the native windows, cached per path.
pub(crate) fn icon_image(path: &str) -> Option<slint::Image> {
    if path.is_empty() {
        return None;
    }
    ICONS.with(|cache| {
        cache
            .borrow_mut()
            .entry(path.to_string())
            .or_insert_with(|| {
                crate::icons::app_icon_rgba(path).map(|icon| {
                    let (w, h, rgba) = &*icon;
                    slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(rgba, *w, *h))
                })
            })
            .clone()
    })
}

pub(crate) fn app_icon(app: &App) -> AppIcon {
    let image = icon_image(&app.path);
    AppIcon {
        has_image: image.is_some(),
        image: image.unwrap_or_default(),
        letter: app.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into(),
        name: app.name.as_str().into(),
        live: app.active,
    }
}

/// "−14.0 dB", "+3.5 dB", "0.0 dB", "−∞ dB".
fn fmt_db(db: f32) -> String {
    if !db.is_finite() || db < -99.0 {
        return "−∞ dB".into();
    }
    let sign = if db > 0.05 { "+" } else if db < -0.05 { "−" } else { "" };
    format!("{sign}{:.1} dB", db.abs())
}

fn db_of(volume: f32) -> f32 {
    if volume <= 0.0001 {
        f32::NEG_INFINITY
    } else {
        20.0 * volume.log10()
    }
}

/// "CABLE-A" for "CABLE-A Output (VB-Audio Cable A)", like the old page.
fn cable_label(devices: &[DeviceInfo], id: &Option<String>) -> String {
    let Some(id) = id else { return "No cable".into() };
    let Some(d) = devices.iter().find(|d| &d.id == id) else { return "No cable".into() };
    let base = d.name.split(" (").next().unwrap_or(&d.name);
    base.trim_end_matches(" Output").trim_end_matches(" Input").to_string()
}

/// Everything but the meters, from the current settings, devices, apps and stream status.
fn refresh(app: &AppHandle) {
    let _com = audio::ComGuard::new();
    let state = app.state::<AppState>();
    let config: Config = state.config.lock().clone();
    let status = state.engine.lock().as_ref().map(|e| e.shared.status.lock().clone()).unwrap_or_default();
    let mut devices = device::list(Flow::Capture).unwrap_or_default();
    devices.extend(device::list(Flow::Render).unwrap_or_default());
    let resolve = |flow: Flow, id: &Option<String>| {
        device::resolve_physical(flow, id.as_deref(), config.previous_default(flow).as_deref())
            .ok()
            .and_then(|d| device::friendly_name(&d).ok())
    };
    let active_output = resolve(Flow::Render, &config.output_device);
    let active_mic = resolve(Flow::Capture, &config.mic_device);
    let apps = group_apps(crate::audio::routing::list_apps().unwrap_or_default());

    MAIN.with(|m| {
        let m = m.borrow();
        let Some(m) = m.as_ref() else { return };
        let w = &m.window;

        // Channels.
        for (i, ch) in config.channels.iter().enumerate() {
            let old = m.channels.row_data(i);
            // Apps on this channel: up to five icons, then "+N". Unplaced apps play on Game.
            let on_channel: Vec<&App> = apps.iter().filter(|a| channel_of(&config, a).0 == i).collect();
            let icons: Vec<AppIcon> =
                if ch.source.is_some() { on_channel.iter().take(5).map(|a| app_icon(a)).collect() } else { Vec::new() };
            let more = if ch.source.is_some() { on_channel.len().saturating_sub(5) as i32 } else { 0 };
            let note = if ch.source.is_none() { "Pick a cable in Settings" } else { "No apps yet" };
            let s = &ch.settings;
            let strip = StripData {
                name: CHANNEL_NAMES[i].into(),
                sub: cable_label(&devices, &ch.source).into(),
                note: note.into(),
                apps: ModelRc::from(Rc::new(VecModel::from(icons))),
                more_apps: more,
                kind: i as i32,
                volume: s.volume,
                muted: s.muted,
                readout: fmt_db(db_of(s.volume)).into(),
                feature_k: "EQ".into(),
                feature_v: if s.eq.enabled { s.eq.preset.as_str().into() } else { "Off".into() },
                feature_open: m.eq_open == Some(i),
                feature_static: false,
                ..old.unwrap_or_default()
            };
            if i < m.channels.row_count() {
                m.channels.set_row_data(i, strip);
            } else {
                m.channels.push(strip);
            }
        }

        // The EQ drawer.
        w.set_eq_open(m.eq_open.map_or(-1, |c| c as i32));
        if let Some(ch) = m.eq_open {
            w.set_eq(crate::eqedit::view(&config.channels[ch].settings.eq, DRAWER_GRAPH));
        }

        // Master.
        let output = active_output.as_deref().map(short_name).unwrap_or_else(|| "Automatic".into());
        let mut master = w.get_master();
        master.name = "Master".into();
        master.sub = "Output".into();
        master.note = output.as_str().into();
        master.kind = CHANNEL_COUNT as i32;
        master.volume = config.master.volume;
        master.muted = config.master.muted;
        master.readout = fmt_db(db_of(config.master.volume)).into();
        master.feature_k = "Limit".into();
        master.feature_static = true;
        w.set_master(master);

        // Mic.
        let mic_cfg = &config.mic;
        let steps_on = [
            mic_cfg.denoise.enabled,
            mic_cfg.gate.enabled,
            mic_cfg.eq.enabled,
            mic_cfg.compressor.enabled,
            mic_cfg.limiter.enabled,
        ]
        .iter()
        .filter(|on| **on)
        .count();
        let mut mic = w.get_mic();
        mic.name = "Mic".into();
        mic.sub = cable_label(&devices, &config.mic_sink).into();
        mic.note = active_mic.as_deref().map(short_name).unwrap_or_else(|| "No microphone".into()).into();
        mic.kind = CHANNEL_COUNT as i32 + 1;
        mic.volume = 10f32.powf(mic_cfg.gain_db / 20.0).clamp(0.0, MAX_VOLUME);
        mic.muted = mic_cfg.muted;
        mic.readout = fmt_db(mic_cfg.gain_db).into();
        mic.feature_k = "Chain".into();
        mic.feature_v = format!("{steps_on} of {} on", MIC_STEPS.len()).into();
        w.set_mic(mic);

        // Top bar.
        w.set_output_name(output.into());
        let failing = status
            .iter()
            .filter(|(k, v)| v.as_str() != "running" && !(k.as_str() == "Microphone" && device_missing(v)))
            .count();
        let mic_off = status.get("Microphone").is_some_and(|v| device_missing(v));
        w.set_status_warn(failing > 0);
        w.set_status_text(
            match failing {
                0 if mic_off => "Running · mic off".to_string(),
                0 => "All streams running".to_string(),
                1 => "1 stream needs attention".to_string(),
                n => format!("{n} streams need attention"),
            }
            .into(),
        );
        let update = state.updates.status_available();
        w.set_update_version(update.unwrap_or_default().into());
    });
}

/// Meters at 20 fps: each strip's two sides with the mixer's peak hold, Buffer and Limit.
fn update_meters(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(meters) = state.engine.lock().as_ref().map(|e| e.meters()) else { return };
    // The engine reports 0 dB for a mic that isn't running; show it empty.
    let mic_running = state
        .engine
        .lock()
        .as_ref()
        .is_some_and(|e| e.shared.status.lock().get("Microphone").map(String::as_str) == Some("running"));
    let mic_db = if mic_running { meters.mic.output_db } else { f32::NEG_INFINITY };
    let now = Instant::now();
    MAIN.with(|m| {
        let mut m = m.borrow_mut();
        let Some(m) = m.as_mut() else { return };
        let pairs: Vec<[f32; 2]> = meters
            .channels
            .iter()
            .copied()
            .chain([meters.master, [mic_db, mic_db]])
            .collect();
        for (i, pair) in pairs.iter().enumerate() {
            let mut strip = if i < CHANNEL_COUNT {
                match m.channels.row_data(i) {
                    Some(s) => s,
                    None => continue,
                }
            } else if i == CHANNEL_COUNT {
                m.window.get_master()
            } else {
                m.window.get_mic()
            };
            let levels = [meter_fraction(pair[0]), meter_fraction(pair[1])];
            let mut peaks = [0.0; 2];
            for side in 0..2 {
                let level = if strip.muted { 0.0 } else { levels[side] };
                let (hold, at) = &mut m.holds[i][side];
                if level >= *hold {
                    *hold = level;
                    *at = now;
                } else if now - *at > PEAK_HOLD {
                    *hold = (*hold - 0.025).max(level);
                }
                peaks[side] = *hold;
            }
            let limit = (i == CHANNEL_COUNT).then(|| fmt_db(meters.master_reduction_db));
            let changed = limit.as_deref().is_some_and(|l| strip.feature_v.as_str() != l)
                || (strip.level_l - levels[0]).abs() > 0.001
                || (strip.level_r - levels[1]).abs() > 0.001
                || (strip.peak_l - peaks[0]).abs() > 0.001
                || (strip.peak_r - peaks[1]).abs() > 0.001;
            if !changed {
                continue;
            }
            strip.level_l = levels[0];
            strip.level_r = levels[1];
            strip.peak_l = peaks[0];
            strip.peak_r = peaks[1];
            if let Some(limit) = limit {
                strip.feature_v = limit.into();
            }
            if i < CHANNEL_COUNT {
                m.channels.set_row_data(i, strip);
            } else if i == CHANNEL_COUNT {
                m.window.set_master(strip);
            } else {
                m.window.set_mic(strip);
            }
        }
        m.window.set_buffer(
            if meters.buffer_ms > 0.0 { format!("{} ms", meters.buffer_ms.round()) } else { "–".into() }.into(),
        );
    });
}
