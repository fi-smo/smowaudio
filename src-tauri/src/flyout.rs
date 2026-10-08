//! The tray flyout: quick channel volumes, the output device and mic controls, drawn natively
//! with Slint (ui-slint/flyout.slint) instead of a WebView, so nothing heavier than Smowaudio
//! itself runs while the main window is closed.
//!
//! Slint runs its own event loop on a "Flyout" thread (winit allows that on Windows), next to
//! Tauri's on the main thread. Everything that touches the Slint window happens on that thread:
//! other threads hand work over with `slint::invoke_from_event_loop`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::winit_030::{winit, CustomApplicationHandler, EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, Model, ModelRc, PhysicalPosition, VecModel};
use tauri::{AppHandle, Listener, Manager};

use crate::audio::device::{self, Flow};
use crate::config::{CHANNEL_COUNT, CHANNEL_NAMES};
use crate::{audio, notify_config_changed, AppState};

use crate::ui::{Device, FlyoutWindow, Px, Row, Theme};

/// Width of the flyout, in logical pixels; its height follows its content.
const WIDTH: f32 = 320.0;
/// Gap between the flyout and the taskbar / screen edges, in logical pixels.
const MARGIN: f64 = 12.0;
/// How often the meters update while the flyout is open.
const METER_EVERY: Duration = Duration::from_millis(50);
/// Peak marks hold this long before they start falling.
const PEAK_HOLD: Duration = Duration::from_millis(900);

thread_local! {
    /// The flyout window and its meter state; only ever touched on the Flyout thread.
    static FLYOUT: RefCell<Option<Flyout>> = const { RefCell::new(None) };
}

struct Flyout {
    window: FlyoutWindow,
    rows: Rc<VecModel<Row>>,
    devices: Rc<VecModel<Device>>,
    /// Per row: peak hold (0..1) and when it was last raised.
    holds: Vec<(f32, Instant)>,
    meter_timer: slint::Timer,
}

/// Where to put the flyout: the point it hangs above (the tray click, or the tray icon), and the
/// work area and scale of the screen that point is on. Worked out on the main thread, where Tauri
/// answers monitor questions.
#[derive(Clone, Copy)]
pub struct Anchor {
    pub x: f64,
    pub y: f64,
    /// Work area: left, top, right, bottom, in physical pixels.
    pub area: Option<(f64, f64, f64, f64)>,
    pub scale: f64,
}

/// Starts the Flyout thread and its Slint event loop. The window is created hidden.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    let spawned = std::thread::Builder::new().name("Flyout".into()).spawn(move || {
        if let Err(e) = run(app) {
            crate::append_log(&format!("flyout unavailable: {e}"));
        }
    });
    if let Err(e) = spawned {
        crate::append_log(&format!("flyout thread failed to start: {e}"));
    }
}

fn run(app: AppHandle) -> Result<(), slint::PlatformError> {
    // Device names and the output list need COM on this thread.
    let _com = audio::ComGuard::new();
    slint::BackendSelector::new()
        .backend_name("winit".into())
        // A small window that changes rarely: the software renderer needs no GPU context.
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(|attributes| {
            use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
            attributes
                .with_decorations(false)
                .with_resizable(false)
                .with_skip_taskbar(true)
                // Windows 11 rounds the corners; no drop shadow (it darkens a wide area around
                // the flyout) and no system border (the flyout draws its own).
                .with_undecorated_shadow(false)
                .with_corner_preference(CornerPreference::Round)
                .with_border_color(None)
                // Drag and drop needs OLE's single-threaded COM, but this thread uses the
                // multithreaded kind for the device list; the flyout takes no drops anyway.
                .with_drag_and_drop(false)
        })
        .with_winit_custom_application_handler(FocusWatcher { app: app.clone() })
        .select()?;

    let window = FlyoutWindow::new()?;
    window.global::<Theme>().set_dark(!crate::apps_use_light_theme());
    let rows = Rc::new(VecModel::<Row>::default());
    let devices = Rc::new(VecModel::<Device>::default());
    window.set_rows(ModelRc::from(rows.clone()));
    window.set_devices(ModelRc::from(devices.clone()));
    wire_callbacks(&window, &app);

    FLYOUT.with(|f| {
        *f.borrow_mut() = Some(Flyout {
            window,
            rows,
            devices,
            holds: vec![(0.0, Instant::now()); CHANNEL_COUNT + 1],
            meter_timer: slint::Timer::default(),
        })
    });

    // Settings changed elsewhere (main window, shortcuts): show them if the flyout is open.
    let handle = app.clone();
    app.listen_any("config-changed", move |event| {
        // The flyout's own changes are already on screen.
        if event.payload().contains("\"flyout\"") {
            return;
        }
        let handle = handle.clone();
        let _ = slint::invoke_from_event_loop(move || refresh_if_visible(&handle));
    });

    slint::run_event_loop_until_quit()
}

/// Hides the flyout when it loses focus, like Windows' own tray flyouts.
struct FocusWatcher {
    app: AppHandle,
}

impl CustomApplicationHandler for FocusWatcher {
    fn window_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _winit_window: Option<&winit::window::Window>,
        _slint_window: Option<&slint::Window>,
        event: &winit::event::WindowEvent,
    ) -> EventResult {
        if let winit::event::WindowEvent::Focused(false) = event {
            hide();
            *self.app.state::<AppState>().flyout_hidden_at.lock() = Some(Instant::now());
        }
        EventResult::Propagate
    }
}

fn wire_callbacks(window: &FlyoutWindow, app: &AppHandle) {
    let handle = app.clone();
    window.on_set_volume(move |row, volume| {
        update_row(&handle, row as usize, |s| s.volume = volume);
        // Show it right away: while dragging the fader shows its own value, but a change from
        // the keyboard, the scroll wheel or a screen reader only shows once the row has it.
        FLYOUT.with(|f| {
            if let Some(f) = f.borrow().as_ref() {
                if let Some(mut r) = f.rows.row_data(row as usize) {
                    r.volume = volume;
                    f.rows.set_row_data(row as usize, r);
                }
            }
        });
    });
    let handle = app.clone();
    window.on_toggle_mute(move |row| {
        update_row(&handle, row as usize, |s| s.muted = !s.muted);
        refresh(&handle);
    });
    let handle = app.clone();
    window.on_choose_output(move |id| {
        let output = (!id.is_empty()).then(|| id.to_string());
        let handle = handle.clone();
        // Reopening the output stream takes a moment: off this thread, like the old command.
        std::thread::spawn(move || {
            let _com = audio::ComGuard::new();
            handle.state::<AppState>().set_output(output);
            notify_config_changed(&handle, "flyout");
            let _ = slint::invoke_from_event_loop(move || refresh(&handle));
        });
    });
    let handle = app.clone();
    window.on_toggle_mic_mute(move || {
        let state = handle.state::<AppState>();
        let mut mic = state.config.lock().mic.clone();
        mic.muted = !mic.muted;
        state.set_mic_settings(mic);
        notify_config_changed(&handle, "flyout");
        refresh(&handle);
    });
    let handle = app.clone();
    window.on_toggle_mic_listen(move || {
        let state = handle.state::<AppState>();
        let mut mic = state.config.lock().mic.clone();
        mic.monitor = !mic.monitor;
        state.set_mic_settings(mic);
        notify_config_changed(&handle, "flyout");
        refresh(&handle);
    });
    let handle = app.clone();
    window.on_open_main(move |view| {
        hide();
        let handle = handle.clone();
        let view = view.to_string();
        // Creating the main window from a non-async context can deadlock WebView2; a plain thread
        // is what the old async command amounted to.
        std::thread::spawn(move || crate::show_main(&handle, Some(&view)));
    });
    window.on_dismiss(hide);
}

/// Changes one row's settings (0..3 channels, 4 Master) and tells the rest of the app.
fn update_row(app: &AppHandle, row: usize, change: impl FnOnce(&mut crate::dsp::chain::ChannelSettings)) {
    let state = app.state::<AppState>();
    if row < CHANNEL_COUNT {
        let mut settings = state.config.lock().channels[row].settings.clone();
        change(&mut settings);
        state.set_channel_settings(row, settings);
    } else {
        let mut settings = state.config.lock().master.clone();
        change(&mut settings);
        state.set_master_settings(settings);
    }
    notify_config_changed(app, "flyout");
}

/// Shows the flyout above `anchor`, or hides it if it's open. Call from any thread.
pub fn toggle(app: &AppHandle, anchor: Anchor) {
    let app = app.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let open = FLYOUT.with(|f| f.borrow().as_ref().is_some_and(|f| f.window.window().is_visible()));
        if open {
            hide();
        } else {
            show(&app, anchor);
        }
    });
}

/// Hides the flyout. Call from any thread.
pub fn hide_from_any_thread() {
    let _ = slint::invoke_from_event_loop(hide);
}

fn hide() {
    FLYOUT.with(|f| {
        if let Some(f) = f.borrow_mut().as_mut() {
            f.window.set_outputs_open(false);
            f.meter_timer.stop();
            let _ = f.window.hide();
        }
    });
}

fn show(app: &AppHandle, anchor: Anchor) {
    refresh(app);
    FLYOUT.with(|f| {
        let mut f = f.borrow_mut();
        let Some(f) = f.as_mut() else { return };
        f.window.global::<Theme>().set_dark(!crate::apps_use_light_theme());
        f.window.global::<Px>().set_scale(anchor.scale as f32);
        // Place it from its layout's size before it appears, then again from the window's real
        // size: the first show lays out the rows just filled in, so only then is the height final.
        let estimate = (WIDTH as f64 * anchor.scale, f.window.get_wanted_height() as f64 * anchor.scale);
        place(&f.window, anchor, estimate);
        if f.window.show().is_ok() {
            let size = f.window.window().size();
            place(&f.window, anchor, (size.width as f64, size.height as f64));
            // Take focus, so the scroll wheel works over the sliders straight away and clicking
            // elsewhere hides it again. winit only asks Windows for focus once the window is
            // visible, which it isn't until `show` has run its course, so ask a moment later.
            slint::Timer::single_shot(Duration::from_millis(30), || focus(true));
        }
        let handle = app.clone();
        f.meter_timer.start(slint::TimerMode::Repeated, METER_EVERY, move || update_meters(&handle));
    });
}

/// Makes the open flyout the foreground window. If Windows refused (it only lets the app that the
/// user just interacted with take focus), tries once more and leaves a line in the log.
fn focus(retry: bool) {
    let Some(hwnd) = FLYOUT.with(|f| {
        let f = f.borrow();
        let f = f.as_ref()?;
        if !f.window.window().is_visible() {
            return None;
        }
        f.window.window().with_winit_window(|w: &winit::window::Window| {
            w.focus_window();
            hwnd_of(w)
        })?
    }) else {
        return;
    };
    slint::Timer::single_shot(Duration::from_millis(150), move || {
        let foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.0 as isize;
        if foreground == hwnd {
            return;
        }
        if retry {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(
                    windows::Win32::Foundation::HWND(hwnd as *mut _),
                );
            }
            focus(false);
        } else {
            crate::append_log("flyout: Windows didn't give it focus, so the scroll wheel needs a click first");
        }
    });
}

fn hwnd_of(window: &winit::window::Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// Puts the flyout (`size` in physical pixels) above the anchor, inside the screen's work area.
fn place(window: &FlyoutWindow, anchor: Anchor, (width, height): (f64, f64)) {
    let margin = MARGIN * anchor.scale;
    let mut x = anchor.x - width / 2.0;
    let mut y = anchor.y - height - margin;
    if let Some((left, top, right, bottom)) = anchor.area {
        x = x.clamp(left + margin, (right - width - margin).max(left + margin));
        y = (bottom - height - margin).max(top + margin);
    }
    window.window().set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

fn refresh_if_visible(app: &AppHandle) {
    let visible = FLYOUT.with(|f| f.borrow().as_ref().is_some_and(|f| f.window.window().is_visible()));
    if visible {
        refresh(app);
    }
}

/// Fills in everything but the meters from the current settings, devices and stream status.
fn refresh(app: &AppHandle) {
    let state = app.state::<AppState>();
    let config = state.config.lock().clone();
    let status = state.engine.lock().as_ref().map(|e| e.shared.status.lock().clone()).unwrap_or_default();
    let render = device::list(Flow::Render).unwrap_or_default();
    let active_mic = device::resolve_physical(Flow::Capture, config.mic_device.as_deref(), config.previous_default(Flow::Capture).as_deref())
        .ok()
        .and_then(|d| device::friendly_name(&d).ok());

    FLYOUT.with(|f| {
        let f = f.borrow();
        let Some(f) = f.as_ref() else { return };
        let w = &f.window;

        // Rows: the channels, then Master. Levels are left to the meter timer.
        let settings = config.channels.iter().map(|c| &c.settings).chain(std::iter::once(&config.master));
        for (i, (s, name)) in settings.zip(CHANNEL_NAMES.iter().copied().chain(["Master"])).enumerate() {
            let old = f.rows.row_data(i);
            let row = Row {
                name: name.into(),
                volume: s.volume,
                muted: s.muted,
                level: old.as_ref().map_or(0.0, |r| r.level),
                peak: old.as_ref().map_or(0.0, |r| r.peak),
            };
            if i < f.rows.row_count() {
                f.rows.set_row_data(i, row);
            } else {
                f.rows.push(row);
            }
        }

        // Output: Automatic, then the physical devices.
        let physical: Vec<_> = render.iter().filter(|d| !d.is_virtual()).collect();
        let current = config.output_device.as_deref();
        let selected = physical.iter().find(|d| Some(d.id.as_str()) == current);
        w.set_output_label(selected.map_or("Automatic".into(), |d| short_name(&d.name)).into());
        let mut list = vec![Device {
            id: "".into(),
            label: "Automatic".into(),
            title: "Your usual default headphones or speakers".into(),
            selected: current.is_none(),
        }];
        list.extend(physical.iter().map(|d| Device {
            id: d.id.as_str().into(),
            label: short_name(&d.name).into(),
            title: d.name.as_str().into(),
            selected: Some(d.id.as_str()) == current,
        }));
        f.devices.set_vec(list);

        // Status pill: a mic that's switched off isn't a problem; the mic panel says so.
        let mut bad: Vec<&String> = status
            .iter()
            .filter(|(k, v)| v.as_str() != "running" && !(k.as_str() == "Microphone" && device_missing(v)))
            .map(|(k, _)| k)
            .collect();
        bad.sort();
        w.set_status_warn(!bad.is_empty());
        w.set_status_text(
            match bad.as_slice() {
                [] => "All running".to_string(),
                [one] if one.as_str() == "Microphone" => "Mic stopped".to_string(),
                [one] => format!("{one} stopped"),
                many => format!("{} streams stopped", many.len()),
            }
            .into(),
        );

        // Mic panel.
        let mic_state = status.get("Microphone");
        let stopped = mic_state.is_some_and(|s| s != "running");
        let off = stopped && mic_state.is_some_and(|s| device_missing(s));
        let name = active_mic.as_deref().map_or("Automatic".to_string(), short_name);
        w.set_mic_text(
            match mic_state {
                None => "No Virtual Mic cable set up".to_string(),
                Some(_) if !stopped => format!("{} · {name}", if config.mic.muted { "Muted" } else { "Live" }),
                Some(_) if off => "Off · not connected".to_string(),
                Some(_) => "Mic stopped".to_string(),
            }
            .into(),
        );
        w.set_mic_failed(stopped && !off);
        w.set_mic_muted(config.mic.muted);
        w.set_mic_listen(config.mic.monitor);
    });
}

/// Meters at 20 fps while open: each row's louder side, with the mixer's peak hold.
fn update_meters(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(meters) = state.engine.lock().as_ref().map(|e| e.meters()) else { return };
    let (mic_running, mic_muted) = {
        let status = state.engine.lock().as_ref().map(|e| e.shared.status.lock().get("Microphone").cloned()).flatten();
        (status.as_deref() == Some("running"), state.config.lock().mic.muted)
    };
    let now = Instant::now();
    FLYOUT.with(|f| {
        let mut f = f.borrow_mut();
        let Some(f) = f.as_mut() else { return };
        let levels = meters.channels.iter().chain(std::iter::once(&meters.master));
        for (i, pair) in levels.enumerate() {
            let Some(mut row) = f.rows.row_data(i) else { continue };
            let level = if row.muted { 0.0 } else { meter_fraction(pair[0].max(pair[1])) };
            let (hold, at) = &mut f.holds[i];
            if level >= *hold {
                *hold = level;
                *at = now;
            } else if now - *at > PEAK_HOLD {
                *hold = (*hold - 0.025).max(level);
            }
            if (row.level - level).abs() > 0.001 || (row.peak - *hold).abs() > 0.001 {
                row.level = level;
                row.peak = *hold;
                f.rows.set_row_data(i, row);
            }
        }
        let mic = if mic_running && !mic_muted { meter_fraction(meters.mic.output_db) } else { 0.0 };
        f.window.set_mic_level(mic);
    });
}

/// dBFS to a meter fraction: -60 dB and below is empty, 0 dB full.
pub(crate) fn meter_fraction(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

pub(crate) fn device_missing(reason: &str) -> bool {
    reason.to_lowercase().contains("no physical audio device")
}

/// "Speakers (5- soundcore Select 4 Go )" -> "soundcore Select 4 Go", like the main window.
pub(crate) fn short_name(name: &str) -> String {
    let trimmed = name.trim_end();
    if let (Some(open), true) = (trimmed.find('('), trimmed.ends_with(')')) {
        let inner = trimmed[open + 1..trimmed.len() - 1].trim();
        // Drop Windows' "5- " numbering of duplicate devices.
        let inner = match inner.split_once("- ") {
            Some((n, rest)) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => rest.trim(),
            _ => inner,
        };
        if !inner.is_empty() {
            return inner.to_string();
        }
        return trimmed[..open].trim().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_are_shortened_like_the_main_window() {
        assert_eq!(short_name("Speakers (5- soundcore Select 4 Go )"), "soundcore Select 4 Go");
        assert_eq!(short_name("Headphones (Arctis Nova Pro Wireless)"), "Arctis Nova Pro Wireless");
        assert_eq!(short_name("Speakers"), "Speakers");
    }
}
