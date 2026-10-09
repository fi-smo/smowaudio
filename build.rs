fn main() {
    // The native windows (ui/app.slint).
    slint_build::compile("ui/app.slint").expect("compile the Slint UI");

    // Icon, version details and manifest. Examples such as the flyout preview need the manifest
    // too, or Windows refuses to start them ("TaskDialogIndirect could not be located").
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap();
        let mut parts: Vec<String> = version.split(['.', '-']).take(3).map(str::to_string).collect();
        parts.resize(4, "0".into());
        let commas = format!("VERSION_COMMAS={}", parts.join(","));
        let text = format!("VERSION_TEXT=\"{version}\"");
        embed_resource::compile_for_everything("windows/app.rc", [commas.as_str(), text.as_str()])
            .manifest_required()
            .expect("compile the Windows resources");
    }
    println!("cargo:rerun-if-changed=windows");
}
