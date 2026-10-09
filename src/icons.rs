//! App icons for the Apps view and the Mixer strips: the exe's own icon, as RGBA pixels.

use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HDC,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, PrivateExtractIconsW, HICON, ICONINFO};

const SIZE: i32 = 48;

/// The exe's first icon at 48 px as RGBA pixels (width, height, pixels), or None. Cached per path.
pub fn app_icon_rgba(path: &str) -> Option<std::sync::Arc<(u32, u32, Vec<u8>)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<std::sync::Arc<(u32, u32, Vec<u8>)>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().get(path) {
        return hit.clone();
    }
    let icon = extract(path).map(std::sync::Arc::new);
    cache.lock().insert(path.to_string(), icon.clone());
    icon
}

fn extract(path: &str) -> Option<(u32, u32, Vec<u8>)> {
    let wide: Vec<u16> = path.encode_utf16().collect();
    let mut name = [0u16; 260];
    if wide.len() >= name.len() {
        return None;
    }
    name[..wide.len()].copy_from_slice(&wide);
    let mut icons = [HICON::default()];
    let found = unsafe { PrivateExtractIconsW(&name, 0, SIZE, SIZE, Some(&mut icons), None, 0) };
    if found == 0 || found == u32::MAX || icons[0].is_invalid() {
        return None;
    }
    let pixels = unsafe { icon_rgba(icons[0]) };
    unsafe {
        let _ = DestroyIcon(icons[0]);
    }
    pixels
}

/// Reads an icon's color bitmap as straight RGBA. Old icons without an alpha channel get their
/// transparency from the icon mask instead.
unsafe fn icon_rgba(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    let mut info = ICONINFO::default();
    GetIconInfo(icon, &mut info).ok()?;
    let (color, mask) = (info.hbmColor, info.hbmMask);
    let result = (|| {
        if color.is_invalid() {
            return None;
        }
        let mut bitmap = BITMAP::default();
        let size = std::mem::size_of::<BITMAP>() as i32;
        if GetObjectW(color.into(), size, Some((&mut bitmap as *mut BITMAP).cast())) == 0 {
            return None;
        }
        let (width, height) = (bitmap.bmWidth, bitmap.bmHeight);
        let dc = CreateCompatibleDC(None);
        let pixels = read_bits(dc, color, width, height);
        let mask_pixels = if mask.is_invalid() { None } else { read_bits(dc, mask, width, height) };
        let _ = DeleteDC(dc);

        let mut pixels = pixels?;
        let has_alpha = pixels.chunks_exact(4).any(|p| p[3] != 0);
        for (i, p) in pixels.chunks_exact_mut(4).enumerate() {
            p.swap(0, 2); // BGRA -> RGBA
            if !has_alpha {
                let transparent = mask_pixels.as_ref().is_some_and(|m| m[i * 4] != 0);
                p[3] = if transparent { 0 } else { 255 };
            }
        }
        Some((width as u32, height as u32, pixels))
    })();
    if !color.is_invalid() {
        let _ = DeleteObject(color.into());
    }
    if !mask.is_invalid() {
        let _ = DeleteObject(mask.into());
    }
    result
}

/// A bitmap's pixels as top-down 32-bit BGRA.
unsafe fn read_bits(dc: HDC, bitmap: HBITMAP, width: i32, height: i32) -> Option<Vec<u8>> {
    let mut info = BITMAPINFO::default();
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let mut buffer = vec![0u8; (width * height * 4) as usize];
    let lines = GetDIBits(dc, bitmap, 0, height as u32, Some(buffer.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
    (lines != 0).then_some(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "reads icons from this PC's executables; run with --ignored"]
    fn extracts_explorer_icon() {
        let icon = app_icon_rgba(r"C:\Windows\explorer.exe").expect("explorer.exe has an icon");
        assert_eq!((icon.0, icon.1), (48, 48));
        assert!(icon.2.chunks(4).any(|px| px[3] > 0), "some pixels are visible");
    }
}
