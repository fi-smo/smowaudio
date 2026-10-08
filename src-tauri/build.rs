fn main() {
    // Native Slint windows (ui-slint/app.slint): the tray flyout, and the main window being ported.
    slint_build::compile("ui-slint/app.slint").expect("compile the Slint UI");
    tauri_build::build();
    // tauri-build links its Windows resources (icon, and the manifest that enables Common Controls
    // v6) into the app only. Examples such as the flyout preview need the manifest too, or Windows
    // refuses to start them ("TaskDialogIndirect could not be located").
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let lib = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("resource.lib");
        println!("cargo:rustc-link-arg-examples={}", lib.display());
    }
}
