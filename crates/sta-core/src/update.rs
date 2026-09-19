//! Updates: the release manifest and the state a build is in [owner: core]
//! (`crates/sta/src/update.rs` does the fetching, docs/RELEASING.md describes the release).
//!
//! A release built by `.github/workflows/release.yml` carries one archive per platform and a
//! `latest.json` describing them. A running sta reads that file from the **published** release
//! ([`MANIFEST_URL`]) — GitHub does not serve drafts there, so a draft offers nobody anything —
//! and compares its `version` with its own [`env!("CARGO_PKG_VERSION")`].
//!
//! What this module is responsible for: parsing that manifest, deciding whether it is newer,
//! picking this platform's archive, and refusing one that does not come from the project's own
//! releases. Everything with a socket or a file handle in it lives in the shell.
//!
//! The safety of an update rests on three things, in this order:
//! 1. the manifest is fetched over **HTTPS** from `github.com` and nowhere else;
//! 2. the archive it names must live under the same repository's releases ([`Asset::trusted`]) —
//!    so a manifest that is somehow rewritten cannot point at another host;
//! 3. the downloaded bytes must hash to the **SHA-256** the manifest states before anything is
//!    unpacked or run.
//!
//! There is no signature: whoever controls the GitHub repository controls the release either way.
//! Adding one (an ed25519 key in CI, the public half compiled in) is a change to this file and to
//! the workflow, nothing else.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// The repository releases are published from. Both URLs below must live under it.
pub const REPOSITORY: &str = "P-Asta/sta";
/// What that repository was called until 2026-09 (`P-Asta/Astatine` still redirects). A manifest may
/// name assets under either: the workflow writes whatever `GITHUB_REPOSITORY` says, and 0.1.x trusted
/// only the old name — which refused every asset the renamed repository's workflow ever listed.
pub const LEGACY_REPOSITORY: &str = "P-Asta/Astatine";
/// The manifest of the latest **published** release.
pub const MANIFEST_URL: &str = "https://github.com/P-Asta/sta/releases/latest/download/latest.json";
/// A manifest longer than this is not one of ours.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// An archive larger than this is not one of ours either (the Windows build is ~200 MB).
pub const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;

/// This build's key in a manifest's `platforms` map (`windows-x86_64`, `darwin-aarch64`, …).
pub const fn platform_key() -> &'static str {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(windows, target_arch = "aarch64"))]
    {
        "windows-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "darwin-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "darwin-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        "unknown"
    }
}

/// How this copy of sta was put on the machine, which decides what can update it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    /// Unpacked from the `.zip` (or the macOS app): the updater swaps the files itself.
    Archive,
    /// Installed by the `.msi` into Program Files, where sta cannot write: the next `.msi` updates
    /// it (`msiexec /i … /passive`), listed in the manifest as `<platform>-msi`.
    Msi,
}

impl Package {
    /// This package's key in a manifest's `platforms` map.
    pub fn key(self) -> String {
        match self {
            Package::Archive => platform_key().to_string(),
            Package::Msi => format!("{}-msi", platform_key()),
        }
    }
}

/// One platform's archive in a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub url: String,
    /// Lowercase hex SHA-256 of the archive.
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

impl Asset {
    /// Whether this archive may be downloaded: an HTTPS URL under this repository's releases, a
    /// well-formed hash, and a plausible size. A manifest is data from the network; nothing about
    /// it is believed without this.
    pub fn trusted(&self) -> bool {
        let hash_ok = self.sha256.len() == 64 && self.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        let url_ok = [REPOSITORY, LEGACY_REPOSITORY].iter().any(|repository| {
            let prefix = format!("https://github.com/{repository}/releases/download/");
            self.url.starts_with(&prefix) && !self.url[prefix.len()..].contains("..")
        });
        let size_ok = self.size == 0 || self.size <= MAX_ARCHIVE_BYTES;
        hash_ok && url_ok && size_ok
    }

    /// The archive's file name (the last path segment), for the staging directory.
    pub fn file_name(&self) -> &str {
        self.url.rsplit('/').next().unwrap_or("sta-update.zip")
    }
}

/// `latest.json` as the workflow writes it (`tools/package-release.mjs manifest`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub pub_date: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub platforms: BTreeMap<String, Asset>,
}

impl Manifest {
    /// Parses a manifest body. Anything that is not JSON, is too long, or carries a version that is
    /// not a version at all is refused here rather than believed later.
    pub fn parse(body: &[u8]) -> Result<Manifest, String> {
        if body.len() > MAX_MANIFEST_BYTES {
            return Err(format!("manifest is {} bytes, more than {MAX_MANIFEST_BYTES}", body.len()));
        }
        let manifest: Manifest = serde_json::from_slice(body).map_err(|e| format!("manifest is not valid JSON: {e}"))?;
        if parse_version(&manifest.version).is_none() {
            return Err(format!("manifest version {:?} is not a version", manifest.version));
        }
        Ok(manifest)
    }

    /// This platform's archive, when the manifest has one and it may be downloaded.
    pub fn asset(&self) -> Option<&Asset> {
        self.asset_for(Package::Archive)
    }

    /// What updates a copy of sta that was installed as `package`. There is no falling back from
    /// one to the other: an archive cannot be applied to Program Files, and an .msi would install a
    /// second copy beside a portable one.
    pub fn asset_for(&self, package: Package) -> Option<&Asset> {
        self.platforms.get(&package.key()).filter(|a| a.trusted())
    }

    /// Whether this manifest describes a build that supersedes `current`.
    pub fn is_newer_than(&self, current: &str) -> bool {
        compare_versions(&self.version, current) == Ordering::Greater
    }
}

/// `1.2.3`, `1.2.3-beta.1` → the numeric parts and the pre-release tag. `None` when it is not a
/// version (a build with no numbers in it, an empty string, a tag with letters in its numbers).
fn parse_version(v: &str) -> Option<(Vec<u64>, Option<String>)> {
    let v = v.trim().strip_prefix('v').unwrap_or(v.trim());
    let (numbers, pre) = match v.split_once('-') {
        Some((n, p)) if !p.is_empty() => (n, Some(p.to_string())),
        Some(_) => return None,
        None => (v, None),
    };
    let parts: Vec<u64> = numbers.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    (!parts.is_empty() && parts.len() <= 4).then_some((parts, pre))
}

/// Orders two versions the way semver does: numbers first, and a pre-release is *older* than the
/// release of the same numbers (`1.2.0-beta.1` < `1.2.0`). Anything unparseable sorts as equal, so
/// a manifest that makes no sense never looks like an update.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (Some((a_nums, a_pre)), Some((b_nums, b_pre))) = (parse_version(a), parse_version(b)) else {
        return Ordering::Equal;
    };
    let len = a_nums.len().max(b_nums.len());
    for i in 0..len {
        let (x, y) = (a_nums.get(i).copied().unwrap_or(0), b_nums.get(i).copied().unwrap_or(0));
        if x != y {
            return x.cmp(&y);
        }
    }
    match (a_pre, b_pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => x.cmp(&y),
    }
}

// -------------------------------------------------------------------------------- SHA-256
//
// The hash a download is checked against, in the crate that can be unit-tested. FIPS 180-4, the
// plain implementation: sta has no cryptography dependency and this is the only place it needs
// one — and a hash nobody can test is worse than one written out here against the standard's own
// vectors (see the tests).

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
    0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
    0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Streaming SHA-256: `update` it with the bytes as they arrive, `hex` when they stop.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    bits: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub const fn new() -> Sha256 {
        Sha256 {
            state: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            block: [0; 64],
            filled: 0,
            bits: 0,
        }
    }

    pub fn update(&mut self, mut bytes: &[u8]) {
        self.bits = self.bits.wrapping_add((bytes.len() as u64) * 8);
        while !bytes.is_empty() {
            let take = (64 - self.filled).min(bytes.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled == 64 {
                let block = self.block;
                self.compress(&block);
                self.filled = 0;
            }
        }
    }

    /// The digest as lowercase hex, which is how a manifest writes it.
    pub fn hex(mut self) -> String {
        // Padding: a 1 bit, zeroes, then the length in bits as a big-endian u64.
        let bits = self.bits;
        self.update_raw(&[0x80]);
        while self.filled != 56 {
            self.update_raw(&[0]);
        }
        self.update_raw(&bits.to_be_bytes());
        let mut out = String::with_capacity(64);
        for word in self.state {
            out.push_str(&format!("{word:08x}"));
        }
        out
    }

    /// `update` without counting the bytes (padding).
    fn update_raw(&mut self, bytes: &[u8]) {
        let saved = self.bits;
        self.update(bytes);
        self.bits = saved;
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

/// SHA-256 of `bytes`, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.hex()
}

/// What the browser is doing about updates, as the UI shows it (`UiState.update`).
///
/// The shell owns every transition (`crates/sta/src/update.rs`) and reports it with
/// [`crate::Command::UpdateStatusChanged`]; core only stores it and toasts the two states worth
/// interrupting somebody for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "stage", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UpdateStatus {
    /// Nothing has been checked yet this run (updates off, or too early).
    #[default]
    Idle,
    /// The manifest is being fetched.
    Checking,
    /// This build is the latest release. `checkedAt` is Unix ms.
    UpToDate { checked_at: i64 },
    /// A newer release exists and has not been fetched yet.
    Available { version: String, notes: String, size: u64 },
    /// …and is being downloaded. `total` is 0 while the size is unknown.
    Downloading { version: String, received: u64, total: u64 },
    /// The archive is downloaded, verified and unpacked: it is applied on the next restart.
    Ready { version: String },
    /// The last attempt failed. The message is for the About section, not a dialog.
    Failed { message: String },
}

impl UpdateStatus {
    /// The version this status is about, when it is about one.
    pub fn version(&self) -> Option<&str> {
        match self {
            UpdateStatus::Available { version, .. } | UpdateStatus::Downloading { version, .. } | UpdateStatus::Ready { version } => {
                Some(version)
            }
            _ => None,
        }
    }

    /// Whether a download may be started from this state (the UI's button, and the shell's guard).
    pub fn can_download(&self) -> bool {
        matches!(self, UpdateStatus::Available { .. }) || matches!(self, UpdateStatus::Failed { .. })
    }

    /// Whether the browser has an update staged for the next start.
    pub fn is_ready(&self) -> bool {
        matches!(self, UpdateStatus::Ready { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(url: &str) -> Asset {
        Asset { url: url.to_string(), sha256: "a".repeat(64), size: 1024 }
    }

    #[test]
    fn versions_order_like_semver() {
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.4", "1.2.3"), Ordering::Greater);
        assert_eq!(compare_versions("1.3.0", "1.2.9"), Ordering::Greater);
        assert_eq!(compare_versions("2.0", "1.99.99"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.0", "0.1.0.1"), Ordering::Less, "a fourth part counts");
        assert_eq!(compare_versions("1.2.0", "1.2"), Ordering::Equal, "missing parts are zero");
        assert_eq!(compare_versions("v1.2.3", "1.2.3"), Ordering::Equal, "a leading v is a tag, not a version");
        // A pre-release is older than its own release, and ordered by its tag between themselves.
        assert_eq!(compare_versions("1.2.0-beta.1", "1.2.0"), Ordering::Less);
        assert_eq!(compare_versions("1.2.0", "1.2.0-beta.1"), Ordering::Greater);
        assert_eq!(compare_versions("1.2.0-beta.2", "1.2.0-beta.1"), Ordering::Greater);
        // Nonsense never looks like an update.
        for bad in ["", "latest", "1.2.x", "1..2", "-1", "1.2.3-", "1.2.3.4.5"] {
            assert_eq!(compare_versions(bad, "1.0.0"), Ordering::Equal, "{bad:?}");
            assert_eq!(compare_versions("1.0.0", bad), Ordering::Equal, "{bad:?}");
        }
    }

    /// An .msi install and a portable copy are offered different files, and neither falls back to
    /// the other's.
    #[test]
    fn each_package_kind_has_its_own_manifest_entry() {
        let zip = asset(&format!("https://github.com/{REPOSITORY}/releases/download/v2.0.0/sta-2.0.0.zip"));
        let msi = asset(&format!("https://github.com/{REPOSITORY}/releases/download/v2.0.0/sta-2.0.0.msi"));
        let mut manifest = Manifest { version: "2.0.0".into(), pub_date: String::new(), notes: String::new(), platforms: BTreeMap::new() };
        manifest.platforms.insert(Package::Archive.key(), zip.clone());
        assert_eq!(Package::Msi.key(), format!("{}-msi", platform_key()));
        assert_eq!(manifest.asset_for(Package::Archive), Some(&zip));
        assert_eq!(manifest.asset(), Some(&zip));
        assert_eq!(manifest.asset_for(Package::Msi), None, "an archive cannot update Program Files");
        manifest.platforms.insert(Package::Msi.key(), msi.clone());
        assert_eq!(manifest.asset_for(Package::Msi), Some(&msi));
        assert_eq!(manifest.asset_for(Package::Archive), Some(&zip));
    }

    #[test]
    fn an_asset_is_trusted_only_from_this_repository_over_https() {
        let good = format!("https://github.com/{REPOSITORY}/releases/download/v1.2.3/sta-1.2.3-windows-x64.zip");
        assert!(asset(&good).trusted());
        assert_eq!(asset(&good).file_name(), "sta-1.2.3-windows-x64.zip");
        // The repository's old name still redirects, and old workflows wrote it.
        assert!(asset(&format!("https://github.com/{LEGACY_REPOSITORY}/releases/download/v1.2.3/x.zip")).trusted());
        assert!(!asset("https://github.com/P-Asta/other/releases/download/v1.2.3/x.zip").trusted());
        for bad in [
            "http://github.com/P-Asta/Astatine/releases/download/v1/x.zip",
            "https://github.com/someone-else/Astatine/releases/download/v1/x.zip",
            "https://evil.example/P-Asta/Astatine/releases/download/v1/x.zip",
            "https://github.com/P-Asta/Astatine/releases/download/../../x.zip",
        ] {
            assert!(!asset(bad).trusted(), "{bad}");
        }
        // The hash has to be a hash, and the size has to be plausible.
        assert!(!Asset { sha256: "abc".into(), ..asset(&good) }.trusted());
        assert!(!Asset { sha256: "A".repeat(64), ..asset(&good) }.trusted(), "uppercase hex is not what we write");
        assert!(!Asset { size: MAX_ARCHIVE_BYTES + 1, ..asset(&good) }.trusted());
        assert!(Asset { size: 0, ..asset(&good) }.trusted(), "a manifest without sizes is still usable");
    }

    #[test]
    fn a_manifest_names_this_platform_or_offers_nothing() {
        let body = format!(
            r#"{{"version":"9.9.9","pubDate":"2026-01-01T00:00:00Z","notes":"hi","platforms":{{
                 "{key}": {{"url":"https://github.com/{REPOSITORY}/releases/download/v9.9.9/sta.zip","sha256":"{hash}","size":10}},
                 "other-cpu": {{"url":"https://github.com/{REPOSITORY}/releases/download/v9.9.9/other.zip","sha256":"{hash}","size":10}}
               }}}}"#,
            key = platform_key(),
            hash = "b".repeat(64),
        );
        let manifest = Manifest::parse(body.as_bytes()).expect("parses");
        assert!(manifest.is_newer_than(env!("CARGO_PKG_VERSION")));
        assert_eq!(manifest.asset().map(|a| a.file_name()), Some("sta.zip"));
        assert_eq!(manifest.notes, "hi");
        // A manifest for platforms we are not offers nothing at all.
        let elsewhere = Manifest { platforms: BTreeMap::new(), ..manifest.clone() };
        assert!(elsewhere.asset().is_none());
        // …and neither does one whose asset is not ours.
        let mut tampered = manifest.clone();
        tampered.platforms.get_mut(platform_key()).unwrap().url = "https://evil.example/sta.zip".into();
        assert!(tampered.asset().is_none());
    }

    #[test]
    fn a_manifest_that_is_not_one_is_refused() {
        assert!(Manifest::parse(b"not json").is_err());
        assert!(Manifest::parse(br#"{"version":"latest","platforms":{}}"#).is_err());
        assert!(Manifest::parse(&vec![b'{'; MAX_MANIFEST_BYTES + 1]).is_err());
        // Nothing but the version is required: an older manifest without notes still parses.
        let ok = Manifest::parse(br#"{"version":"1.0.0"}"#).expect("parses");
        assert!(ok.notes.is_empty() && ok.platforms.is_empty() && ok.asset().is_none());
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        // FIPS 180-4 / NIST CAVP: the empty string, "abc", the 56-byte message (two blocks), and
        // a million 'a' (many blocks, and a length that needs the 64-bit counter).
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let mut hasher = Sha256::new();
        for _ in 0..1000 {
            hasher.update(&[b'a'; 1000]);
        }
        assert_eq!(hasher.hex(), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // The streaming API must agree with the one-shot one whatever the chunks are.
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        for chunk in [1usize, 7, 64, 63, 65, 1000] {
            let mut hasher = Sha256::new();
            for part in data.chunks(chunk) {
                hasher.update(part);
            }
            assert_eq!(hasher.hex(), sha256_hex(&data), "chunked by {chunk}");
        }
    }

    #[test]
    fn the_status_says_what_the_ui_may_do() {
        assert!(UpdateStatus::default() == UpdateStatus::Idle);
        let available = UpdateStatus::Available { version: "1.0.0".into(), notes: String::new(), size: 1 };
        assert!(available.can_download() && !available.is_ready() && available.version() == Some("1.0.0"));
        assert!(!UpdateStatus::Checking.can_download());
        assert!(UpdateStatus::Failed { message: "network".into() }.can_download(), "a failure can be retried");
        assert!(UpdateStatus::Ready { version: "1.0.0".into() }.is_ready());
        assert!(!UpdateStatus::Ready { version: "1.0.0".into() }.can_download(), "it is already here");
        // The JSON the UI sees is tagged by stage.
        let json = serde_json::to_string(&UpdateStatus::Downloading { version: "1.0.0".into(), received: 1, total: 2 }).unwrap();
        assert_eq!(json, r#"{"stage":"downloading","version":"1.0.0","received":1,"total":2}"#);
    }
}
