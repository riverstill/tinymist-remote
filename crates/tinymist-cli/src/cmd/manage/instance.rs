//! The single-instance guard and the autostart registration.

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex};

/// The name of the mutex guarding a single manager instance.
const MUTEX_NAME: &str = "Local\\TinymistServiceManager";

/// The registry path of the autostart entries.
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// The name of the autostart value.
const RUN_VALUE: &str = "TinymistServiceManager";

/// Encodes a string as a null terminated UTF-16 sequence.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The guard keeping a single manager instance alive.
pub struct InstanceGuard {
    handle: *mut core::ffi::c_void,
}

impl InstanceGuard {
    /// Acquires the single instance guard.
    ///
    /// Returns `None` if another manager instance is already running.
    pub fn acquire() -> Option<Self> {
        // SAFETY: the mutex name is a valid null terminated wide string and the
        // returned handle is properly released in `Drop`.
        unsafe {
            let name = to_wide(MUTEX_NAME);
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                return None;
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                ReleaseMutex(handle);
                CloseHandle(handle);
                return None;
            }
            Some(Self { handle })
        }
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: the handle was acquired by `CreateMutexW` and is not used
        // anywhere else.
        unsafe {
            ReleaseMutex(self.handle);
            CloseHandle(self.handle);
        }
    }
}

/// Returns the command registered for autostarting, if any.
pub fn autostart_command() -> Option<String> {
    read_reg_sz(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE)
}

/// Enables or disables the autostart registration.
pub fn set_autostart(enable: bool, command: &str) {
    if enable {
        write_reg_sz(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, command);
    } else {
        delete_reg_value(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
    }
}

/// Reads a `REG_SZ` value from the registry.
fn read_reg_sz(hkey: HKEY, subkey: &str, value: &str) -> Option<String> {
    let subkey = to_wide(subkey);
    let value = to_wide(value);
    // SAFETY: the key and value names are valid null terminated wide strings
    // and the buffers are sized according to the API contract.
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        let err = RegOpenKeyExW(hkey, subkey.as_ptr(), 0, KEY_READ, &mut key);
        if err != 0 {
            return None;
        }
        let mut size = 0u32;
        let err = RegGetValueW(
            key,
            std::ptr::null(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        );
        if err != 0 || size < 2 {
            RegCloseKey(key);
            return None;
        }

        let mut buffer = vec![0u16; (size / 2) as usize];
        let err = RegGetValueW(
            key,
            std::ptr::null(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        );
        RegCloseKey(key);
        if err != 0 {
            return None;
        }
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..len]))
    }
}

/// Writes a `REG_SZ` value to the registry.
fn write_reg_sz(hkey: HKEY, subkey: &str, value: &str, data: &str) {
    let subkey = to_wide(subkey);
    let value = to_wide(value);
    let mut wide: Vec<u16> = data.encode_utf16().collect();
    wide.push(0);
    // SAFETY: the key and value names are valid null terminated wide strings
    // and the data buffer matches the passed byte length.
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        let err = RegCreateKeyExW(
            hkey,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        );
        if err != 0 {
            log::error!("failed to open the autostart registry key: {err}");
            return;
        }
        let err = RegSetValueExW(
            key,
            value.as_ptr(),
            0,
            REG_SZ,
            wide.as_ptr().cast(),
            (wide.len() * 2) as u32,
        );
        RegCloseKey(key);
        if err != 0 {
            log::error!("failed to write the autostart registry value: {err}");
        }
    }
}

/// Deletes a value from the registry.
fn delete_reg_value(hkey: HKEY, subkey: &str, value: &str) {
    let subkey = to_wide(subkey);
    let value = to_wide(value);
    // SAFETY: the key and value names are valid null terminated wide strings.
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        let err = RegOpenKeyExW(hkey, subkey.as_ptr(), 0, KEY_WRITE, &mut key);
        if err != 0 {
            return;
        }
        RegDeleteValueW(key, value.as_ptr());
        RegCloseKey(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_to_wide() {
        assert_eq!(to_wide("ab"), vec![b'a' as u16, b'b' as u16, 0]);
    }
}
