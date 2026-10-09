//! On-screen overlay in the top-right corner, confirming what a keyboard shortcut changed. A
//! Slint window (ui-slint/osd.slint) on the Slint thread that never takes focus or clicks.

use std::cell::RefCell;
use std::time::Duration;

use serde::Serialize;
use slint::winit_030::{winit, WinitWindowAccessor};
use slint::{ComponentHandle, PhysicalPosition};
use tauri::AppHandle;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, WS_EX_NOACTIVATE,
};

use crate::flyout::{create_window, hwnd_of, WindowKind};
use crate::ui::{OsdWindow, Theme};

/// Window size in logical pixels; the card inside leaves room for its shadow.
const SIZE: (f64, f64) = (320.0, 96.0);
/// Distance from the screen's top-right corner, in logical pixels.
const MARGIN: f64 = 12.0;
/// How long the overlay stays up, and when it starts fading out.
const VISIBLE: Duration = Duration::from_millis(1600);
const FADE_AFTER: Duration = Duration::from_millis(1300);

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

struct Overlay {
    window: OsdWindow,
    hwnd: Option<isize>,
    /// On screen right now (the window is shown and hidden behind Slint's back).
    up: bool,
    fade: slint::Timer,
    hide: slint::Timer,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

/// Shows the overlay and hides it again shortly after. Call from any thread; a held volume
/// shortcut calls this ~40 times a second, which only updates it and pushes the hiding back.
pub fn show(_app: &AppHandle, osd: Osd) {
    let _ = slint::invoke_from_event_loop(move || show_now(osd));
}

fn show_now(osd: Osd) {
    OVERLAY.with(|o| {
        let mut o = o.borrow_mut();
        if o.is_none() {
            match create_window(WindowKind::Overlay, OsdWindow::new) {
                Ok(window) => {
                    *o = Some(Overlay {
                        window,
                        hwnd: None,
                        up: false,
                        fade: slint::Timer::default(),
                        hide: slint::Timer::default(),
                    })
                }
                Err(e) => {
                    crate::append_log(&format!("overlay window failed: {e}"));
                    return;
                }
            }
        }
        let Some(o) = o.as_mut() else { return };
        let w = &o.window;
        w.global::<Theme>().set_dark(!crate::apps_use_light_theme());
        w.set_group(osd.group.into());
        w.set_tape(osd.tape.into());
        w.set_label(osd.label.into());
        w.set_value(osd.value.into());
        w.set_level(osd.level.map_or(-1.0, |l| l.clamp(0.0, 1.0)));
        w.set_unity(osd.unity.unwrap_or(-1.0));
        w.set_dim(osd.dim);

        if !o.up {
            // Top-right of the screen the mouse is on, which is where the user is looking.
            if let Some((right, top, scale)) = cursor_work_area() {
                let x = right - ((SIZE.0 + MARGIN) * scale).round() as i32;
                let y = top + (MARGIN * scale).round() as i32;
                w.window().set_position(PhysicalPosition::new(x, y));
            }
            match o.hwnd {
                // Never activated: the game or app in front keeps focus.
                Some(hwnd) => unsafe {
                    let hwnd = HWND(hwnd as *mut _);
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                },
                None => {
                    // The first show doesn't activate it (see the window attributes); after
                    // that it's shown with Win32 directly, which Slint doesn't need to know.
                    if let Err(e) = w.show() {
                        crate::append_log(&format!("overlay didn't open: {e}"));
                        return;
                    }
                    o.hwnd = w.window().with_winit_window(|win: &winit::window::Window| {
                        // Clicks go through to whatever is underneath.
                        let _ = win.set_cursor_hittest(false);
                        hwnd_of(win)
                    })
                    .flatten();
                    if let Some(hwnd) = o.hwnd {
                        unsafe {
                            let hwnd = HWND(hwnd as *mut _);
                            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_NOACTIVATE.0 as isize);
                        }
                    }
                }
            }
            o.up = true;
        }
        w.set_shown(true);

        let weak = w.as_weak();
        o.fade.start(slint::TimerMode::SingleShot, FADE_AFTER, move || {
            if let Some(w) = weak.upgrade() {
                w.set_shown(false);
            }
        });
        // Hide the window itself afterwards: a transparent topmost window left over a game can
        // stop it presenting directly to the screen, which costs latency.
        o.hide.start(slint::TimerMode::SingleShot, VISIBLE, hide);
    });
}

fn hide() {
    OVERLAY.with(|o| {
        if let Some(o) = o.borrow_mut().as_mut() {
            if let Some(hwnd) = o.hwnd {
                unsafe {
                    let _ = ShowWindow(HWND(hwnd as *mut _), SW_HIDE);
                }
            }
            o.up = false;
        }
    });
}

/// The right and top edges of the work area of the screen the mouse is on (physical pixels), and
/// that screen's scale.
fn cursor_work_area() -> Option<(i32, i32, f64)> {
    unsafe {
        let mut point = POINT::default();
        GetCursorPos(&mut point).ok()?;
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        Some((info.rcWork.right, info.rcWork.top, dpi_x as f64 / 96.0))
    }
}
