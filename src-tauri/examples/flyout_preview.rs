//! Shows the tray flyout with sample data, for checking its look without the app around it:
//! `cargo run --example flyout_preview [light]`
slint::include_modules!();

fn main() {
    let window = FlyoutWindow::new().unwrap();
    window.global::<Theme>().set_dark(std::env::args().nth(1).as_deref() != Some("light"));
    // Like the app: the scale of the screen it opens on (known once the window exists).
    let weak = window.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(1), move || {
        if let Some(w) = weak.upgrade() {
            w.global::<Px>().set_scale(w.window().scale_factor());
        }
    });
    let row = |name: &str, volume: f32, muted: bool, level: f32, peak: f32| Row { name: name.into(), volume, muted, level, peak };
    window.set_rows(std::rc::Rc::new(slint::VecModel::from(vec![
        row("Game", 0.2, false, 0.86, 0.9),
        row("Chat", 1.0, false, 0.0, 0.0),
        row("Media", 1.0, false, 0.95, 0.97),
        row("Aux", 1.0, true, 0.0, 0.0),
        row("Master", 1.0, false, 0.88, 0.93),
    ])).into());
    let device = |id: &str, label: &str, selected: bool| Device { id: id.into(), label: label.into(), title: label.into(), selected };
    window.set_devices(std::rc::Rc::new(slint::VecModel::from(vec![
        device("", "Automatic", true),
        device("a", "soundcore Select 4 Go", false),
        device("b", "Arctis Nova Pro Wireless", false),
    ])).into());
    window.set_output_label("Automatic".into());
    window.set_status_text("All running".into());
    window.set_mic_text("Off · not connected".into());
    window.run().unwrap();
}
