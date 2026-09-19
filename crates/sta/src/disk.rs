//! Low disk space, said where it matters [owner: tabs].
//!
//! Chromium installs an extension by unpacking its CRX into the profile
//! (`User Data/Default/Extensions/Temp/…`) and then indexing its blocking rules next to it. On a
//! full disk both steps fail, and what the Web Store shows for it is "Could not unzip extension" or
//! "Package is invalid … Internal error while parsing rules" — nothing about the disk, so it reads
//! as a broken extension or a broken browser. It happened with 0.33 GB free: AdBlock is a 78 MB
//! download that unpacks to 340 MB, and it failed three times in a row while smaller extensions
//! installed fine in between.
//!
//! sta cannot see Chromium's install error (the Alloy runtime reports none), but it can see the
//! cause: whenever a tab lands on a Chrome Web Store page, the volume that holds the profile is
//! asked for its free space, and under [`LOW_BYTES`] core is told (`Command::LowDiskSpace`), which
//! toasts once per run.
//!
//! Public API:
//! - `pub fn on_tab_address(url: &str)` — `DisplayHandler::on_address_change` of a tab (UI thread)

use crate::{controller, paths, platform};
use sta_core::Command;

/// Below this, a large extension may not fit: its download, the unpacked copy in `Temp`, the
/// installed copy and the indexed rulesets exist side by side for a moment.
const LOW_BYTES: u64 = 1024 * 1024 * 1024;

/// A tab committed `url`. Cheap for everything that is not a store page (one host comparison), and
/// one `GetDiskFreeSpaceExW` for those that are.
pub fn on_tab_address(url: &str) {
    if !sta_core::urls::is_web_store_url(url) {
        return;
    }
    let Some(dirs) = paths::try_dirs() else { return };
    let Some(free) = platform::free_disk_bytes(&dirs.user_data) else { return };
    if let Some(free_mb) = low_mb(free) {
        log_warn!("disk: {free_mb} MB free on the profile's volume; extension installs may fail");
        controller::dispatch(Command::LowDiskSpace { free_mb });
    }
}

/// `Some(free space in MB)` when `free` bytes is low enough to say so.
fn low_mb(free: u64) -> Option<u64> {
    (free < LOW_BYTES).then_some(free / (1024 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_nearly_full_disk_is_reported() {
        // What the failed installs actually had: 0.33 GB.
        assert_eq!(low_mb(342 * 1024 * 1024), Some(342));
        assert_eq!(low_mb(0), Some(0));
        assert_eq!(low_mb(LOW_BYTES - 1), Some(1023));
        assert_eq!(low_mb(LOW_BYTES), None);
        assert_eq!(low_mb(13 * LOW_BYTES), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_answers_for_a_real_directory() {
        let free = platform::free_disk_bytes(&std::env::temp_dir());
        assert!(free.is_some(), "GetDiskFreeSpaceExW refused the temp directory");
    }
}
