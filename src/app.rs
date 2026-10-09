//! The app around the audio engine: the Slint event loop on the main thread, the tray icon and its
//! menu, and the hooks every native window shares (window attributes, focus, keys for Settings).
//!
//! Everything that touches a window happens on the main thread; other threads hand work over
//! with `slint::invoke_from_event_loop`.

use std::time::{Duration, Instant};

use slint::winit_030::{winit, CustomApplicationHandler, EventResult, WinitWindowAccessor};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use crate::{append_log, AppState};

/// A handle to the running app, passed to whatever needs its state or windows. Free to copy.
#[derive(Clone, Copy, Default)]
pub struct AppHandle;

impl AppHandle {
    pub fn state(&self) -> &'static AppState {
        crate::state()
    }

    /// Quits: ends the event loop, after which `main` saves the settings and stops the audio.
    pub fn exit(&self) {
        let _ = slint::quit_event_loop();
    }
}

/// Which window is being created next: Slint asks for its window attributes before it knows.
#[derive(Clone, Copy)]
pub enum WindowKind {
    Flyout,
    Main,
    Overlay,
}

thread_local! {
    static NEXT_WINDOW: std::cell::Cell<WindowKind> = const { std::cell::Cell::new(WindowKind::Flyout) };
    static TRAY: std::cell::RefCell<Option<TrayIcon>> = const { std::cell::RefCell::new(None) };
}

/// Creates a window of the given kind (`create` calls its `::new()`).
pub fn create_window<T>(kind: WindowKind, create: impl FnOnce() -> T) -> T {
    NEXT_WINDOW.set(kind);
    let window = create();
    NEXT_WINDOW.set(WindowKind::Flyout);
    window
}

const TRAY_ICON: &[u8] = include_bytes!("../icons/tray.png");
/// The same bars with a red "off" badge, while the mic is muted.
const TRAY_ICON_MUTED: &[u8] = include_bytes!("../icons/tray-muted.png");

/// Runs the app on this (the main) thread until "Quit". `ready` runs once the event loop is set
/// up, to start what needs it.
pub fn run(app: AppHandle, ready: impl FnOnce() + 'static) -> Result<(), slint::PlatformError> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        // Small windows that change rarely: the software renderer needs no GPU context.
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(|attributes| {
            use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
            // Drag and drop needs OLE's single-threaded COM, but this thread uses the
            // multithreaded kind for the audio devices; the Apps view drags on its own.
            let attributes = attributes.with_drag_and_drop(false);
            match NEXT_WINDOW.get() {
                WindowKind::Main => attributes.with_skip_taskbar(false),
                WindowKind::Overlay => attributes
                    .with_decorations(false)
                    .with_resizable(false)
                    .with_skip_taskbar(true)
                    .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
                    .with_active(false)
                    .with_undecorated_shadow(false)
                    .with_corner_preference(CornerPreference::DoNotRound)
                    .with_border_color(None),
                WindowKind::Flyout => attributes
                    .with_decorations(false)
                    .with_resizable(false)
                    .with_skip_taskbar(true)
                    // Windows 11 rounds the corners; no drop shadow (it darkens a wide area
                    // around the flyout) and no system border (the flyout draws its own).
                    .with_undecorated_shadow(false)
                    .with_corner_preference(CornerPreference::Round)
                    .with_border_color(None),
            }
        })
        .with_winit_custom_application_handler(WindowEvents { app, mods: Default::default() })
        .select()?;

    crate::flyout::create(&app)?;
    create_tray(app);
    // Runs as the loop starts, so anything it opens has a running loop behind it.
    slint::Timer::single_shot(Duration::ZERO, ready);
    slint::run_event_loop_until_quit()
}

fn icon(png: &[u8]) -> Option<Icon> {
    let mut reader = png::Decoder::new(png).read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).ok()?;
    rgba.truncate(info.buffer_size());
    Icon::from_rgba(rgba, info.width, info.height).ok()
}

fn create_tray(app: AppHandle) {
    let open = MenuItem::with_id("open", "Open Smowaudio", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&open, &quit]);
    // Events arrive while Windows dispatches a message; act on them once that's done.
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let id = event.id.0.clone();
        let _ = slint::invoke_from_event_loop(move || match id.as_str() {
            "open" => crate::mainwin::open(&app, None),
            "quit" => app.exit(),
            _ => {}
        });
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, position, .. } = event {
            let _ = slint::invoke_from_event_loop(move || toggle_flyout(&app, position.x, position.y));
        }
    }));
    let tray = TrayIconBuilder::new()
        .with_id("tray")
        // Bars without the app icon's tile, so they read at tray size on light and dark taskbars.
        .with_icon(icon(TRAY_ICON).expect("tray icon"))
        .with_tooltip("Smowaudio")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build();
    match tray {
        Ok(tray) => TRAY.with(|t| *t.borrow_mut() = Some(tray)),
        Err(e) => append_log(&format!("tray icon unavailable: {e}")),
    }
}

/// Shows the mic's mute on the tray icon and `tooltip` as its tooltip. Call from any thread.
pub fn set_tray(muted: bool, tooltip: String) {
    let _ = slint::invoke_from_event_loop(move || {
        TRAY.with(|t| {
            if let Some(tray) = t.borrow().as_ref() {
                let _ = tray.set_tooltip(Some(&tooltip));
                let _ = tray.set_icon(icon(if muted { TRAY_ICON_MUTED } else { TRAY_ICON }));
            }
        })
    });
}

/// Toggles the flyout above the tray icon, for the shortcut and `--flyout` (there's no click to
/// place it by). Call on the main thread.
pub fn toggle_flyout_at_tray(app: &AppHandle) {
    let at_tray = TRAY.with(|t| t.borrow().as_ref().and_then(|tray| tray.rect())).map(|r| {
        let size = r.size;
        (r.position.x + size.width as f64 / 2.0, r.position.y)
    });
    let (x, y) = at_tray.unwrap_or_else(cursor_position);
    toggle_flyout(app, x, y);
}

/// Shows or hides the quick-controls flyout above a point (in physical pixels) near the tray.
fn toggle_flyout(app: &AppHandle, x: f64, y: f64) {
    // Clicking the tray icon while the flyout is open first takes focus from it, which hides it;
    // don't reopen it on that same click.
    if app.state().flyout_hidden_at.lock().is_some_and(|t| t.elapsed() < Duration::from_millis(350)) {
        return;
    }
    let screen = work_area_at(x as i32, y as i32);
    let anchor = crate::flyout::Anchor {
        x,
        y,
        area: screen.map(|((l, t, r, b), _)| (l as f64, t as f64, r as f64, b as f64)),
        scale: screen.map_or(1.0, |(_, scale)| scale),
    };
    crate::flyout::toggle(app, anchor);
}

fn cursor_position() -> (f64, f64) {
    let mut point = POINT::default();
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) };
    (point.x as f64, point.y as f64)
}

pub fn hwnd_of(window: &winit::window::Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// Brings a window to the front with focus (Windows allows it after a click on the tray, or when
/// a second launch handed over; see instance.rs).
pub fn bring_to_front(window: &slint::Window) {
    window.with_winit_window(|w: &winit::window::Window| {
        w.focus_window();
        if let Some(hwnd) = hwnd_of(w) {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(windows::Win32::Foundation::HWND(hwnd as *mut _));
            }
        }
    });
}

/// The work area (left, top, right, bottom, physical pixels) and scale of the screen at a point.
pub fn work_area_at(x: i32, y: i32) -> Option<((i32, i32, i32, i32), f64)> {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let r = info.rcWork;
        Some(((r.left, r.top, r.right, r.bottom), dpi_x as f64 / 96.0))
    }
}

/// The work area and scale of the screen the mouse is on.
pub fn work_area_at_cursor() -> Option<((i32, i32, i32, i32), f64)> {
    let (x, y) = cursor_position();
    work_area_at(x as i32, y as i32)
}

/// Window events Slint doesn't handle: the flyout hides when it loses focus, like Windows' own
/// tray flyouts, and a shortcut being recorded in Settings takes the keys before Slint sees them.
struct WindowEvents {
    app: AppHandle,
    /// Held modifiers, for recording shortcuts.
    mods: winit::keyboard::ModifiersState,
}

impl CustomApplicationHandler for WindowEvents {
    fn window_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        window_id: winit::window::WindowId,
        _winit_window: Option<&winit::window::Window>,
        _slint_window: Option<&slint::Window>,
        event: &winit::event::WindowEvent,
    ) -> EventResult {
        use winit::event::WindowEvent;
        match event {
            // Only the flyout's own focus: the main window losing focus to it mustn't close it.
            WindowEvent::Focused(false) if crate::flyout::is_window(window_id) => {
                crate::flyout::hide();
                *self.app.state().flyout_hidden_at.lock() = Some(Instant::now());
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                if crate::settingsui::is_recording() {
                    crate::settingsui::key_pressed(&self.app, held(self.mods), "", true);
                }
            }
            WindowEvent::KeyboardInput { event, .. } if crate::settingsui::is_recording() => {
                if event.state.is_pressed() && !event.repeat {
                    if let winit::keyboard::PhysicalKey::Code(code) = event.physical_key {
                        // Named like the web's KeyboardEvent.code: "KeyM", "Digit1", "F13".
                        let name = format!("{code:?}");
                        let modifier = ["Control", "Alt", "Shift", "Super", "Meta"].iter().any(|m| name.starts_with(m));
                        if !modifier {
                            crate::settingsui::key_pressed(&self.app, held(self.mods), &name, false);
                        }
                    }
                }
                return EventResult::PreventDefault;
            }
            _ => {}
        }
        EventResult::Propagate
    }
}

/// Modifiers in the order shortcuts are written: Ctrl, Alt, Shift, Win.
fn held(m: winit::keyboard::ModifiersState) -> Vec<&'static str> {
    [(m.control_key(), "Ctrl"), (m.alt_key(), "Alt"), (m.shift_key(), "Shift"), (m.super_key(), "Super")]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect()
}
