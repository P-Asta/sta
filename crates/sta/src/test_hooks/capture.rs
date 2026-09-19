//! Window capture and PNG for the test surface: what `tools/capture-window.ps1` and
//! `e2e/png-pixel.ps1` did from PowerShell, without a console window and without a screen grab.
//!
//! - [`window`] draws **one window of this process** with `PrintWindow(PW_RENDERFULLCONTENT)` —
//!   the only way to get GPU-composited Chromium content — into a top-down 32-bit DIB. Nothing
//!   outside that window is ever read.
//! - [`write_png`] / [`read_png`] are a minimal 8-bit PNG writer and reader (zlib through
//!   `flate2`, all five scanline filters on the way in), so a capture is a normal PNG that a
//!   person, an agent or `.NET` can open, and `test_pixels` can read a capture that
//!   `capture-window.ps1` wrote.

use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;

pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA, top-down.
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        Some([self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]])
    }
}

// ----------------------------------------------------------------------------------- capture

#[cfg(windows)]
mod sys {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::HDC;

    // Not in the windows-sys features this crate enables; declared here so the test surface adds
    // nothing to a normal build.
    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> i32;
        pub fn GetDpiForWindow(hwnd: HWND) -> u32;
        pub fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
    }
}

#[cfg(windows)]
pub fn dpi_of(hwnd: isize) -> u32 {
    if hwnd == 0 {
        return 96;
    }
    // SAFETY: plain Win32 call on a window handle.
    let dpi = unsafe { sys::GetDpiForWindow(hwnd as _) };
    if dpi == 0 { 96 } else { dpi }
}

#[cfg(not(windows))]
pub fn dpi_of(_hwnd: isize) -> u32 {
    96
}

/// `PW_RENDERFULLCONTENT` (2), plus `PW_CLIENTONLY` (1) for the client region.
#[cfg(windows)]
pub fn window(hwnd: isize, client_only: bool) -> Result<Image, String> {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
        SelectObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

    if hwnd == 0 {
        return Err("no window".into());
    }
    let h = hwnd as HWND;
    // SAFETY: every handle below is created here and released on every path; the DIB buffer is
    // sized from the header GetDIBits is given.
    unsafe {
        let mut rect: RECT = std::mem::zeroed();
        let ok = if client_only { sys::GetClientRect(h, &mut rect) } else { GetWindowRect(h, &mut rect) };
        if ok == 0 {
            return Err("the window has no rectangle".into());
        }
        let (width, height) = ((rect.right - rect.left).max(1), (rect.bottom - rect.top).max(1));
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return Err("no screen DC".into());
        }
        let memory = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let mut result = Err("PrintWindow failed".to_string());
        if !memory.is_null() && !bitmap.is_null() {
            let previous = SelectObject(memory, bitmap.cast());
            let flags = if client_only { 1 | 2 } else { 2 };
            if sys::PrintWindow(h, memory, flags) != 0 {
                let mut info: BITMAPINFO = std::mem::zeroed();
                info.bmiHeader = BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                };
                let mut bgra = vec![0u8; (width as usize) * (height as usize) * 4];
                let lines = GetDIBits(memory, bitmap, 0, height as u32, bgra.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS);
                if lines > 0 {
                    for px in bgra.chunks_exact_mut(4) {
                        px.swap(0, 2); // BGRA → RGBA
                        px[3] = 255; // PrintWindow leaves the alpha channel undefined
                    }
                    result = Ok(Image { width: width as u32, height: height as u32, rgba: bgra });
                } else {
                    result = Err("GetDIBits returned no scan lines".into());
                }
            }
            SelectObject(memory, previous);
        }
        if !bitmap.is_null() {
            DeleteObject(bitmap.cast());
        }
        if !memory.is_null() {
            DeleteDC(memory);
        }
        ReleaseDC(std::ptr::null_mut(), screen);
        result
    }
}

#[cfg(not(windows))]
pub fn window(_hwnd: isize, _client_only: bool) -> Result<Image, String> {
    Err("window capture is a Windows feature".into())
}

// ----------------------------------------------------------------------------------- PNG

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(body);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

/// An 8-bit RGBA PNG (color type 6), filter 0 on every scan line.
pub fn write_png(image: &Image) -> Result<Vec<u8>, String> {
    let mut raw = Vec::with_capacity(image.rgba.len() + image.height as usize);
    for y in 0..image.height as usize {
        let start = y * image.width as usize * 4;
        raw.push(0);
        raw.extend_from_slice(&image.rgba[start..start + image.width as usize * 4]);
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&raw).map_err(|e| e.to_string())?;
    let compressed = encoder.finish().map_err(|e| e.to_string())?;

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&image.width.to_be_bytes());
    header.extend_from_slice(&image.height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &header);
    chunk(&mut png, b"IDAT", &compressed);
    chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i32, b as i32, c as i32);
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// Reads an 8-bit non-interlaced PNG (grayscale, grayscale+alpha, RGB or RGBA) into RGBA — enough
/// for both this module's captures and the ones `tools/capture-window.ps1` writes.
pub fn read_png(bytes: &[u8]) -> Result<Image, String> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    if bytes.len() < 8 || bytes[..8] != [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a] {
        return Err("not a PNG".into());
    }
    let (mut width, mut height, mut channels) = (0u32, 0u32, 0usize);
    let mut idat = Vec::new();
    let mut at = 8;
    while at + 8 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().map_err(|_| "truncated")?) as usize;
        let kind = &bytes[at + 4..at + 8];
        let body_at = at + 8;
        if body_at + length + 4 > bytes.len() {
            return Err("truncated chunk".into());
        }
        let body = &bytes[body_at..body_at + length];
        match kind {
            b"IHDR" => {
                if length < 13 {
                    return Err("short IHDR".into());
                }
                width = u32::from_be_bytes(body[0..4].try_into().unwrap());
                height = u32::from_be_bytes(body[4..8].try_into().unwrap());
                let (depth, color, interlace) = (body[8], body[9], body[12]);
                if depth != 8 || interlace != 0 {
                    return Err(format!("only 8-bit non-interlaced PNGs are read (depth {depth}, interlace {interlace})"));
                }
                channels = match color {
                    0 => 1,
                    2 => 3,
                    4 => 2,
                    6 => 4,
                    other => return Err(format!("unsupported PNG color type {other}")),
                };
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        at = body_at + length + 4;
    }
    if width == 0 || height == 0 || channels == 0 {
        return Err("no image header".into());
    }
    let mut raw = Vec::new();
    ZlibDecoder::new(&idat[..]).read_to_end(&mut raw).map_err(|e| format!("inflate: {e}"))?;
    let stride = width as usize * channels;
    if raw.len() < (stride + 1) * height as usize {
        return Err("short image data".into());
    }
    let mut lines: Vec<u8> = vec![0; stride * height as usize];
    for y in 0..height as usize {
        let filter = raw[y * (stride + 1)];
        let src = &raw[y * (stride + 1) + 1..y * (stride + 1) + 1 + stride];
        for x in 0..stride {
            let a = if x >= channels { lines[y * stride + x - channels] } else { 0 };
            let b = if y > 0 { lines[(y - 1) * stride + x] } else { 0 };
            let c = if y > 0 && x >= channels { lines[(y - 1) * stride + x - channels] } else { 0 };
            let value = match filter {
                0 => src[x],
                1 => src[x].wrapping_add(a),
                2 => src[x].wrapping_add(b),
                3 => src[x].wrapping_add((((a as u16) + (b as u16)) / 2) as u8),
                4 => src[x].wrapping_add(paeth(a, b, c)),
                other => return Err(format!("unknown scan-line filter {other}")),
            };
            lines[y * stride + x] = value;
        }
    }
    let mut rgba = vec![255u8; width as usize * height as usize * 4];
    for i in 0..width as usize * height as usize {
        let p = &lines[i * channels..i * channels + channels];
        let (r, g, b, a) = match channels {
            1 => (p[0], p[0], p[0], 255),
            2 => (p[0], p[0], p[0], p[1]),
            3 => (p[0], p[1], p[2], 255),
            _ => (p[0], p[1], p[2], p[3]),
        };
        rgba[i * 4..i * 4 + 4].copy_from_slice(&[r, g, b, a]);
    }
    Ok(Image { width, height, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip() {
        let mut rgba = Vec::new();
        for y in 0..7u32 {
            for x in 0..5u32 {
                rgba.extend_from_slice(&[(x * 40) as u8, (y * 30) as u8, 0x7f, 255]);
            }
        }
        let image = Image { width: 5, height: 7, rgba };
        let png = write_png(&image).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let back = read_png(&png).unwrap();
        assert_eq!((back.width, back.height), (5, 7));
        assert_eq!(back.pixel(4, 6), Some([160, 180, 0x7f, 255]));
        assert_eq!(back.pixel(0, 0), Some([0, 0, 0x7f, 255]));
        assert_eq!(back.pixel(5, 0), None, "outside the image");
        assert!(read_png(b"not a png at all").is_err());
    }

    /// The reader must handle every scan-line filter: a hand-built image with all five.
    #[test]
    fn png_filters() {
        let (w, h) = (4usize, 5usize);
        let mut raw = Vec::new();
        for y in 0..h {
            raw.push(y as u8 % 5); // filters 0..4, one per row
            for x in 0..w * 3 {
                raw.push(((x + y) % 7) as u8);
            }
        }
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&raw).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut header = Vec::new();
        header.extend_from_slice(&(w as u32).to_be_bytes());
        header.extend_from_slice(&(h as u32).to_be_bytes());
        header.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &header);
        chunk(&mut png, b"IDAT", &compressed);
        chunk(&mut png, b"IEND", &[]);
        let image = read_png(&png).expect("all five filters");
        assert_eq!((image.width, image.height), (4, 5));
        assert_eq!(image.pixel(0, 0), Some([0, 1, 2, 255]), "row 0 is unfiltered");
    }
}
