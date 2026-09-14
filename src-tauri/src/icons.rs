//! App icons for the Apps view: the exe's own icon as a PNG data URL, so the UI can show it
//! with a plain <img>.

use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HDC,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, PrivateExtractIconsW, HICON, ICONINFO};

const SIZE: i32 = 48;

/// The exe's first icon at 48 px as a PNG data URL, or None if it has none. Cached per path, since
/// the Apps view asks again whenever it redraws.
pub fn app_icon(path: &str) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().get(path) {
        return hit.clone();
    }
    let url = extract(path).map(|(w, h, rgba)| format!("data:image/png;base64,{}", base64(&png(w, h, &rgba))));
    cache.lock().insert(path.to_string(), url.clone());
    url
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

/// Minimal RGBA PNG encoder. Icons are 48 px, so uncompressed deflate blocks keep it tiny and simple.
fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for row in rgba.chunks_exact(width as usize * 4) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    let mut blocks = data.chunks(65_535).peekable();
    while let Some(block) = blocks.next() {
        out.push(u8::from(blocks.peek().is_none()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for group in data.chunks(3) {
        let n = (group[0] as u32) << 16 | (*group.get(1).unwrap_or(&0) as u32) << 8 | *group.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if group.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if group.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_and_base64_match_known_values() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"hi!"), "aGkh");
    }

    #[test]
    fn png_has_valid_structure() {
        let image = png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 128]);
        assert_eq!(&image[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&image[12..16], b"IHDR");
        assert_eq!(&image[image.len() - 8..image.len() - 4], b"IEND");
    }

    #[test]
    #[ignore = "reads icons from this PC's executables; run with --ignored"]
    fn extracts_explorer_icon() {
        let url = app_icon(r"C:\Windows\explorer.exe").expect("explorer.exe has an icon");
        assert!(url.starts_with("data:image/png;base64,iVBORw0KGgo"));
        println!("explorer icon: {} bytes of base64", url.len());
    }
}
