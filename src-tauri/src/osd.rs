//! On-screen overlay in the top-right corner, confirming what a keyboard shortcut changed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

/// Window size in CSS pixels; the card inside leaves room for its shadow.
const SIZE: (f64, f64) = (320.0, 96.0);
/// Distance from the screen's top-right corner, in CSS pixels.
const MARGIN: f64 = 12.0;
/// How long the overlay stays up; the page fades out just before.
const VISIBLE: Duration = Duration::from_millis(1600);

#[derive(Serialize, Clone, Debug)]
pub struct Osd {
    /// Colour and tape: game, chat, media, aux, master, mic, output or system.
    pub group: &'static str,
    /// Tape text, e.g. "Game".
    pub tape: String,
    /// What changed, e.g. "Volume" or "Noise removal".
    pub label: String,
    /// The new value, e.g. "65 %", "On", "Muted".
    pub value: String,
    /// Bar fill from 0 to 1, for volumes and gain.
    pub level: Option<f32>,
    /// Where 100 % (or 0 dB) sits on the bar.
    pub unity: Option<f32>,
    /// Muted or switched off: drawn greyed out.
    pub dim: bool,
}

static LATEST: Mutex<Option<Osd>> = Mutex::new(None);
static HIDE_AT: Mutex<Option<Instant>> = Mutex::new(None);
static TIMER_RUNNING: AtomicBool = AtomicBool::new(false);

/// Shows the overlay (creating its window the first time) and hides it again shortly after.
/// Call off the UI thread; creating the window waits for it.
pub fn show(app: &AppHandle, osd: Osd) {
    *LATEST.lock() = Some(osd.clone());
    let window = match app.get_webview_window("osd") {
        Some(window) => {
            let _ = window.emit("osd", osd);
            window
        }
        // The page picks up LATEST through `take_osd` once it has loaded.
        None => match WebviewWindowBuilder::new(app, "osd", WebviewUrl::App("osd.html".into()))
            .title("Smowaudio overlay")
            .inner_size(SIZE.0, SIZE.1)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .resizable(false)
            .skip_taskbar(true)
            .always_on_top(true)
            // Never take focus from the game or app in front.
            .focusable(false)
            .focused(false)
            .visible(false)
            .build()
        {
            Ok(window) => {
                let _ = window.set_ignore_cursor_events(true);
                window
            }
            Err(e) => {
                crate::append_log(&format!("overlay window failed: {e}"));
                return;
            }
        },
    };

    // Already up (a held volume shortcut updates it ~40 times a second): just extend it.
    if !window.is_visible().unwrap_or(false) {
        // Top-right of the screen the mouse is on, which is where the user is looking.
        let monitor = app
            .cursor_position()
            .ok()
            .and_then(|p| window.monitor_from_point(p.x, p.y).ok().flatten())
            .or_else(|| window.primary_monitor().ok().flatten());
        if let Some(monitor) = monitor {
            let scale = monitor.scale_factor();
            let area = monitor.work_area();
            let _ = window.set_size(LogicalSize::new(SIZE.0, SIZE.1));
            let x = area.position.x as f64 + area.size.width as f64 - (SIZE.0 + MARGIN) * scale;
            let y = area.position.y as f64 + MARGIN * scale;
            let _ = window.set_position(PhysicalPosition::new(x, y));
        }
        let _ = window.show();
    }

    // Hide the window itself afterwards: a transparent topmost window left over a game can
    // stop it presenting directly to the screen, which costs latency. One timer thread at a time;
    // showing again only pushes its deadline back.
    let mut hide_at = HIDE_AT.lock();
    *hide_at = Some(Instant::now() + VISIBLE);
    if !TIMER_RUNNING.swap(true, Ordering::Relaxed) {
        let handle = app.clone();
        std::thread::spawn(move || loop {
            let due = HIDE_AT.lock().unwrap_or_else(Instant::now);
            let now = Instant::now();
            if now < due {
                std::thread::sleep(due - now);
                continue;
            }
            // Decide and hide under the lock, so a show() can't slip in between and be hidden
            // straight away. show() runs on the shortcut worker, never the UI thread.
            let mut hide_at = HIDE_AT.lock();
            if hide_at.is_some_and(|due| Instant::now() < due) {
                continue;
            }
            *hide_at = None;
            if let Some(window) = handle.get_webview_window("osd") {
                let _ = window.hide();
            }
            TIMER_RUNNING.store(false, Ordering::Relaxed);
            break;
        });
    }
}

/// The overlay to draw right after its window loads.
#[tauri::command]
pub fn take_osd() -> Option<Osd> {
    LATEST.lock().clone()
}
