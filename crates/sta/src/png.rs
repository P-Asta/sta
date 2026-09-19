//! A minimal 8-bit RGBA PNG codec, and the cropping the translate feature needs.
//!
//! Lifted out of `test_hooks/capture.rs`, which is compiled only in an armed debug build
//! (`main.rs`: `all(debug_assertions, feature = "test-hooks")`) — "Translate text in images" needs
//! the same codec in a release build. `capture.rs` re-exports these so its own captures and
//! `tools/capture-window.ps1` keep working unchanged.
//!
//! Hand-written on purpose: a PNG crate would be a new dependency of `sta`, and adding one re-runs
//! `cef-dll-sys`'s build script, which needs cmake and ninja (docs/RELEASING.md).
//!
//! Public API:
//! - `pub struct Image { width, height, rgba }` with `pixel(x, y)`
//! - `pub fn read_png(&[u8]) -> Result<Image, String>`, `pub fn write_png(&Image) -> Result<Vec<u8>, String>`
//! - `pub fn crop(&Image, x, y, w, h) -> Image`, `pub fn upscale2x(&Image) -> Image`

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

/// The `(x, y, w, h)` sub-image, clamped to `image`. An empty rect yields a 0x0 image, which every
/// caller treats as "nothing to read here".
pub fn crop(image: &Image, x: u32, y: u32, w: u32, h: u32) -> Image {
    let x0 = x.min(image.width);
    let y0 = y.min(image.height);
    let w = w.min(image.width.saturating_sub(x0));
    let h = h.min(image.height.saturating_sub(y0));
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let start = (((y0 + row) * image.width + x0) * 4) as usize;
        rgba.extend_from_slice(&image.rgba[start..start + (w * 4) as usize]);
    }
    Image { width: w, height: h, rgba }
}

/// Nearest-neighbour double. Windows' text recognition has a fixed minimum feature size, so small
/// captions are read far more reliably at 2x; nearest-neighbour keeps glyph edges hard, which the
/// recogniser prefers to a smoothed upscale.
pub fn upscale2x(image: &Image) -> Image {
    let (w, h) = (image.width * 2, image.height * 2);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..image.height {
        for x in 0..image.width {
            let src = ((y * image.width + x) * 4) as usize;
            let px = &image.rgba[src..src + 4];
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let dst = (((y * 2 + dy) * w + x * 2 + dx) * 4) as usize;
                rgba[dst..dst + 4].copy_from_slice(px);
            }
        }
    }
    Image { width: w, height: h, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Image {
        Image { width: w, height: h, rgba: px.iter().copied().cycle().take((w * h * 4) as usize).collect() }
    }

    #[test]
    fn a_png_round_trips() {
        let mut image = solid(5, 3, [10, 20, 30, 255]);
        image.rgba[0] = 200;
        let bytes = write_png(&image).unwrap();
        let back = read_png(&bytes).unwrap();
        assert_eq!((back.width, back.height), (5, 3));
        assert_eq!(back.rgba, image.rgba);
    }

    #[test]
    fn crop_is_clamped_to_the_image() {
        let image = solid(4, 4, [1, 2, 3, 4]);
        assert_eq!((crop(&image, 1, 1, 2, 2).width, crop(&image, 1, 1, 2, 2).height), (2, 2));
        // Entirely outside, and partly outside, both stay inside the buffer instead of panicking.
        let out = crop(&image, 9, 9, 4, 4);
        assert_eq!((out.width, out.height), (0, 0));
        let over = crop(&image, 3, 3, 10, 10);
        assert_eq!((over.width, over.height), (1, 1));
    }

    #[test]
    fn upscale_doubles_every_pixel() {
        let image = solid(2, 2, [7, 8, 9, 255]);
        let big = upscale2x(&image);
        assert_eq!((big.width, big.height), (4, 4));
        assert_eq!(big.pixel(3, 3), Some([7, 8, 9, 255]));
    }
}
