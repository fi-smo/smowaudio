//! Shows the native main window with sample data: `cargo run --example main_preview [light]`
slint::include_modules!();

fn main() {
    let window = MainWindow::new().unwrap();
    window.global::<Theme>().set_dark(std::env::args().nth(1).as_deref() != Some("light"));
    let weak = window.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(1), move || {
        if let Some(w) = weak.upgrade() {
            w.global::<Px>().set_scale(w.window().scale_factor());
        }
    });
    let strip = |name: &str, kind: i32, sub: &str, note: &str, volume: f32, readout: &str, l: f32, k: &str, v: &str| StripData {
        name: name.into(), sub: sub.into(), note: note.into(), kind, volume, muted: false, readout: readout.into(),
        level_l: l, level_r: l * 0.9, peak_l: (l + 0.05).min(1.0), peak_r: l, feature_k: k.into(), feature_v: v.into(),
        feature_open: false, feature_static: kind == 4, ..Default::default()
    };
    window.set_channels(std::rc::Rc::new(slint::VecModel::from(vec![
        strip("Game", 0, "CABLE-A", "No apps yet", 0.2, "−14.0 dB", 0.78, "EQ", "Off"),
        strip("Chat", 1, "CABLE-B", "No apps yet", 1.0, "0.0 dB", 0.0, "EQ", "Off"),
        strip("Media", 2, "CABLE-C", "No apps yet", 1.0, "0.0 dB", 0.83, "EQ", "Bass boost"),
        strip("Aux", 3, "CABLE-D", "No apps yet", 1.0, "0.0 dB", 0.0, "EQ", "Off"),
    ])).into());
    window.set_master(strip("Master", 4, "Output", "soundcore Select 4 Go", 1.0, "0.0 dB", 0.85, "Limit", "0.0 dB"));
    window.set_mic(strip("Mic", 5, "CABLE-C Output", "No microphone", 1.5, "+3.5 dB", 0.6, "Chain", "4 of 5 on"));
    window.set_output_name("Automatic".into());
    window.set_status_text("Running · mic off".into());
    window.set_buffer("14 ms".into());
    // The Apps view, with two sample apps, when asked for: `main_preview dark apps`.
    if std::env::args().nth(2).as_deref() == Some("apps") {
        let card = |exe: &str, name: &str, ch: i32| AppCardData {
            exe: exe.into(), channel: ch, chosen: true, level: "−∞".into(), silent: true, peak: -1,
            icon: AppIcon { letter: name[..1].into(), name: name.into(), ..Default::default() }, ..Default::default()
        };
        let lanes: Vec<LaneData> = ["Game", "Chat", "Media", "Aux"].iter().enumerate().map(|(i, n)| LaneData {
            name: (*n).into(),
            cards: std::rc::Rc::new(slint::VecModel::from(match i { 1 => vec![card("blip.exe", "Blip", 1)], 2 => vec![card("spotify.exe", "Spotify", 2)], _ => vec![] })).into(),
        }).collect();
        window.set_lanes(std::rc::Rc::new(slint::VecModel::from(lanes)).into());
        window.set_view("apps".into());
        window.on_move_app(|exe, ch| println!("move-app {exe} -> {ch}"));
    }
    window.run().unwrap();
}
