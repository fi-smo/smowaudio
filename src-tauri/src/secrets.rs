//! The GitHub token for updates, kept in Windows Credential Manager (encrypted to the Windows
//! account) rather than in config.json.

use windows::core::{w, PWSTR};
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

const TARGET: windows::core::PCWSTR = w!("Smowaudio/GitHub token");

pub fn read_token() -> Option<String> {
    unsafe {
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        CredReadW(TARGET, CRED_TYPE_GENERIC, None, &mut credential).ok()?;
        let c = &*credential;
        let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let token = String::from_utf8(blob.to_vec()).ok();
        CredFree(credential as *const _);
        token.filter(|t| !t.is_empty())
    }
}

pub fn save_token(token: &str) -> windows::core::Result<()> {
    let mut blob = token.as_bytes().to_vec();
    let mut target: Vec<u16> = "Smowaudio/GitHub token\0".encode_utf16().collect();
    let mut user: Vec<u16> = "smowaudio\0".encode_utf16().collect();
    let credential = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_mut_ptr()),
        ..Default::default()
    };
    unsafe { CredWriteW(&credential, 0) }
}

pub fn delete_token() {
    unsafe {
        let _ = CredDeleteW(TARGET, CRED_TYPE_GENERIC, None);
    }
}
