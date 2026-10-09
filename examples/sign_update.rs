//! Signs an installer for the updater, as the release workflow does:
//! `cargo run --release --example sign_update -- <installer> <version>`, with the release key in
//! SMOWAUDIO_SIGNING_KEY (base64, as `tauri signer generate` wrote it) and its password, if it has
//! one, in SMOWAUDIO_SIGNING_KEY_PASSWORD. Writes `<installer>.sig`, which goes into latest.json.
use base64::Engine as _;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, file, version] = args.as_slice() else {
        eprintln!("usage: sign_update <installer> <version>");
        std::process::exit(2);
    };
    let b64 = base64::engine::general_purpose::STANDARD;
    let key = std::env::var("SMOWAUDIO_SIGNING_KEY").expect("SMOWAUDIO_SIGNING_KEY isn't set");
    let password = std::env::var("SMOWAUDIO_SIGNING_KEY_PASSWORD").unwrap_or_default();
    let key = String::from_utf8(b64.decode(key.trim()).expect("the key isn't base64")).expect("the key isn't text");
    let key = minisign::SecretKeyBox::from_string(&key)
        .and_then(|b| b.into_secret_key(Some(password)))
        .expect("the key or its password is wrong");

    let data = std::fs::read(file).expect("read the installer");
    let name = std::path::Path::new(file).file_name().unwrap().to_string_lossy();
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    // The updater checks the version here against the one latest.json announces.
    let trusted = format!("timestamp:{timestamp}\tfile:{name}\tversion:{version}");
    let signature = minisign::sign(None, &key, std::io::Cursor::new(data), Some(&trusted), Some("signature from the Smowaudio release key"))
        .expect("sign");
    std::fs::write(format!("{file}.sig"), b64.encode(signature.to_string())).expect("write the signature");
    println!("signed {name} as {version}");
}
