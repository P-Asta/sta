//! Windows Default Apps integration. Registration only advertises capabilities; Windows owns
//! the user's default associations. MSI registration is owned and removed by the installer.

use std::io;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

use super::browser_registration::portable_entries;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn read_string(root: HKEY, key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut data = vec![0u16; 32768];
    let mut size = (data.len() * 2) as u32;
    // SAFETY: NUL-terminated inputs and a valid output buffer, with its size in bytes.
    let status = unsafe {
        RegGetValueW(root, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), data.as_mut_ptr().cast(), &mut size)
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = data.iter().position(|c| *c == 0)?;
    String::from_utf16(&data[..len]).ok()
}

fn write_string(key: &str, name: &str, value: &str) -> io::Result<()> {
    let (key, name, value) = (wide(key), wide(name), wide(value));
    let mut handle = std::ptr::null_mut();
    // SAFETY: valid UTF-16 inputs and output handle; no security descriptor or class supplied.
    let status = unsafe {
        RegCreateKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE, std::ptr::null(), &mut handle, std::ptr::null_mut())
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    // SAFETY: live key, UTF-16 REG_SZ buffer including its terminating NUL; always close the key.
    let status = unsafe {
        let status = RegSetValueExW(handle, name.as_ptr(), 0, REG_SZ, value.as_ptr().cast(), (value.len() * 2) as u32);
        RegCloseKey(handle);
        status
    };
    if status == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(status as i32)) }
}

/// Called on a worker: ShellExecute can block. No elevation, no UserChoice writes.
pub fn open_default_browser_settings() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sta executable path is not valid Unicode"))?;
    let installed_command = format!("\"{exe}\" -- \"%1\"");
    let machine = read_string(HKEY_LOCAL_MACHINE, r"Software\RegisteredApplications", "sta")
        .as_deref() == Some(r"Software\Clients\StartMenuInternet\sta\Capabilities")
        && read_string(HKEY_LOCAL_MACHINE, r"Software\Classes\staHTML\shell\open\command", "")
            .is_some_and(|command| command.eq_ignore_ascii_case(&installed_command));
    let uri = if machine {
        "ms-settings:defaultapps?registeredAppMachine=sta"
    } else {
        for entry in portable_entries(exe) {
            write_string(&entry.key, &entry.name, &entry.value)?;
        }
        // SAFETY: ASSOCCHANGED requires no item pointers; notify Explorer of new capabilities.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) };
        "ms-settings:defaultapps?registeredAppUser=sta%20Portable"
    };
    if super::shell_open(uri) || super::shell_open("ms-settings:defaultapps") {
        Ok(())
    } else {
        Err(io::Error::other("Windows Default Apps could not be opened. Open Windows Settings > Apps > Default apps and select sta."))
    }
}
