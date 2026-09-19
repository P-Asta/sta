//! Small byte helpers the shell needs in release builds, written by hand because a new dependency
//! of `sta` re-runs `cef-dll-sys`'s build script (cmake + ninja, docs/RELEASING.md).
//!
//! Public API: `pub fn base64_decode(&str) -> Option<Vec<u8>>`

/// Tolerant standard-alphabet base64: whitespace is skipped, padding is optional, and any other
/// character fails the whole decode. Used for extension manifest keys and for the base64 image a
/// CDP screenshot answers with.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}
