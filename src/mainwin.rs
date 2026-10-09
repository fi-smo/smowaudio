//! The main window: Mixer, Apps, Mic and Settings (ui/main.slint). It lives on the main
//! thread with the other windows (see app.rs) and reads settings, devices, status and meters
//! straight from AppState and the engine.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::audio::device::{self, DeviceInfo, Flow};
use crate::config::{Config, CHANNEL_COUNT, CHANNEL_NAMES};
use crate::flyout::{device_missing, meter_fraction, short_name};
use crate::ui::{AppCardData, AppIcon, ChainTab, LaneData, MainWindow, MicData, Px, StripData, Theme};
use crate::app::AppHandle;
use crate::{audio, notify_config_changed};

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
    /// The Apps view's columns, and what they were built from (exe, channel, placed) so they're
    /// only rebuilt when that changes, never under a card being dragged.
    lanes: [Rc<VecModel<AppCardData>>; CHANNEL_COUNT],
    lane_signature: String,
    apps: Vec<App>,
    /// Per exe: the level meter's state, and since when it has been playing on the wrong device.
    app_meters: std::collections::HashMap<String, AppMeter>,
    stuck_since: std::collections::HashMap<String, Instant>,
    /// The Mic view's chain tabs.
    chain: Rc<VecModel<ChainTab>>,
}

#[derive(Clone, Copy)]
struct AppMeter {
    db: f32,
    hold: f32,
    hold_at: Instant,
    at: Instant,
}

/// A placed app still playing on another device this long after a move gets "Reopen to move".
const STUCK_AFTER: Duration = Duration::from_millis(2500);
const APP_SEGMENTS: f32 = 12.0;

/// Height of the Mixer's EQ graph, in logical pixels.
const DRAWER_GRAPH: f32 = 118.0;

/// Whether the main window is on screen; readable from any thread (the automatic updater waits
/// for it to close).
static OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn is_open() -> bool {
    OPEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Opens the main window (creating it the first time), on a given view ("mixer", "apps", "mic",
/// "settings") or the one it last showed. Call on the main thread.
pub fn open(app: &AppHandle, view: Option<&str>) {
    crate::flyout::hide();
    let created = MAIN.with(|m| m.borrow().is_some());
    if !created {
        if let Err(e) = create(app) {
            crate::append_log(&format!("main window unavailable: {e}"));
            return;
        }
    }
    if let (Some(view), Some(w)) = (view, window()) {
        w.set_view(view.into());
    }
    show(app);
}

/// Settings changed elsewhere (flyout, shortcuts): show them right away. Call on the main thread.
pub fn config_changed(app: &AppHandle, source: &str) {
    // The window's own changes are already on screen, and redrawing could move a fader that's
    // being dragged.
    if source != "main-native" {
        refresh_if_visible(app);
    }
}

fn create(app: &AppHandle) -> Result<(), slint::PlatformError> {
    let window = crate::app::create_window(crate::app::WindowKind::Main, MainWindow::new)?;
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
            lanes: std::array::from_fn(|_| Rc::new(VecModel::default())),
            lane_signature: String::new(),
            apps: Vec::new(),
            app_meters: Default::default(),
            stuck_since: Default::default(),
            chain: Rc::new(VecModel::default()),
        })
    });
    MAIN.with(|m| {
        if let Some(m) = m.borrow().as_ref() {
            let lanes: Vec<LaneData> = m
                .lanes
                .iter()
                .enumerate()
                .map(|(i, cards)| LaneData { name: CHANNEL_NAMES[i].into(), cards: ModelRc::from(cards.clone()) })
                .collect();
            m.window.set_lanes(ModelRc::from(Rc::new(VecModel::from(lanes))));
            m.window.set_chain(ModelRc::from(m.chain.clone()));
        }
    });
    // Closing only hides it: the app keeps running in the tray.
    let handle = *app;
    MAIN.with(|m| {
        if let Some(m) = m.borrow().as_ref() {
            m.window.window().on_close_requested(move || {
                closed(&handle);
                slint::CloseRequestResponse::HideWindow
            });
        }
    });
    Ok(())
}

fn show(app: &AppHandle) {
    refresh(app);
    MAIN.with(|m| {
        let m = m.borrow();
        let Some(m) = m.as_ref() else { return };
        m.window.global::<Theme>().set_dark(!crate::apps_use_light_theme());
        let window = m.window.window();
        if window.is_minimized() {
            window.set_minimized(false);
        }
        if let Err(e) = m.window.show() {
            crate::append_log(&format!("main window didn't open: {e}"));
            return;
        }
        OPEN.store(true, std::sync::atomic::Ordering::Relaxed);
        crate::app::bring_to_front(window);
        m.window.global::<Px>().set_scale(m.window.window().scale_factor());
        let handle = app.clone();
        m.meter_timer.start(slint::TimerMode::Repeated, METER_EVERY, move || update_meters(&handle));
        let handle = app.clone();
        m.refresh_timer.start(slint::TimerMode::Repeated, REFRESH_EVERY, move || refresh_if_visible(&handle));
    });
}

/// The window was closed: stop the timers and give shortcuts back if one was being recorded.
fn closed(app: &AppHandle) {
    OPEN.store(false, std::sync::atomic::Ordering::Relaxed);
    crate::settingsui::stop_recording(app);
    MAIN.with(|m| {
        if let Some(m) = m.borrow().as_ref() {
            m.meter_timer.stop();
            m.refresh_timer.stop();
        }
    });
}

fn wire_callbacks(window: &MainWindow, app: &AppHandle) {
    let handle = app.clone();
    window.on_strip_volume(move |strip, volume| {
        let state = handle.state();
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
        let state = handle.state();
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
    wire_mic(window, app);
    // A view shows the latest state as soon as it opens.
    let handle = app.clone();
    window.on_view_changed(move |view| {
        if view != "settings" {
            crate::settingsui::stop_recording(&handle);
        }
        refresh(&handle);
    });
    crate::settingsui::wire(window, app);
    let handle = app.clone();
    window.on_move_app(move |exe, channel| {
        let exe = exe.to_string();
        let channel = (channel >= 0).then_some(channel as usize);
        let Some(pid) = MAIN.with(|m| {
            m.borrow().as_ref().and_then(|m| m.apps.iter().find(|a| a.exe == exe).map(|a| a.pids[0]))
        }) else {
            return;
        };
        MAIN.with(|m| {
            if let Some(m) = m.borrow_mut().as_mut() {
                m.stuck_since.remove(&exe);
            }
        });
        let _com = audio::ComGuard::new();
        if let Err(e) = crate::assign_app_now(&handle.state(), pid, exe, channel) {
            crate::append_log(&format!("moving an app failed: {e}"));
        }
        notify_config_changed(&handle, "main-native");
        refresh(&handle);
    });
    let weak = window.as_weak();
    let handle = app.clone();
    window.on_buffer_clicked(move || {
        if let Some(w) = weak.upgrade() {
            w.set_settings_tab("devices".into());
            w.set_view("settings".into());
        }
        refresh(&handle);
    });
    let weak = window.as_weak();
    let handle = app.clone();
    window.on_status_clicked(move || {
        if let Some(w) = weak.upgrade() {
            w.set_settings_tab("devices".into());
            w.set_view("settings".into());
        }
        refresh(&handle);
    });
    let weak = window.as_weak();
    let handle = app.clone();
    window.on_update_clicked(move || {
        if let Some(w) = weak.upgrade() {
            w.set_settings_tab("updates".into());
            w.set_view("settings".into());
        }
        refresh(&handle);
    });
}

/// The open drawer's channel, if any.
fn eq_open() -> Option<usize> {
    MAIN.with(|m| m.borrow().as_ref().and_then(|m| m.eq_open))
}

/// Changes the open drawer's channel EQ, applies it and redraws the drawer.
fn edit_eq(app: &AppHandle, change: impl FnOnce(&mut crate::dsp::chain::EqSettings)) {
    let Some(ch) = eq_open() else { return };
    let state = app.state();
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
        let eq = handle.state().config.lock().channels[ch].settings.eq.clone();
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
    let state = app.state();
    let config: Config = state.config.lock().clone();
    let status = state.engine.lock().as_ref().map(|e| e.shared.status.lock().clone()).unwrap_or_default();
    let capture = device::list(Flow::Capture).unwrap_or_default();
    let render = device::list(Flow::Render).unwrap_or_default();
    let mut devices = capture.clone();
    devices.extend(render.iter().cloned());
    let resolve = |flow: Flow, id: &Option<String>| {
        device::resolve_physical(flow, id.as_deref(), config.previous_default(flow).as_deref())
            .ok()
            .and_then(|d| device::friendly_name(&d).ok())
    };
    let active_output = resolve(Flow::Render, &config.output_device);
    let active_mic = resolve(Flow::Capture, &config.mic_device);
    let apps = group_apps(crate::audio::routing::list_apps().unwrap_or_default());

    MAIN.with(|m| {
        let mut m = m.borrow_mut();
        let Some(m) = m.as_mut() else { return };
        let w = m.window.clone_strong();
        let w = &w;

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

        update_lanes(m, &config, apps.clone());

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

        // The Mic view (its live parts come from the meter timer).
        let virtual_mic = virtual_mic_name(&devices, &config);
        let mut md = w.get_mic_data();
        md.muted = mic_cfg.muted;
        md.listen = mic_cfg.monitor;
        md.route_from = active_mic.as_deref().map(short_name).unwrap_or_default().into();
        md.route_auto = config.mic_device.is_none();
        md.route_to = virtual_mic.clone().unwrap_or_default().into();
        md.input_text = match &active_mic {
            Some(name) => format!("Recording from {name}. A mic you plug in later is picked up automatically."),
            None => "No microphone found. Connect one, or pick a specific device in Settings. A mic you plug in later is picked up automatically.".into(),
        }
        .into();
        md.virtual_mic = virtual_mic.unwrap_or_else(|| "the Virtual Mic cable's Output".into()).into();
        fill_mic_settings(&mut md, mic_cfg);
        w.set_mic_data(md);
        w.set_mic_eq(crate::eqedit::view(&mic_cfg.eq, DRAWER_GRAPH));

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
        if w.get_view() == "settings" {
            crate::settingsui::refresh(app, w, &config, &render, &capture, &status);
        }
    });
}

/// The main window, if it has been opened.
pub(crate) fn window() -> Option<MainWindow> {
    MAIN.with(|m| m.borrow().as_ref().map(|m| m.window.clone_strong()))
}

/// Re-reads just the Settings view.
pub(crate) fn refresh_settings(app: &AppHandle) {
    let Some(w) = window() else { return };
    let _com = audio::ComGuard::new();
    let state = app.state();
    let config: Config = state.config.lock().clone();
    let status = state.engine.lock().as_ref().map(|e| e.shared.status.lock().clone()).unwrap_or_default();
    let capture = device::list(Flow::Capture).unwrap_or_default();
    let render = device::list(Flow::Render).unwrap_or_default();
    crate::settingsui::refresh(app, &w, &config, &render, &capture, &status);
}

/// The Apps view's columns: rebuilt when apps come or go or change channel, else updated in place
/// (levels and notes come from `update_app_levels`).
fn update_lanes(m: &mut Main, config: &Config, apps: Vec<App>) {
    let now = Instant::now();
    // "Reopen to move": placed, playing, but not on its channel's cable.
    let mut stuck = std::collections::HashSet::new();
    for app in &apps {
        let (ch, chosen) = channel_of(config, app);
        let sink = config.channels[ch].sink.as_ref();
        if app.active && chosen && sink.is_some_and(|s| !app.playing_on.is_empty() && !app.playing_on.contains(s)) {
            stuck.insert(app.exe.clone());
        }
    }
    m.stuck_since.retain(|exe, _| stuck.contains(exe));
    for exe in stuck {
        m.stuck_since.entry(exe).or_insert(now);
    }

    let signature: String = apps
        .iter()
        .map(|a| {
            let (ch, chosen) = channel_of(config, a);
            format!("{}:{ch}:{chosen}|", a.exe)
        })
        .collect();
    let rebuild = signature != m.lane_signature;
    m.lane_signature = signature;
    m.window.set_apps_playing(apps.iter().filter(|a| a.active).count() as i32);
    m.window.set_apps_empty(apps.is_empty());
    for (i, lane) in m.lanes.iter().enumerate() {
        let cards: Vec<AppCardData> = apps
            .iter()
            .filter(|a| channel_of(config, a).0 == i)
            .map(|a| {
                let (channel, chosen) = channel_of(config, a);
                let old = (0..lane.row_count()).filter_map(|r| lane.row_data(r)).find(|c| c.exe.as_str() == a.exe);
                let stuck = m.stuck_since.get(&a.exe).is_some_and(|t| now - *t >= STUCK_AFTER);
                AppCardData {
                    exe: a.exe.as_str().into(),
                    icon: app_icon(a),
                    channel: channel as i32,
                    chosen,
                    stuck,
                    stuck_title: format!(
                        "{} chooses its output only when it starts. Close and reopen it to hear it on {}.",
                        a.name, CHANNEL_NAMES[channel]
                    )
                    .into(),
                    ..old.unwrap_or(AppCardData { level: "−∞".into(), silent: true, peak: -1, ..Default::default() })
                }
            })
            .collect();
        if rebuild {
            lane.set_vec(cards);
        } else {
            for (r, card) in cards.into_iter().enumerate() {
                lane.set_row_data(r, card);
            }
        }
    }
    m.apps = apps;
}

/// The Apps view's meters, as often as the channel meters: instant rise, a smooth fall of about
/// 26 dB/s, and a peak mark that holds, like the old page.
fn update_app_levels(m: &mut Main) {
    let Ok(levels) = crate::audio::routing::app_levels() else { return };
    let levels: std::collections::HashMap<u32, f32> = levels.into_iter().collect();
    let now = Instant::now();
    for lane in &m.lanes {
        for r in 0..lane.row_count() {
            let Some(mut card) = lane.row_data(r) else { continue };
            let Some(app) = m.apps.iter().find(|a| a.exe == card.exe.as_str()) else { continue };
            let peak = app.pids.iter().filter_map(|p| levels.get(p)).fold(0f32, |a, b| a.max(*b));
            let target = db_of(peak).max(-90.0);
            let meter = m.app_meters.entry(app.exe.clone()).or_insert(AppMeter { db: -90.0, hold: -90.0, hold_at: now, at: now });
            let dt = (now - meter.at).as_secs_f32() * 1000.0;
            meter.db = target.max(meter.db - 0.026 * dt);
            meter.at = now;
            if target >= meter.hold {
                meter.hold = target;
                meter.hold_at = now;
            } else if now - meter.hold_at > PEAK_HOLD {
                meter.hold = (meter.hold - 1.5).max(meter.db);
            }
            let lit = (meter_fraction(meter.db) * APP_SEGMENTS).round() as i32;
            let peak_seg = (meter_fraction(meter.hold) * APP_SEGMENTS).round() as i32 - 1;
            let silent = meter.hold <= -60.0;
            let level = if silent { "−∞".to_string() } else { fmt_db(meter.hold) };
            if card.lit != lit || card.peak != peak_seg || card.silent != silent || card.level.as_str() != level {
                card.lit = lit;
                card.peak = peak_seg;
                card.silent = silent;
                card.level = level.into();
                lane.set_row_data(r, card);
            }
        }
    }
}

/// "CABLE-C Output" for the Virtual Mic: the capture side of the cable apps record from.
fn virtual_mic_name(devices: &[DeviceInfo], config: &Config) -> Option<String> {
    let sink = devices.iter().find(|d| Some(&d.id) == config.mic_sink.as_ref())?;
    devices
        .iter()
        .find(|d| d.flow == Flow::Capture && d.hardware == sink.hardware && !d.name.contains("16ch"))
        .map(|d| d.name.split(" (").next().unwrap_or(&d.name).to_string())
}

/// The Mic view's live parts: chain states, levels, the gate's meter and caption, the limiter and
/// the mic test.
fn update_mic(m: &mut Main, meters: &crate::engine::Meters, status: Option<&str>, config: &Config) {
    let mic = &config.mic;
    let mt = &meters.mic;
    let stopped = status.is_some_and(|s| s != "running");
    let off = stopped && status.is_some_and(device_missing);
    let tab = |id: &str, name: &str, state: String, kind: i32| ChainTab { id: id.into(), name: name.into(), state: state.into(), kind };
    let (ok, offk, warn) = (0, 1, 2);
    let db = |v: f32| fmt_db(v).trim_end_matches(" dB").to_string();
    let tabs = vec![
        if stopped {
            tab("input", "Mic input", if off { "Not connected".into() } else { "Stopped".into() }, if off { offk } else { warn })
        } else {
            tab("input", "Mic input", format!("{} dBFS", db(mt.input_db)), ok)
        },
        if !mic.denoise.enabled {
            tab("denoise", "Noise removal", "Off".into(), offk)
        } else if mt.denoise_ready {
            tab("denoise", "Noise removal", format!("−{:.0} dB noise", mt.noise_reduction_db.max(0.0)), ok)
        } else {
            tab("denoise", "Noise removal", "Loading model…".into(), warn)
        },
        if mic.gate.enabled {
            tab("gate", "Noise gate", if mt.gate_open { "Open".into() } else { "Closed".into() }, ok)
        } else {
            tab("gate", "Noise gate", "Off".into(), offk)
        },
        if mic.eq.enabled { tab("eq", "Equalizer", mic.eq.preset.clone(), ok) } else { tab("eq", "Equalizer", "Off".into(), offk) },
        if mic.compressor.enabled {
            tab("comp", "Compressor", fmt_db(mt.gain_reduction_db), ok)
        } else {
            tab("comp", "Compressor", "Off".into(), offk)
        },
        if !mic.limiter.enabled {
            tab("limiter", "Limiter", "Off".into(), offk)
        } else if mt.limiter_db < -0.05 {
            tab("limiter", "Limiter", fmt_db(mt.limiter_db), ok)
        } else {
            tab("limiter", "Limiter", "Idle".into(), ok)
        },
        if mic.muted {
            tab("output", "Virtual mic", "Muted".into(), warn)
        } else if stopped {
            tab("output", "Virtual mic", "Silent".into(), warn)
        } else {
            tab("output", "Virtual mic", format!("{} dBFS", db(mt.output_db)), ok)
        },
    ];
    for (i, t) in tabs.into_iter().enumerate() {
        if i < m.chain.row_count() {
            if m.chain.row_data(i).as_ref() != Some(&t) {
                m.chain.set_row_data(i, t);
            }
        } else {
            m.chain.push(t);
        }
    }

    let mut md = m.window.get_mic_data();
    md.in_pct = if stopped { 0.0 } else { meter_fraction(mt.input_db) };
    md.in_text = if stopped { "–".into() } else { fmt_db(mt.input_db).into() };
    md.out_pct = if stopped { 0.0 } else { meter_fraction(mt.output_db) };
    md.out_text = if stopped { "–".into() } else { fmt_db(mt.output_db).into() };
    md.gr_pct = (-mt.gain_reduction_db / 20.0).clamp(0.0, 1.0);
    md.gr_text = fmt_db(mt.gain_reduction_db).into();
    md.gate_level = if stopped { 0.0 } else { ((mt.input_db + 80.0) / 80.0).clamp(0.0, 1.0) };
    md.gate_caption = if stopped {
        "Mic not connected: nothing to measure".to_string()
    } else if !mic.gate.enabled {
        format!("Level {} · the gate is off", fmt_db(mt.input_db))
    } else if mt.gate_open {
        format!("Level {}, so the gate is open", fmt_db(mt.input_db))
    } else {
        format!("Room noise {}, so the gate is closed", fmt_db(mt.input_db))
    }
    .into();
    md.limiter_now = fmt_db(mt.limiter_db).into();
    let t = &meters.mic_test;
    let busy = t.phase != "idle";
    md.test_phase = t.phase.into();
    md.test_progress = t.progress;
    md.test_recorded = t.recorded;
    md.test_note = if mic.muted && !busy {
        "Your mic is muted. Unmute it to record a test.".to_string()
    } else if t.phase == "recording" {
        "Speak normally, the way you would in Discord.".to_string()
    } else if t.phase == "playing" {
        if t.original { "Playing your mic without any filters." } else { "Playing with every filter applied, as others hear you." }.to_string()
    } else if t.recorded {
        "Compare the two, or record again.".to_string()
    } else {
        "Play buttons unlock after recording.".to_string()
    }
    .into();
    m.window.set_mic_data(md);
}

/// The settings part of the Mic view's data.
fn fill_mic_settings(md: &mut MicData, mic: &crate::dsp::chain::MicSettings) {
    md.muted = mic.muted;
    md.listen = mic.monitor;
    md.denoise_on = mic.denoise.enabled;
    md.gate_on = mic.gate.enabled;
    md.eq_on = mic.eq.enabled;
    md.comp_on = mic.compressor.enabled;
    md.limiter_on = mic.limiter.enabled;
    md.strength = mic.denoise.strength_db;
    md.post_filter = mic.denoise.post_filter;
    md.low_latency = mic.denoise.low_latency;
    md.threshold = mic.gate.threshold_db;
    md.range = mic.gate.range_db;
    md.attack = mic.gate.attack_ms;
    md.hold = mic.gate.hold_ms;
    md.release = mic.gate.release_ms;
    md.comp_threshold = mic.compressor.threshold_db;
    md.ratio = mic.compressor.ratio;
    md.knee = mic.compressor.knee_db;
    md.makeup = mic.compressor.makeup_db;
    md.comp_attack = mic.compressor.attack_ms;
    md.comp_release = mic.compressor.release_ms;
    md.gain = mic.gain_db;
    md.gate_marker = meter_fraction(mic.gate.threshold_db);
}

/// Changes the mic settings, applies them, and shows the result without a full refresh (slider
/// drags call this many times a second).
fn edit_mic(app: &AppHandle, change: impl FnOnce(&mut crate::dsp::chain::MicSettings)) {
    let state = app.state();
    let mut mic = state.config.lock().mic.clone();
    change(&mut mic);
    state.set_mic_settings(mic.clone());
    notify_config_changed(app, "main-native");
    MAIN.with(|m| {
        if let Some(m) = m.borrow().as_ref() {
            let mut md = m.window.get_mic_data();
            fill_mic_settings(&mut md, &mic);
            m.window.set_mic_data(md);
            m.window.set_mic_eq(crate::eqedit::view(&mic.eq, DRAWER_GRAPH));
            // The Mixer's Mic strip: mute, gain and the chain count.
            let mut strip = m.window.get_mic();
            strip.muted = mic.muted;
            strip.volume = 10f32.powf(mic.gain_db / 20.0).clamp(0.0, MAX_VOLUME);
            strip.readout = fmt_db(mic.gain_db).into();
            m.window.set_mic(strip);
        }
    });
}

fn wire_mic(window: &MainWindow, app: &AppHandle) {
    let handle = app.clone();
    window.on_mic_set(move |key, v| {
        edit_mic(&handle, |mic| match key.as_str() {
            "strength" => mic.denoise.strength_db = v,
            "post_filter" => mic.denoise.post_filter = v,
            "threshold" => mic.gate.threshold_db = v,
            "range" => mic.gate.range_db = v,
            "attack" => mic.gate.attack_ms = v,
            "hold" => mic.gate.hold_ms = v,
            "release" => mic.gate.release_ms = v,
            "gate_defaults" => {
                mic.gate.attack_ms = 2.0;
                mic.gate.hold_ms = 150.0;
                mic.gate.release_ms = 120.0;
            }
            "comp_threshold" => mic.compressor.threshold_db = v,
            "ratio" => mic.compressor.ratio = v,
            "knee" => mic.compressor.knee_db = v,
            "makeup" => mic.compressor.makeup_db = v,
            "comp_attack" => mic.compressor.attack_ms = v,
            "comp_release" => mic.compressor.release_ms = v,
            "gain" => mic.gain_db = v,
            _ => {}
        })
    });
    let handle = app.clone();
    window.on_mic_toggle(move |key, on| {
        edit_mic(&handle, |mic| match key.as_str() {
            "denoise" => mic.denoise.enabled = on,
            "gate" => mic.gate.enabled = on,
            "eq" => mic.eq.enabled = on,
            "compressor" => mic.compressor.enabled = on,
            "limiter" => mic.limiter.enabled = on,
            "low_latency" => mic.denoise.low_latency = on,
            _ => {}
        })
    });
    let handle = app.clone();
    window.on_mic_mute(move || edit_mic(&handle, |mic| mic.muted = !mic.muted));
    let handle = app.clone();
    window.on_mic_listen(move || edit_mic(&handle, |mic| mic.monitor = !mic.monitor));
    let handle = app.clone();
    window.on_mic_test(move |action| {
        let state = handle.state();
        let engine = state.engine.lock();
        let Some(engine) = engine.as_ref() else { return };
        let result = match action.as_str() {
            "record" => engine.mic_test_record(),
            "play" => engine.mic_test_play(false),
            "play_original" => engine.mic_test_play(true),
            _ => {
                engine.mic_test_stop();
                Ok(())
            }
        };
        if let Err(e) = result {
            crate::append_log(&format!("mic test: {e:#}"));
        }
    });
    let weak = window.as_weak();
    let handle = app.clone();
    window.on_open_devices(move || {
        if let Some(w) = weak.upgrade() {
            w.set_settings_tab("devices".into());
            w.set_view("settings".into());
        }
        refresh(&handle);
    });
    let handle = app.clone();
    window.on_mic_eq_preset(move |name| edit_mic(&handle, |mic| crate::eqedit::apply_preset(&mut mic.eq, &name)));
    let handle = app.clone();
    window.on_mic_eq_pick(move |x, y, w, h, radius| {
        let eq = handle.state().config.lock().mic.eq.clone();
        crate::eqedit::pick(&eq, x, y, w, h, radius).map_or(-1, |i| i as i32)
    });
    let handle = app.clone();
    window.on_mic_eq_drag(move |i, x, y, w, h| edit_mic(&handle, |mic| crate::eqedit::drag(&mut mic.eq, i as usize, x, y, w, h)));
    let handle = app.clone();
    window.on_mic_eq_widen(move |i, narrower| edit_mic(&handle, |mic| crate::eqedit::widen(&mut mic.eq, i as usize, narrower)));
}

/// Meters at 20 fps: each strip's two sides with the mixer's peak hold, Buffer and Limit.
fn update_meters(app: &AppHandle) {
    let state = app.state();
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
        if m.window.get_view().as_str() == "apps" {
            update_app_levels(m);
        }
        if m.window.get_view().as_str() == "mic" {
            let status = state.engine.lock().as_ref().and_then(|e| e.shared.status.lock().get("Microphone").cloned());
            let config = state.config.lock().clone();
            update_mic(m, &meters, status.as_deref(), &config);
        }
        m.window.set_buffer(
            if meters.buffer_ms > 0.0 { format!("{} ms", meters.buffer_ms.round()) } else { "–".into() }.into(),
        );
    });
}
