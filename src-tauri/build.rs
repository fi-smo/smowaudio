fn main() {
    // The tray flyout is a native Slint window; everything else is the Tauri WebView UI in ../ui.
    slint_build::compile("ui-slint/flyout.slint").expect("compile the flyout");
    tauri_build::build();
    // tauri-build links its Windows resources (icon, and the manifest that enables Common Controls
    // v6) into the app only. Examples such as the flyout preview need the manifest too, or Windows
    // refuses to start them ("TaskDialogIndirect could not be located").
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let lib = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("resource.lib");
        println!("cargo:rustc-link-arg-examples={}", lib.display());
    }
}
