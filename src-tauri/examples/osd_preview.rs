//! Shows the shortcut overlay with sample data, for checking its look without the app around it:
//! `cargo run --example osd_preview [light] [muted]`
slint::include_modules!();

fn main() {
    let window = OsdWindow::new().unwrap();
    let args: Vec<String> = std::env::args().collect();
    window.global::<Theme>().set_dark(!args.iter().any(|a| a == "light"));
    let muted = args.iter().any(|a| a == "muted");
    window.set_group("game".into());
    window.set_tape("Game".into());
    window.set_label("Volume".into());
    window.set_value(if muted { "Muted".into() } else { "65 %".into() });
    window.set_level(0.65 / 1.5);
    window.set_unity(1.0 / 1.5);
    window.set_dim(muted);
    window.set_shown(true);
    window.run().unwrap();
}
