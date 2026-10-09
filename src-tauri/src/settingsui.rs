//! The native main window's Settings view (ui-slint/settings.slint): device and cable choices,
//! stream health, the delay measurement, General's toggles, keyboard shortcuts (recorded from raw
//! key events, so the stored names are physical keys like "KeyM"), and Updates with the changelog.
//! Everything here runs on the Slint thread; slow work goes to a background thread and comes back
//! with `slint::invoke_from_event_loop`.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, VecModel};
use tauri::{AppHandle, Listener, Manager};

use crate::audio::device::DeviceInfo;
use crate::config::{Config, CHANNEL_COUNT, CHANNEL_NAMES};
use crate::flyout::device_missing;
use crate::ui::{
    ChangeEntry, DelayData, DelayRowData, DeviceRowData, HealthData, MainWindow, SelectOption, ShortcutGroup,
    ShortcutRowData, StreamItem, UpdatesData,
};
use crate::{audio, notify_config_changed, AppState};

/// How long a toast stays: errors longer, so there's time to read them.
const TOAST: Duration = Duration::from_millis(2400);
const TOAST_ERROR: Duration = Duration::from_millis(6000);
/// Several device changes in quick succession restart audio once.
const APPLY_AFTER: Duration = Duration::from_millis(400);
const CHANGELOG_SHOWN: usize = 5;

#[derive(Clone, PartialEq)]
enum RowStatus {
    Applying,
    Applied,
}

#[derive(Default)]
struct Settings {
    /// Device choices made here but not applied yet (they win over the saved config meanwhile).
    pending: BTreeMap<String, Option<String>>,
    rows: HashMap<String, RowStatus>,
    apply_timer: Option<slint::Timer>,
    /// Delay: the channel being measured (-1 while starting), the results and when.
    delay_running: Option<i32>,
    delay_results: Option<Vec<crate::DelayResult>>,
    delay_at: Option<Instant>,
    /// Shortcuts: the group shown, the action waiting for keys, a combo that clashes, and why the
    /// last key couldn't be used.
    group: String,
    recording: Option<String>,
    held: Vec<&'static str>,
    clash: Option<(String, String, String)>, // action, combo, the other action
    record_note: Option<(String, String)>,
    /// Updates.
    checking: bool,
    installing: bool,
    progress: Option<f32>,
    changelog_all: bool,
    toast_timer: Option<slint::Timer>,
    confirm: Option<Box<dyn FnOnce()>>,
    /// Streams failing at the last refresh: the health list opens when that goes up from 0.
    failing: i32,
    /// Lists updated in place, so an open dropdown survives the periodic refresh.
    models: Option<Models>,
}

struct Models {
    playback: Rc<VecModel<DeviceRowData>>,
    cables: Rc<VecModel<DeviceRowData>>,
    groups: Rc<VecModel<ShortcutGroup>>,
    rows: Rc<VecModel<ShortcutRowData>>,
}

/// Replaces a list's rows, touching only rows that changed.
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    use slint::Model;
    if model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (i, row) in rows.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&row) {
            model.set_row_data(i, row);
        }
    }
}

thread_local! {
    static SETTINGS: RefCell<Settings> = RefCell::new(Settings { group: "general".into(), ..Default::default() });
}

fn with<R>(f: impl FnOnce(&mut Settings) -> R) -> R {
    SETTINGS.with(|s| f(&mut s.borrow_mut()))
}

// ---------- toasts and confirmations ----------

/// Shows a short message at the bottom of the main window.
pub fn toast(window: &MainWindow, message: &str, error: bool) {
    window.set_toast(message.into());
    window.set_toast_error(error);
    let weak = window.as_weak();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::SingleShot, if error { TOAST_ERROR } else { TOAST }, move || {
        if let Some(w) = weak.upgrade() {
            w.set_toast("".into());
        }
    });
    with(|s| s.toast_timer = Some(timer));
}

fn confirm(window: &MainWindow, text: &str, action: &str, then: impl FnOnce() + 'static) {
    window.set_confirm_text(text.into());
    window.set_confirm_action(action.into());
    with(|s| s.confirm = Some(Box::new(then)));
}

// ---------- wiring ----------

pub fn wire(window: &MainWindow, app: &AppHandle) {
    let models = Models {
        playback: Rc::new(VecModel::default()),
        cables: Rc::new(VecModel::default()),
        groups: Rc::new(VecModel::default()),
        rows: Rc::new(VecModel::default()),
    };
    window.set_playback_rows(ModelRc::from(models.playback.clone()));
    window.set_cable_rows(ModelRc::from(models.cables.clone()));
    window.set_shortcut_groups(ModelRc::from(models.groups.clone()));
    window.set_shortcut_rows(ModelRc::from(models.rows.clone()));
    with(|s| s.models = Some(models));

    let weak = window.as_weak();
    window.on_confirm_accepted(move || {
        if let Some(w) = weak.upgrade() {
            w.set_confirm_text("".into());
        }
        if let Some(then) = with(|s| s.confirm.take()) {
            then();
        }
    });
    let weak = window.as_weak();
    window.on_confirm_dismissed(move || {
        if let Some(w) = weak.upgrade() {
            w.set_confirm_text("".into());
        }
        with(|s| s.confirm = None);
    });

    let handle = app.clone();
    window.on_settings_tab_changed(move |_| {
        stop_recording(&handle);
        refresh_now(&handle);
    });
    let handle = app.clone();
    window.on_pick_device(move |key, id| pick_device(&handle, key.to_string(), (!id.is_empty()).then(|| id.to_string())));
    let handle = app.clone();
    window.on_preference(move |name, on| {
        let state = handle.state::<AppState>();
        let playing = state.playing_output.lock().clone();
        state.update(|c| match name.as_str() {
            "master_per_output" => {
                c.master_per_output = on;
                if let (true, Some(id)) = (on, playing) {
                    c.output_masters.insert(id, crate::config::OutputMaster { volume: c.master.volume, eq: c.master.eq.clone() });
                }
            }
            "auto_update" => c.auto_update = on,
            _ => {}
        });
        if name.as_str() == "auto_update" && on {
            crate::updates::install_when_idle(&handle);
        }
        notify_config_changed(&handle, "main-native");
        refresh_now(&handle);
    });
    let handle = app.clone();
    window.on_retry(move || {
        let handle = handle.clone();
        std::thread::spawn(move || {
            let _com = audio::ComGuard::new();
            handle.state::<AppState>().restart_engine();
            // Streams report back within a moment of starting.
            std::thread::sleep(Duration::from_millis(1500));
            let _ = slint::invoke_from_event_loop(move || refresh_now(&handle));
        });
    });
    let handle = app.clone();
    window.on_measure(move || measure(&handle));
    let handle = app.clone();
    window.on_launch(move |on| {
        let handle = handle.clone();
        std::thread::spawn(move || {
            let result = crate::set_launch_at_login_now(&handle.state::<AppState>(), on);
            let _ = slint::invoke_from_event_loop(move || {
                if let Err(e) = result {
                    show_error(&e);
                }
                refresh_now(&handle);
            });
        });
    });
    let handle = app.clone();
    window.on_defaults(move |on| {
        let handle = handle.clone();
        std::thread::spawn(move || {
            let _com = audio::ComGuard::new();
            let result = crate::set_windows_defaults_enabled(&handle.state::<AppState>(), on);
            let _ = slint::invoke_from_event_loop(move || {
                match result {
                    Ok(()) => with_window(|w| {
                        toast(
                            w,
                            if on { "Game, Chat and Virtual Mic are now the Windows defaults" } else { "Your previous Windows defaults are restored" },
                            false,
                        )
                    }),
                    Err(e) => show_error(&e),
                }
                refresh_now(&handle);
            });
        });
    });
    window.on_open_log(|| {
        if let Some(dir) = std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("Smowaudio")) {
            let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
        }
    });

    // Shortcuts.
    let handle = app.clone();
    window.on_shortcut_group_changed(move |g| {
        stop_recording(&handle);
        with(|s| {
            s.group = g.to_string();
            s.clash = None;
        });
        refresh_now(&handle);
    });
    let handle = app.clone();
    window.on_record(move |action| start_recording(&handle, action.to_string()));
    let handle = app.clone();
    window.on_clear_shortcut(move |action| save_shortcut(&handle, action.to_string(), None));
    let handle = app.clone();
    window.on_use_here(move |action| {
        let combo = with(|s| s.clash.take()).filter(|(a, _, _)| *a == action.as_str()).map(|(_, combo, _)| combo);
        if let Some(combo) = combo {
            save_shortcut(&handle, action.to_string(), Some(combo));
        }
    });
    let handle = app.clone();
    window.on_volume_step_changed(move |step| {
        handle.state::<AppState>().update(|c| c.volume_step = step.clamp(0.01, 0.25));
        refresh_now(&handle);
    });
    let handle = app.clone();
    window.on_reset_shortcuts(move || {
        let count = handle.state::<AppState>().config.lock().hotkeys.len();
        if count == 0 {
            return;
        }
        let text = format!("Clear all {count} shortcut{}? This can't be undone.", if count == 1 { "" } else { "s" });
        let handle2 = handle.clone();
        with_window(|w| {
            confirm(w, &text, "Clear all", move || {
                stop_recording(&handle2);
                with(|s| s.clash = None);
                handle2.state::<AppState>().update(|c| c.hotkeys.clear());
                let errors = crate::hotkeys::register_all(&handle2);
                *handle2.state::<AppState>().hotkey_errors.lock() = errors;
                with_window(|w| toast(w, "All shortcuts cleared", false));
                refresh_now(&handle2);
            })
        });
    });

    // Updates.
    let handle = app.clone();
    window.on_check_updates(move || {
        with(|s| s.checking = true);
        refresh_now(&handle);
        let handle = handle.clone();
        std::thread::spawn(move || {
            let result = tauri::async_runtime::block_on(crate::updates::check(&handle));
            let _ = slint::invoke_from_event_loop(move || {
                with(|s| s.checking = false);
                with_window(|w| match &result {
                    Ok(Some(v)) => toast(w, &format!("Version {v} is available"), false),
                    Ok(None) => toast(w, "You're up to date", false),
                    Err(e) => toast(w, e, true),
                });
                refresh_now(&handle);
            });
        });
    });
    let handle = app.clone();
    window.on_install_update(move || {
        with(|s| {
            s.installing = true;
            s.progress = Some(0.0);
        });
        refresh_now(&handle);
        let handle = handle.clone();
        std::thread::spawn(move || {
            // On success the installer closes the app and starts the new version.
            let result = tauri::async_runtime::block_on(crate::updates::install(&handle));
            let _ = slint::invoke_from_event_loop(move || {
                with(|s| {
                    s.installing = false;
                    s.progress = None;
                });
                if let Err(e) = result {
                    show_error(&e);
                }
                refresh_now(&handle);
            });
        });
    });
    let handle = app.clone();
    window.on_more_changelog(move || {
        with(|s| s.changelog_all = !s.changelog_all);
        refresh_now(&handle);
    });

    // Download progress and status changes from the updater.
    let handle = app.clone();
    app.listen_any("update-progress", move |event| {
        let v: serde_json::Value = serde_json::from_str(event.payload()).unwrap_or_default();
        let (done, total) = (v["downloaded"].as_f64().unwrap_or(0.0), v["total"].as_f64().unwrap_or(0.0));
        if total > 0.0 {
            let handle = handle.clone();
            let _ = slint::invoke_from_event_loop(move || {
                with(|s| s.progress = Some((done / total) as f32));
                refresh_now(&handle);
            });
        }
    });
    let handle = app.clone();
    app.listen_any("update-status", move |_| {
        let handle = handle.clone();
        let _ = slint::invoke_from_event_loop(move || refresh_now(&handle));
    });
}

fn with_window(f: impl FnOnce(&MainWindow)) {
    if let Some(w) = crate::mainwin::window() {
        f(&w);
    }
}

fn show_error(message: &str) {
    with_window(|w| toast(w, message, true));
}

/// Re-reads everything Settings shows (also used by the main window's own refresh).
fn refresh_now(app: &AppHandle) {
    crate::mainwin::refresh_settings(app);
}

// ---------- devices ----------

/// "CABLE-C Input (VB-Audio Cable C)" -> "CABLE-C Output": the side apps use.
fn paired_side(devices: &[DeviceInfo], id: &Option<String>) -> Option<String> {
    let id = id.as_ref()?;
    let d = devices.iter().find(|d| &d.id == id)?;
    let base = d.name.split(" (").next().unwrap_or(&d.name).trim();
    Some(if let Some(b) = base.strip_suffix("Input") {
        format!("{b}Output")
    } else if let Some(b) = base.strip_suffix("Output") {
        format!("{b}Input")
    } else {
        base.to_string()
    })
}

fn current(config: &Config, key: &str) -> Option<String> {
    match key {
        "output" => config.output_device.clone(),
        "mic" => config.mic_device.clone(),
        "mic_sink" => config.mic_sink.clone(),
        k => k.strip_prefix("source").and_then(|i| i.parse::<usize>().ok()).and_then(|i| config.channels.get(i)?.source.clone()),
    }
}

fn set_current(config: &mut Config, key: &str, value: Option<String>) {
    match key {
        "output" => config.output_device = value,
        "mic" => config.mic_device = value,
        "mic_sink" => config.mic_sink = value,
        k => {
            if let Some(ch) = k.strip_prefix("source").and_then(|i| i.parse::<usize>().ok()).and_then(|i| config.channels.get_mut(i)) {
                ch.source = value;
            }
        }
    }
}

/// The device choices Settings shows: saved ones, overlaid with choices not applied yet.
fn effective(config: &Config) -> Config {
    let mut c = config.clone();
    with(|s| {
        for (k, v) in &s.pending {
            set_current(&mut c, k, v.clone());
        }
    });
    c
}

fn pick_device(app: &AppHandle, key: String, value: Option<String>) {
    with(|s| {
        s.pending.insert(key.clone(), value);
        s.rows.insert(key, RowStatus::Applying);
    });
    refresh_now(app);
    let handle = app.clone();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::SingleShot, APPLY_AFTER, move || apply_devices(&handle));
    with(|s| s.apply_timer = Some(timer));
}

fn apply_devices(app: &AppHandle) {
    let (pending, keys) = with(|s| {
        let pending = std::mem::take(&mut s.pending);
        let keys: Vec<String> = pending.keys().cloned().collect();
        (pending, keys)
    });
    if keys.is_empty() {
        return;
    }
    let mut config = app.state::<AppState>().config.lock().clone();
    for (k, v) in pending {
        set_current(&mut config, &k, v);
    }
    let handle = app.clone();
    std::thread::spawn(move || {
        let _com = audio::ComGuard::new();
        let sources = config.channels.iter().map(|c| c.source.clone()).collect();
        crate::set_devices_now(&handle.state::<AppState>(), config.output_device, config.mic_device, config.mic_sink, sources);
        notify_config_changed(&handle, "main-native");
        let _ = slint::invoke_from_event_loop(move || {
            with(|s| {
                for k in &keys {
                    s.rows.insert(k.clone(), RowStatus::Applied);
                }
            });
            refresh_now(&handle);
            // "✓ Applied" stays for a moment.
            let handle2 = handle.clone();
            slint::Timer::single_shot(Duration::from_millis(2500), move || {
                with(|s| s.rows.retain(|_, st| *st != RowStatus::Applied));
                refresh_now(&handle2);
            });
        });
    });
}

fn device_rows(config: &Config, render: &[DeviceInfo], capture: &[DeviceInfo], mic_status: Option<&str>) -> (Vec<DeviceRowData>, Vec<DeviceRowData>) {
    let config = effective(config);
    let physical = |list: &[DeviceInfo]| list.iter().filter(|d| !d.is_virtual()).cloned().collect::<Vec<_>>();
    let cables = |list: &[DeviceInfo]| list.iter().filter(|d| d.hardware.contains("VB-Audio")).cloned().collect::<Vec<_>>();
    let rows = with(|s| s.rows.clone());
    let row = |key: &str, label: &str, tape_kind: i32, list: Vec<DeviceInfo>, empty: &str, help: (String, String, String, i32)| {
        let selected = current(&config, key);
        let mut options = vec![SelectOption { id: "".into(), label: empty.into() }];
        options.extend(list.iter().map(|d| SelectOption { id: d.id.as_str().into(), label: d.name.as_str().into() }));
        let selected_label = match &selected {
            None => empty.to_string(),
            Some(id) => match list.iter().find(|d| &d.id == id) {
                Some(d) => d.name.clone(),
                None => {
                    // A device that's configured but unplugged still shows as selected.
                    options.insert(0, SelectOption { id: id.as_str().into(), label: "Unavailable device".into() });
                    "Unavailable device".into()
                }
            },
        };
        let status = rows.get(key);
        let (help, code, tail, kind) = match status {
            Some(RowStatus::Applying) => ("Applying…".to_string(), String::new(), String::new(), 3),
            Some(RowStatus::Applied) => ("✓ Applied · audio restarted".to_string(), String::new(), String::new(), 2),
            None => help,
        };
        DeviceRowData {
            key: key.into(),
            label: label.into(),
            tape_kind,
            options: ModelRc::from(Rc::new(VecModel::from(options))),
            selected: selected.unwrap_or_default().into(),
            selected_label: selected_label.into(),
            help: help.into(),
            code: code.into(),
            help_tail: tail.into(),
            help_kind: kind,
            busy: status == Some(&RowStatus::Applying),
            applied: status == Some(&RowStatus::Applied),
        }
    };
    let plain = |t: &str| (t.to_string(), String::new(), String::new(), 0);
    let mic_help = if mic_status.is_some_and(device_missing) {
        plain("Not connected right now. It starts by itself when the mic is switched on.")
    } else {
        plain("Filtered by the chain in the Mic tab, then sent to the Virtual Mic.")
    };
    let playback = vec![
        row("output", "Headphones / speakers", -1, physical(render), "Automatic (your usual default)", plain("Where every channel is mixed down to.")),
        row("mic", "Microphone", -1, physical(capture), "Automatic (your usual default)", mic_help),
    ];
    let mut cable_rows = vec![row(
        "mic_sink",
        "Virtual mic",
        5,
        cables(render),
        "None",
        match paired_side(render, &config.mic_sink) {
            Some(side) => ("Apps pick".into(), side, "as their mic".into(), 0),
            None => plain("No cable: apps have no Virtual Mic"),
        },
    )];
    for i in 0..CHANNEL_COUNT {
        let source = config.channels[i].source.clone();
        cable_rows.push(row(
            &format!("source{i}"),
            CHANNEL_NAMES[i],
            i as i32,
            cables(capture),
            "None",
            match paired_side(capture, &source) {
                Some(side) => ("Apps play into".into(), side, String::new(), 0),
                None => plain("No cable: this channel is off"),
            },
        ));
    }
    (playback, cable_rows)
}

// ---------- stream health ----------

const STREAM_ORDER: [&str; 9] =
    ["App routing", "Output", "Aux input", "Game input", "Chat input", "Media input", "Microphone", "Virtual mic", "Virtual mic listeners"];

fn health(status: &HashMap<String, String>) -> HealthData {
    let mut list: Vec<(&String, &String)> = status.iter().collect();
    let rank = |n: &str| STREAM_ORDER.iter().position(|o| *o == n).unwrap_or(STREAM_ORDER.len());
    list.sort_by(|a, b| rank(a.0).cmp(&rank(b.0)).then(a.0.cmp(b.0)));
    let items: Vec<StreamItem> = list
        .iter()
        .map(|(name, state)| {
            let ok = state.as_str() == "running";
            let off = !ok && name.as_str() == "Microphone" && device_missing(state);
            StreamItem { name: name.as_str().into(), kind: if ok { 0 } else if off { 1 } else { 2 }, reason: state.as_str().into() }
        })
        .collect();
    let bad: Vec<&StreamItem> = items.iter().filter(|s| s.kind == 2).collect();
    let (title, text) = if bad.is_empty() {
        let off = items.iter().filter(|s| s.kind == 1).count();
        let running = items.len() - off;
        (
            "Audio streams".to_string(),
            if items.is_empty() {
                " — starting".to_string()
            } else if off > 0 {
                format!(" — {running} running · mic not connected")
            } else {
                format!(" — all {running} running")
            },
        )
    } else {
        let reason = bad[0].reason.trim_end_matches(['.', ' ']).to_string();
        let mut sentence = reason.clone();
        if let Some(first) = sentence.get(0..1) {
            sentence = first.to_lowercase() + &sentence[1..];
        }
        let sentence = if device_missing(&reason) { format!("{sentence}. Plug one in or pick another below.") } else { format!("{sentence}.") };
        (
            if bad.len() == 1 { format!("{} stream stopped", bad[0].name) } else { format!("{} streams stopped", bad.len()) },
            format!(" — {sentence}"),
        )
    };
    let failing = bad.len() as i32;
    HealthData { failing, title: title.into(), text: text.into(), items: ModelRc::from(Rc::new(VecModel::from(items))) }
}

// ---------- delay ----------

fn measure(app: &AppHandle) {
    with(|s| s.delay_running = Some(-1));
    refresh_now(app);
    let handle = app.clone();
    std::thread::spawn(move || {
        let _com = audio::ComGuard::new();
        let progress_handle = handle.clone();
        let result = crate::measure_delay_now(&handle.state::<AppState>(), move |channel| {
            let h = progress_handle.clone();
            let _ = slint::invoke_from_event_loop(move || {
                with(|s| s.delay_running = Some(channel as i32));
                refresh_now(&h);
            });
        });
        let _ = slint::invoke_from_event_loop(move || {
            with(|s| s.delay_running = None);
            match result {
                Ok(results) => {
                    if results.is_empty() {
                        with_window(|w| toast(w, "No channel has a cable to measure. Pick cables above.", true));
                    }
                    with(|s| {
                        s.delay_results = Some(results);
                        s.delay_at = Some(Instant::now());
                    });
                }
                Err(e) => show_error(&e),
            }
            refresh_now(&handle);
        });
    });
}

/// "just now", "5 min ago", "2 h ago", "3 d ago".
pub fn ago(since: Instant) -> String {
    let s = since.elapsed().as_secs();
    if s < 60 {
        return "just now".into();
    }
    let m = (s as f64 / 60.0).round() as u64;
    if m < 60 {
        return format!("{m} min ago");
    }
    let h = (m as f64 / 60.0).round() as u64;
    if h < 24 { format!("{h} h ago") } else { format!("{} d ago", (h as f64 / 24.0).round()) }
}

fn delay() -> DelayData {
    with(|s| {
        let button = match s.delay_running {
            Some(c) if c >= 0 => format!("Measuring {}…", CHANNEL_NAMES[c as usize]),
            Some(_) => "Starting…".into(),
            None if s.delay_results.is_some() => "Measure again".into(),
            None => "Measure".into(),
        };
        let results = s.delay_results.clone().unwrap_or_default();
        let longest = results
            .iter()
            .filter_map(|r| Some(r.cable_ms? + r.engine_ms?))
            .fold(1.0f64, f64::max);
        let rows: Vec<DelayRowData> = results
            .iter()
            .map(|r| match (r.cable_ms, r.engine_ms, &r.error) {
                (Some(c), Some(e), None) => DelayRowData {
                    channel: r.channel as i32,
                    cable: (c / longest) as f32,
                    engine: (e / longest) as f32,
                    parts: format!("{} ms + {} ms", c.round(), e.round()).into(),
                    total: format!("{} ms", (c + e).round()).into(),
                    error: "".into(),
                },
                _ => DelayRowData {
                    channel: r.channel as i32,
                    error: r.error.clone().unwrap_or_else(|| "Couldn't measure".into()).into(),
                    ..Default::default()
                },
            })
            .collect();
        DelayData {
            running: s.delay_running.is_some(),
            button: button.into(),
            empty: if s.delay_running.is_some() { "Listening for the beeps…".into() } else { "Not measured yet.".into() },
            measured: s.delay_at.map(|t| format!("measured {}", ago(t))).unwrap_or_default().into(),
            rows: ModelRc::from(Rc::new(VecModel::from(rows))),
        }
    })
}

// ---------- shortcuts ----------

/// Groups and their actions; the ids must match hotkeys.rs.
fn shortcut_groups() -> Vec<(&'static str, &'static str, i32, Vec<(String, &'static str)>)> {
    let channel = |id: &str| -> Vec<(String, &'static str)> {
        vec![
            (format!("channel.{id}.volume_up"), "Volume up"),
            (format!("channel.{id}.volume_down"), "Volume down"),
            (format!("channel.{id}.mute"), "Mute on/off"),
            (format!("channel.{id}.eq"), "EQ on/off"),
        ]
    };
    let mut groups = vec![
        (
            "general",
            "General",
            -1,
            vec![
                ("app.mixer".to_string(), "Open the mixer"),
                ("app.flyout".to_string(), "Show or hide the tray flyout"),
                ("output.next".to_string(), "Next output device"),
                ("output.previous".to_string(), "Previous output device"),
                ("windows_defaults".to_string(), "Windows default devices on/off"),
            ],
        ),
        ("master", "Master", 4, channel("master")),
    ];
    for (i, name) in CHANNEL_NAMES.iter().enumerate() {
        let id: &'static str = match i {
            0 => "game",
            1 => "chat",
            2 => "media",
            _ => "aux",
        };
        groups.push((id, name, i as i32, channel(id)));
    }
    groups.push((
        "mic",
        "Mic",
        5,
        [
            ("mic.mute", "Mute on/off"),
            ("mic.push_to_talk", "Push to talk (hold)"),
            ("mic.push_to_mute", "Push to mute (hold)"),
            ("mic.gain_up", "Gain up 1 dB"),
            ("mic.gain_down", "Gain down 1 dB"),
            ("mic.monitor", "Listen to yourself on/off"),
            ("mic.denoise", "Noise removal on/off"),
            ("mic.low_latency", "Low-latency noise model on/off"),
            ("mic.gate", "Noise gate on/off"),
            ("mic.eq", "EQ on/off"),
            ("mic.compressor", "Compressor on/off"),
            ("mic.limiter", "Limiter on/off"),
        ]
        .into_iter()
        .map(|(a, l)| (a.to_string(), l))
        .collect(),
    ));
    groups
}

/// What's printed on the key, for a stored key name ("KeyM" -> "M", "ArrowUp" -> "↑").
pub fn key_name(part: &str) -> String {
    let named = match part {
        "Backquote" => "`",
        "Minus" => "-",
        "Equal" => "=",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        "Backslash" => "\\",
        "Semicolon" => ";",
        "Quote" => "'",
        "Comma" => ",",
        "Period" => ".",
        "Slash" => "/",
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        "ArrowLeft" => "←",
        "ArrowRight" => "→",
        "PageUp" => "Page Up",
        "PageDown" => "Page Down",
        "PrintScreen" => "Print Screen",
        "ScrollLock" => "Scroll Lock",
        "NumLock" => "Num Lock",
        "CapsLock" => "Caps Lock",
        "NumpadAdd" => "Num +",
        "NumpadSubtract" => "Num −",
        "NumpadMultiply" => "Num *",
        "NumpadDivide" => "Num /",
        "NumpadDecimal" => "Num .",
        "NumpadEnter" => "Num Enter",
        "NumpadEqual" => "Num =",
        "AudioVolumeUp" => "Volume Up",
        "AudioVolumeDown" => "Volume Down",
        "AudioVolumeMute" => "Volume Mute",
        "MediaPlayPause" => "Play/Pause",
        "MediaStop" => "Media Stop",
        "MediaTrackNext" => "Next Track",
        "MediaTrackPrevious" => "Previous Track",
        "Super" => "Win",
        _ => "",
    };
    if !named.is_empty() {
        return named.into();
    }
    if let Some(n) = part.strip_prefix("Numpad") {
        return format!("Num {n}");
    }
    part.trim_start_matches("Key").trim_start_matches("Digit").to_string()
}

fn keycaps(combo: &str) -> Vec<slint::SharedString> {
    combo.split('+').map(|k| key_name(k).into()).collect()
}

fn combo_text(combo: &str) -> String {
    combo.split('+').map(key_name).collect::<Vec<_>>().join(" + ")
}

fn label_of(action: &str) -> String {
    for (id, title, _, actions) in shortcut_groups() {
        if let Some((_, label)) = actions.iter().find(|(a, _)| a == action) {
            return if id == "general" { label.to_string() } else { format!("{title} → {label}") };
        }
    }
    action.to_string()
}

fn group_title_of(action: &str) -> String {
    shortcut_groups()
        .into_iter()
        .find(|(_, _, _, actions)| actions.iter().any(|(a, _)| a == action))
        .map(|(_, t, _, _)| t.to_string())
        .unwrap_or_default()
}

fn start_recording(app: &AppHandle, action: String) {
    // Bound shortcuts would otherwise fire instead of reaching the window.
    if with(|s| s.recording.is_none()) {
        crate::hotkeys::unregister_all(app);
    }
    with(|s| {
        s.recording = Some(action.clone());
        s.held.clear();
        s.record_note = None;
        if s.clash.as_ref().is_some_and(|(a, _, _)| *a == action) {
            s.clash = None;
        }
    });
    refresh_now(app);
}

/// Stops listening for keys and turns the shortcuts back on.
pub fn stop_recording(app: &AppHandle) {
    if with(|s| s.recording.take().is_some()) {
        with(|s| {
            s.record_note = None;
            s.held.clear();
        });
        let errors = crate::hotkeys::register_all(app);
        *app.state::<AppState>().hotkey_errors.lock() = errors;
        refresh_now(app);
    }
}

fn save_shortcut(app: &AppHandle, action: String, keys: Option<String>) {
    with(|s| {
        s.recording = None;
        s.record_note = None;
        s.held.clear();
    });
    let state = app.state::<AppState>();
    state.update(|c| match &keys {
        Some(keys) => {
            c.hotkeys.retain(|_, bound| bound != keys);
            c.hotkeys.insert(action.clone(), keys.clone());
        }
        None => {
            c.hotkeys.remove(&action);
        }
    });
    let errors = crate::hotkeys::register_all(app);
    *state.hotkey_errors.lock() = errors;
    refresh_now(app);
}

/// Keys the shortcut library can register, and keys fine without a modifier.
fn supported(code: &str) -> bool {
    let simple = [
        "Backquote", "Minus", "Equal", "BracketLeft", "BracketRight", "Backslash", "Semicolon", "Quote", "Comma", "Period",
        "Slash", "Space", "Tab", "Enter", "Backspace", "Delete", "Insert", "Home", "End", "PageUp", "PageDown",
        "PrintScreen", "ScrollLock", "Pause", "NumLock", "CapsLock", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
        "AudioVolumeUp", "AudioVolumeDown", "AudioVolumeMute", "MediaPlayPause", "MediaStop", "MediaTrackNext",
        "MediaTrackPrevious", "NumpadAdd", "NumpadSubtract", "NumpadMultiply", "NumpadDivide", "NumpadDecimal",
        "NumpadEnter", "NumpadEqual",
    ];
    if simple.contains(&code) {
        return true;
    }
    let tail = |p: &str| code.strip_prefix(p);
    if let Some(k) = tail("Key") {
        return k.len() == 1 && k.chars().all(|c| c.is_ascii_uppercase());
    }
    if let Some(d) = tail("Digit").or(tail("Numpad")) {
        return d.len() == 1 && d.chars().all(|c| c.is_ascii_digit());
    }
    if let Some(n) = tail("F") {
        return n.parse::<u32>().is_ok_and(|n| (1..=24).contains(&n));
    }
    false
}

fn bare_ok(code: &str) -> bool {
    code.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()).is_some_and(|n| (1..=24).contains(&n))
        || matches!(code, "Pause" | "ScrollLock" | "PrintScreen")
        || code.starts_with("AudioVolume")
        || code.starts_with("Media")
}

/// A key pressed in the main window while a shortcut is being recorded. `mods` are the held
/// modifiers ("Ctrl", "Alt", "Shift", "Super", in that order); `code` the physical key's name.
/// Returns true if the key was used (so Slint doesn't also act on it).
pub fn key_pressed(app: &AppHandle, mods: Vec<&'static str>, code: &str, is_modifier: bool) -> bool {
    let Some(action) = with(|s| s.recording.clone()) else { return false };
    if is_modifier {
        with(|s| s.held = mods);
        refresh_now(app);
        return true;
    }
    if mods.is_empty() && code == "Escape" {
        stop_recording(app);
        return true;
    }
    if mods.is_empty() && (code == "Backspace" || code == "Delete") {
        save_shortcut(app, action, None);
        return true;
    }
    let note = |text: &str| {
        with(|s| s.record_note = Some((action.clone(), text.to_string())));
        refresh_now(app);
    };
    if !supported(code) {
        note("That key can't be used for a shortcut. Try another.");
        return true;
    }
    if mods.is_empty() && !bare_ok(code) {
        note("Add Ctrl, Alt, Shift or Win to this key.");
        return true;
    }
    let combo = mods.iter().copied().chain(std::iter::once(code)).collect::<Vec<_>>().join("+");
    let hotkeys = app.state::<AppState>().config.lock().hotkeys.clone();
    if hotkeys.get(&action) == Some(&combo) {
        stop_recording(app);
        return true;
    }
    // A combo that's taken asks which action should keep it.
    if let Some(other) = hotkeys.iter().find(|(a, k)| **k == combo && **a != action).map(|(a, _)| a.clone()) {
        stop_recording(app);
        with(|s| s.clash = Some((action, combo, other)));
        refresh_now(app);
        return true;
    }
    save_shortcut(app, action, Some(combo));
    true
}

pub fn is_recording() -> bool {
    with(|s| s.recording.is_some())
}

fn shortcuts(window: &MainWindow, config: &Config, errors: &BTreeMap<String, String>) {
    let groups = shortcut_groups();
    let (current_group, recording, held, clash, note) =
        with(|s| (s.group.clone(), s.recording.clone(), s.held.clone(), s.clash.clone(), s.record_note.clone()));
    let flagged_group = clash.as_ref().map(|(_, _, other)| group_title_of(other));
    let group_models: Vec<ShortcutGroup> = groups
        .iter()
        .map(|(id, title, kind, actions)| ShortcutGroup {
            id: (*id).into(),
            title: (*title).into(),
            kind: *kind,
            assigned: actions.iter().filter(|(a, _)| config.hotkeys.contains_key(a)).count() as i32,
            count: actions.len() as i32,
            flagged: flagged_group.as_deref() == Some(*title),
        })
        .collect();
    let (_, group_title, _, actions) = groups.iter().find(|(id, ..)| *id == current_group).unwrap_or(&groups[0]);
    let rows: Vec<ShortcutRowData> = actions
        .iter()
        .map(|(action, label)| {
            let is_recording = recording.as_deref() == Some(action.as_str());
            let clash_here = clash.as_ref().filter(|(a, _, _)| a == action);
            let keys: Vec<slint::SharedString> = if is_recording {
                held.iter().map(|m| key_name(m).into()).collect()
            } else if let Some((_, combo, _)) = clash_here {
                keycaps(combo)
            } else {
                config.hotkeys.get(action).map(|k| keycaps(k)).unwrap_or_default()
            };
            let (note_text, kind) = if is_recording {
                match note.as_ref().filter(|(a, _)| a == action) {
                    Some((_, text)) => (text.clone(), 0),
                    None => ("Esc cancels · Backspace clears".to_string(), 0),
                }
            } else if let Some((_, combo, other)) = clash_here {
                (format!("{} is already {}. Only one can use it.", combo_text(combo), label_of(other)), 1)
            } else if let Some(e) = errors.get(action) {
                (e.clone(), 2)
            } else {
                (String::new(), 0)
            };
            ShortcutRowData {
                action: action.as_str().into(),
                label: (*label).into(),
                keys: ModelRc::from(Rc::new(VecModel::from(keys))),
                recording: is_recording,
                clash: clash_here.is_some(),
                note: note_text.into(),
                note_kind: kind,
                other: clash_here.map(|(_, _, o)| group_title_of(o)).unwrap_or_default().into(),
            }
        })
        .collect();
    with(|s| {
        if let Some(m) = &s.models {
            sync(&m.groups, group_models);
            sync(&m.rows, rows);
        }
    });
    window.set_shortcut_group(current_group.as_str().into());
    window.set_shortcut_group_title((*group_title).into());
    window.set_volume_step(config.volume_step);
    window.set_any_shortcuts(!config.hotkeys.is_empty());
}

// ---------- updates ----------

struct Change {
    version: String,
    date: Option<String>,
    items: Vec<String>,
}

/// The changelog's "## 0.9.4 — 2026-10-08" sections and their "- " items.
fn parse_changelog(md: &str) -> Vec<Change> {
    let mut entries: Vec<Change> = Vec::new();
    for line in md.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            let head = head.trim().trim_start_matches('v');
            let version: String = head.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            if version.split('.').count() == 3 {
                // The heading ends in the date: "0.9.4 — 2026-10-08".
                let date = head
                    .rsplit(char::is_whitespace)
                    .next()
                    .filter(|d| d.len() == 10 && d.chars().all(|c| c.is_ascii_digit() || c == '-'))
                    .map(str::to_string);
                entries.push(Change { version, date, items: Vec::new() });
            }
        } else if let Some(item) = line.trim_start().strip_prefix("- ").or(line.trim_start().strip_prefix("* ")) {
            if let Some(e) = entries.last_mut() {
                e.items.push(item.trim().to_string());
            }
        }
    }
    entries
}

fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |v: &str| v.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    parts(a).cmp(&parts(b))
}

/// "2026-10-08" -> "Oct 8, 2026".
fn fmt_date(iso: &str) -> String {
    let mut p = iso.split('-');
    let (Some(y), Some(m), Some(d)) = (p.next(), p.next(), p.next()) else { return iso.into() };
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let month = m.parse::<usize>().ok().and_then(|m| MONTHS.get(m.wrapping_sub(1))).copied().unwrap_or(m);
    format!("{month} {}, {y}", d.trim_start_matches('0'))
}

fn entry(c: &Change, badge: &str) -> ChangeEntry {
    let items: Vec<slint::SharedString> = c.items.iter().map(|i| i.as_str().into()).collect();
    ChangeEntry {
        version: c.version.as_str().into(),
        date: c.date.as_deref().map(fmt_date).unwrap_or_default().into(),
        badge: badge.into(),
        items: ModelRc::from(Rc::new(VecModel::from(items))),
    }
}

fn updates(app: &AppHandle, config: &Config) -> UpdatesData {
    let st = crate::updates::status(app);
    let (checking, installing, progress, all) = with(|s| (s.checking, s.installing, s.progress, s.changelog_all));
    let (state, kind) = if let Some(v) = &st.available {
        let auto = if config.auto_update { " · installs itself once this window is closed and nothing is playing" } else { "" };
        (format!("Version {v} is available{auto}"), 1)
    } else if let Some(e) = &st.error {
        (e.clone(), 2)
    } else if st.checked {
        let when = st.checked_at.map(|ms| {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
            ago(Instant::now() - Duration::from_millis(now.saturating_sub(ms)))
        });
        (format!("You're up to date{}", when.map(|w| format!(" · checked {w}")).unwrap_or_default()), 1)
    } else {
        ("Not checked yet".to_string(), 0)
    };
    let history = parse_changelog(crate::CHANGELOG);
    let shown: Vec<ChangeEntry> = history
        .iter()
        .take(if all { history.len() } else { CHANGELOG_SHOWN })
        .map(|c| entry(c, if c.version == st.current { "Installed" } else { "" }))
        .collect();
    let more_count = history.len().saturating_sub(CHANGELOG_SHOWN);
    let more = if all && more_count > 0 {
        "Show fewer".to_string()
    } else if more_count > 0 {
        format!("Show {more_count} older version{}", if more_count == 1 { "" } else { "s" })
    } else {
        String::new()
    };
    // What's new: the versions an update brings, from its notes (same format as the changelog).
    let (news, news_plain, news_title) = match &st.available {
        Some(v) => {
            let notes = st.notes.clone().unwrap_or_default();
            let newer: Vec<Change> = parse_changelog(&notes).into_iter().filter(|c| version_cmp(&c.version, &st.current).is_gt()).collect();
            let plain = if newer.is_empty() {
                notes.lines().filter(|l| !l.trim().is_empty() && !l.contains("Full Changelog")).collect::<Vec<_>>().join("\n")
            } else {
                String::new()
            };
            let title = if newer.len() > 1 { format!("What's new since {}", st.current) } else { format!("What's new in {v}") };
            (newer.iter().map(|c| entry(c, if &c.version == v { "New" } else { "" })).collect::<Vec<_>>(), plain, title)
        }
        None => (Vec::new(), String::new(), String::new()),
    };
    UpdatesData {
        version: st.current.as_str().into(),
        state: state.into(),
        state_kind: kind,
        available: st.available.is_some(),
        installing,
        install_text: match progress {
            Some(p) if installing && p >= 1.0 => "Installing… Smowaudio restarts in a moment".into(),
            Some(p) if installing => format!("Downloading… {} %", (p * 100.0).round()).into(),
            _ => "Install update".into(),
        },
        progress: if installing { progress.unwrap_or(0.0) } else { -1.0 },
        checking,
        auto: config.auto_update,
        news_title: news_title.into(),
        news: ModelRc::from(Rc::new(VecModel::from(news))),
        news_plain: news_plain.into(),
        history: ModelRc::from(Rc::new(VecModel::from(shown))),
        more: more.into(),
    }
}

/// Fills in the whole Settings view.
pub fn refresh(
    app: &AppHandle,
    window: &MainWindow,
    config: &Config,
    render: &[DeviceInfo],
    capture: &[DeviceInfo],
    status: &HashMap<String, String>,
) {
    let (playback, cables) = device_rows(config, render, capture, status.get("Microphone").map(String::as_str));
    with(|s| {
        if let Some(m) = &s.models {
            sync(&m.playback, playback);
            sync(&m.cables, cables);
        }
    });
    window.set_master_per_output(config.master_per_output);
    let h = health(status);
    // The health list opens when something stops and closes once all runs again; in between
    // it stays as the user left it.
    let before = with(|s| std::mem::replace(&mut s.failing, h.failing));
    if (before == 0) != (h.failing == 0) {
        window.set_health_expanded(h.failing > 0);
    }
    window.set_health(h);
    window.set_delay(delay());
    window.set_launch_at_login(config.launch_at_login);
    window.set_windows_defaults(config.set_windows_defaults);
    let errors = app.state::<AppState>().hotkey_errors.lock().clone();
    shortcuts(window, config, &errors);
    window.set_updates(updates(app, config));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changelog_sections_and_items_are_read() {
        let md = "# Changelog\n\nIntro.\n\n## 0.9.4 — 2026-10-08\n- Master slider\n\n## 0.9.3 — 2026-10-07\n- Reopen to move\n- Store names\n";
        let c = parse_changelog(md);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].version.as_str(), c[0].date.as_deref()), ("0.9.4", Some("2026-10-08")));
        assert_eq!(c[1].items, vec!["Reopen to move", "Store names"]);
        assert_eq!(fmt_date("2026-10-08"), "Oct 8, 2026");
        assert!(version_cmp("0.10.0", "0.9.4").is_gt());
    }

    #[test]
    fn shortcut_keys_are_named_like_the_old_page() {
        assert_eq!(combo_text("Ctrl+Alt+KeyM"), "Ctrl + Alt + M");
        assert_eq!(combo_text("Super+Digit1"), "Win + 1");
        assert_eq!(key_name("NumpadAdd"), "Num +");
        assert!(supported("F13") && supported("KeyQ") && !supported("Escape"));
        assert!(bare_ok("F5") && !bare_ok("KeyA"));
    }
}
