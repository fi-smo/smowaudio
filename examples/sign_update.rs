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
    let key = if password.is_empty() {
        open_with_empty_password(&key)
    } else {
        minisign::SecretKeyBox::from_string(&key)
            .and_then(|b| b.into_secret_key(Some(password)))
            .expect("the key or its password is wrong")
    };

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

/// Tauri's key generator encrypted the key even with an empty password, but current minisign
/// treats an empty password as "not encrypted" (and asks on the console when given none), so the
/// key is decrypted here: XOR with scrypt("", salt) over the key number, key and checksum.
fn open_with_empty_password(key_box: &str) -> minisign::SecretKey {
    let b64 = base64::engine::general_purpose::STANDARD;
    let encoded = key_box.lines().nth(1).expect("the key has no key line");
    let mut bytes = b64.decode(encoded.trim()).expect("the key line isn't base64");
    // Layout: algorithms (6 bytes), salt (32), opslimit (8), memlimit (8), key number, key and
    // checksum (8 + 64 + 32).
    assert_eq!(bytes.len(), 158, "unexpected key size");
    let salt = bytes[6..38].to_vec();
    let opslimit = u64::from_le_bytes(bytes[38..46].try_into().unwrap());
    let memlimit = u64::from_le_bytes(bytes[46..54].try_into().unwrap());
    let mut stream = [0u8; 104];
    scrypt::scrypt(b"", &salt, &scrypt_params(memlimit, opslimit), &mut stream).expect("derive the key stream");
    for (byte, s) in bytes[54..].iter_mut().zip(stream) {
        *byte ^= s;
    }
    minisign::SecretKey::from_bytes(&bytes).expect("read the decrypted key")
}

/// libsodium's scrypt parameters for an opslimit and memlimit, as minisign derives them.
fn scrypt_params(memlimit: u64, opslimit: u64) -> scrypt::Params {
    let opslimit = opslimit.max(32768);
    let r = 8u32;
    let mut n_log2 = 1u8;
    let p;
    if opslimit < memlimit / 32 {
        p = 1;
        let maxn = opslimit / (u64::from(r) * 4);
        while n_log2 < 63 && 1u64 << n_log2 <= maxn / 2 {
            n_log2 += 1;
        }
    } else {
        let maxn = memlimit / (u64::from(r) * 128);
        while n_log2 < 63 && 1u64 << n_log2 <= maxn / 2 {
            n_log2 += 1;
        }
        let maxrp = (0x3fff_ffff_u64).min((opslimit / 4) / (1u64 << n_log2)) as u32;
        p = maxrp / r;
    }
    scrypt::Params::new(n_log2, r, p, scrypt::Params::RECOMMENDED_LEN).expect("scrypt parameters")
}
