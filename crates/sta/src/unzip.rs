//! Reading a ZIP archive [owner: chrome] — the release the updater downloaded (`update.rs`).
//!
//! Only what a release archive is made of: the central directory, stored (0) and deflated (8)
//! entries, no encryption, no zip64. Anything else is refused rather than guessed at, because the
//! only archives this ever opens are the ones `tools/package-release.mjs` and the release workflow
//! produce — an archive that does not look like one of those has no business being unpacked over
//! somebody's browser.
//!
//! What it checks, beyond the format:
//! - every entry's path must stay **inside** the destination: no absolute paths, no drive letters,
//!   no `..` segment, no NUL. (A "zip slip" writes `..\..\Windows\System32\…` and is the reason
//!   unpacking an archive is not just a loop over its entries.)
//! - every entry's CRC-32 and length must match its header after inflating.
//!
//! Public API:
//! - `pub struct Entry`, `pub fn entries(bytes: &[u8]) -> Result<Vec<Entry>, String>`
//! - `pub fn extract(bytes: &[u8], into: &Path) -> Result<usize, String>`

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
/// The end-of-central-directory record, plus the longest comment that may follow it.
const EOCD_MAX_TAIL: usize = 22 + u16::MAX as usize;
/// A value of all ones means "see the zip64 record", which these archives never have.
const ZIP64_MARKER: u32 = 0xFFFF_FFFF;

const STORED: u16 = 0;
const DEFLATED: u16 = 8;

/// One file in the archive (directories are not listed: their paths are created as files need them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The path inside the archive, as it was stored (`/` separated).
    pub name: String,
    pub method: u16,
    pub compressed_size: u32,
    pub size: u32,
    pub crc32: u32,
    /// Offset of the entry's local header in the archive.
    pub offset: u32,
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| format!("truncated archive at {at}"))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| format!("truncated archive at {at}"))
}

/// The archive's entries, from its central directory.
pub fn entries(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    // The record is at the end, after a comment of unknown length: scan back for its signature.
    let tail_from = bytes.len().saturating_sub(EOCD_MAX_TAIL);
    let eocd = (tail_from..bytes.len().saturating_sub(21))
        .rev()
        .find(|&at| u32_at(bytes, at) == Ok(EOCD_SIGNATURE))
        .ok_or("not a zip archive (no end-of-central-directory record)")?;
    let count = u16_at(bytes, eocd + 10)? as usize;
    let directory_at = u32_at(bytes, eocd + 16)?;
    if directory_at == ZIP64_MARKER || u16_at(bytes, eocd + 8)? == u16::MAX {
        return Err("zip64 archives are not supported".to_string());
    }

    let mut out = Vec::with_capacity(count);
    let mut at = directory_at as usize;
    for i in 0..count {
        if u32_at(bytes, at)? != CENTRAL_SIGNATURE {
            return Err(format!("entry {i} has no central directory header"));
        }
        let method = u16_at(bytes, at + 10)?;
        let crc32 = u32_at(bytes, at + 16)?;
        let compressed_size = u32_at(bytes, at + 20)?;
        let size = u32_at(bytes, at + 24)?;
        let name_len = u16_at(bytes, at + 28)? as usize;
        let extra_len = u16_at(bytes, at + 30)? as usize;
        let comment_len = u16_at(bytes, at + 32)? as usize;
        let offset = u32_at(bytes, at + 42)?;
        if compressed_size == ZIP64_MARKER || size == ZIP64_MARKER || offset == ZIP64_MARKER {
            return Err("zip64 entries are not supported".to_string());
        }
        let name = bytes
            .get(at + 46..at + 46 + name_len)
            .ok_or("truncated entry name")
            .and_then(|b| std::str::from_utf8(b).map_err(|_| "entry name is not UTF-8"))?
            .to_string();
        out.push(Entry { name, method, compressed_size, size, crc32, offset });
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// `name` as a path under `into`, or `None` when it would land anywhere else.
fn safe_path(into: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('\0') || name.starts_with('/') || name.starts_with('\\') {
        return None;
    }
    // A Windows drive or UNC prefix, spelled either way.
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        return None;
    }
    let mut path = into.to_path_buf();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return None,
            _ => path.push(part),
        }
    }
    // Belt and braces: whatever the segments were, the result has to stay under `into`.
    if !path.starts_with(into) || path.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    (path != into).then_some(path)
}

/// The bytes of one entry, inflated and checked against its header.
fn read_entry(bytes: &[u8], entry: &Entry) -> Result<Vec<u8>, String> {
    let at = entry.offset as usize;
    if u32_at(bytes, at)? != LOCAL_SIGNATURE {
        return Err(format!("{}: no local header", entry.name));
    }
    let name_len = u16_at(bytes, at + 26)? as usize;
    let extra_len = u16_at(bytes, at + 28)? as usize;
    let from = at + 30 + name_len + extra_len;
    let raw = bytes
        .get(from..from + entry.compressed_size as usize)
        .ok_or_else(|| format!("{}: the archive ends inside it", entry.name))?;
    let data = match entry.method {
        STORED => raw.to_vec(),
        DEFLATED => {
            let mut out = Vec::with_capacity(entry.size as usize);
            flate2::read::DeflateDecoder::new(raw)
                .take(entry.size as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|e| format!("{}: {e}", entry.name))?;
            out
        }
        other => return Err(format!("{}: compression method {other} is not supported", entry.name)),
    };
    if data.len() as u32 != entry.size {
        return Err(format!("{}: {} bytes, the header says {}", entry.name, data.len(), entry.size));
    }
    let mut crc = flate2::Crc::new();
    crc.update(&data);
    if crc.sum() != entry.crc32 {
        return Err(format!("{}: checksum does not match", entry.name));
    }
    Ok(data)
}

/// Unpacks every file of the archive under `into` (created if needed). Returns how many files were
/// written. Refuses an entry that would be written outside `into`, and leaves what it already wrote
/// behind for the caller to remove.
pub fn extract(bytes: &[u8], into: &Path) -> Result<usize, String> {
    fs::create_dir_all(into).map_err(|e| format!("{}: {e}", into.display()))?;
    let into = into.canonicalize().map_err(|e| format!("{}: {e}", into.display()))?;
    let mut written = 0;
    for entry in entries(bytes)? {
        if entry.name.ends_with('/') || entry.name.ends_with('\\') {
            if let Some(dir) = safe_path(&into, &entry.name) {
                fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            continue;
        }
        let path = safe_path(&into, &entry.name).ok_or_else(|| format!("{:?} would be written outside the update", entry.name))?;
        let data = read_entry(bytes, &entry)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(&path, &data).map_err(|e| format!("{}: {e}", path.display()))?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Builds a ZIP in memory: `(name, contents, deflate)`.
    fn zip(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data, deflate) in files {
            let offset = out.len() as u32;
            let mut crc = flate2::Crc::new();
            crc.update(data);
            let (method, payload) = if *deflate {
                let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
                encoder.write_all(data).unwrap();
                (DEFLATED, encoder.finish().unwrap())
            } else {
                (STORED, data.to_vec())
            };
            let header = |signature: u32, extra_fields: &[u8]| {
                let mut h = signature.to_le_bytes().to_vec();
                h.extend_from_slice(extra_fields);
                h
            };
            // Local header.
            let mut local = header(LOCAL_SIGNATURE, &[]);
            local.extend_from_slice(&20u16.to_le_bytes()); // version needed
            local.extend_from_slice(&0u16.to_le_bytes()); // flags
            local.extend_from_slice(&method.to_le_bytes());
            local.extend_from_slice(&[0; 4]); // time, date
            local.extend_from_slice(&crc.sum().to_le_bytes());
            local.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            local.extend_from_slice(&(data.len() as u32).to_le_bytes());
            local.extend_from_slice(&(name.len() as u16).to_le_bytes());
            local.extend_from_slice(&0u16.to_le_bytes()); // extra
            local.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&local);
            out.extend_from_slice(&payload);
            // Central directory entry.
            let mut c = header(CENTRAL_SIGNATURE, &[]);
            c.extend_from_slice(&20u16.to_le_bytes()); // version made by
            c.extend_from_slice(&20u16.to_le_bytes()); // version needed
            c.extend_from_slice(&0u16.to_le_bytes()); // flags
            c.extend_from_slice(&method.to_le_bytes());
            c.extend_from_slice(&[0; 4]); // time, date
            c.extend_from_slice(&crc.sum().to_le_bytes());
            c.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            c.extend_from_slice(&(data.len() as u32).to_le_bytes());
            c.extend_from_slice(&(name.len() as u16).to_le_bytes());
            c.extend_from_slice(&[0; 6]); // extra len, comment len, disk number
            c.extend_from_slice(&[0; 6]); // internal (2) and external (4) attributes
            c.extend_from_slice(&offset.to_le_bytes());
            c.extend_from_slice(name.as_bytes());
            central.extend_from_slice(&c);
        }
        let directory_at = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&[0; 4]); // disks
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&directory_at.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        out
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sta-unzip-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn reads_stored_and_deflated_entries() {
        let big = "sta ".repeat(5000);
        let archive = zip(&[("sta.exe", b"MZ fake", false), ("locales/en-US.pak", big.as_bytes(), true)]);
        let list = entries(&archive).expect("entries");
        assert_eq!(list.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["sta.exe", "locales/en-US.pak"]);
        assert_eq!(list[1].method, DEFLATED);
        assert!(list[1].compressed_size < list[1].size, "the deflated entry is smaller than its contents");

        let dir = temp_dir("read");
        assert_eq!(extract(&archive, &dir).expect("extract"), 2);
        assert_eq!(fs::read(dir.join("sta.exe")).unwrap(), b"MZ fake");
        assert_eq!(fs::read_to_string(dir.join("locales").join("en-US.pak")).unwrap(), big);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nothing_is_written_outside_the_destination() {
        let dir = temp_dir("escape");
        for name in ["../escaped.txt", "..\\escaped.txt", "a/../../escaped.txt", "/etc/passwd", "C:\\Windows\\x.dll"] {
            let archive = zip(&[(name, b"no", false)]);
            let err = extract(&archive, &dir).expect_err(name);
            assert!(err.contains("outside the update"), "{name}: {err}");
        }
        // …and a path that only looks dangerous is fine.
        let archive = zip(&[("ui/..safe/x.txt", b"yes", false)]);
        assert_eq!(extract(&archive, &dir).expect("extract"), 1);
        assert!(dir.join("ui").join("..safe").join("x.txt").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_damaged_or_unsupported_archive_is_refused() {
        let dir = temp_dir("damaged");
        assert!(entries(b"not a zip at all").is_err());
        // A flipped byte in the payload fails the checksum rather than being written.
        let mut archive = zip(&[("sta.exe", b"MZ fake", false)]);
        let at = archive.iter().position(|&b| b == b'f').expect("payload");
        archive[at] = b'F';
        let err = extract(&archive, &dir).expect_err("checksum");
        assert!(err.contains("checksum"), "{err}");
        // An entry whose method we do not implement is refused, not guessed at.
        let mut archive = zip(&[("sta.exe", b"MZ fake", false)]);
        let list = entries(&archive).unwrap();
        let central_method_at = archive.len() - 22 - 46 - list[0].name.len() + 10;
        archive[central_method_at] = 99;
        let err = extract(&archive, &dir).expect_err("method");
        assert!(err.contains("not supported"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }
}
