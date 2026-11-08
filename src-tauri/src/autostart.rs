//! Launch at login, implemented straight against `HKCU\...\CurrentVersion\Run`.
//!
//! Written by hand instead of using an autostart plugin for one reason: the
//! command line has to be quoted. `"...\Teecy\Teecy.exe" --autostart` survives
//! an install folder with a space in it; an unquoted path silently turns into
//! "start C:\Program" at login and nothing happens.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Task Manager keeps its enable/disable decision for startup entries here.
/// Writing the "enabled" blob makes Teecy reappear there instead of staying
/// silently switched off by a past click.
const APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const APPROVED_ENABLED: [u8; 12] = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const VALUE_NAME: &str = "Teecy";
/// Tells the app this launch came from the Run key, not from the user.
pub const ARG: &str = "--autostart";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

pub fn launched_by_autostart() -> bool {
    std::env::args().any(|a| a == ARG)
}

fn command_line() -> String {
    format!("\"{}\" {}", exe_path(), ARG)
}

/// Current value of the Run entry, `None` when autostart is off.
pub fn read() -> Option<String> {
    unsafe {
        let mut key = HKEY::default();
        let sub = wide(RUN_KEY);
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let name = wide(VALUE_NAME);
        let mut ty = REG_SZ;
        let mut size = 0u32;
        let probe = RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut size),
        );
        if probe != ERROR_SUCCESS || size == 0 {
            let _ = RegCloseKey(key);
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let res = RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        );
        let _ = RegCloseKey(key);
        if res != ERROR_SUCCESS {
            return None;
        }
        buf.truncate(size as usize);
        let u16s: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|c| *c != 0)
            .collect();
        Some(String::from_utf16_lossy(&u16s))
    }
}

pub fn is_enabled() -> bool {
    read().is_some()
}

pub fn enable() -> Result<(), String> {
    let value = command_line();
    let bytes: Vec<u8> = value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    unsafe {
        // The Run key is created by Windows itself, so opening it is enough and
        // we never need the security-attributes API.
        let mut key = HKEY::default();
        let sub = wide(RUN_KEY);
        let rc = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut key,
        );
        if rc != ERROR_SUCCESS {
            return Err(format!("无法打开启动项（错误码 {}）", rc.0));
        }
        let name = wide(VALUE_NAME);
        let res = RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(&bytes));
        let _ = RegCloseKey(key);
        if res != ERROR_SUCCESS {
            return Err(format!("无法写入启动项（错误码 {}）", res.0));
        }
    }
    clear_startup_override();
    Ok(())
}

pub fn disable() -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        let sub = wide(RUN_KEY);
        let rc = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut key,
        );
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if rc != ERROR_SUCCESS {
            return Err(format!("无法打开启动项（错误码 {}）", rc.0));
        }
        let name = wide(VALUE_NAME);
        let res = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
        let _ = RegCloseKey(key);
        if res != ERROR_SUCCESS && res != ERROR_FILE_NOT_FOUND {
            return Err(format!("无法删除启动项（错误码 {}）", res.0));
        }
    }
    Ok(())
}

/// Drops a stale Task-Manager "disabled" flag so the entry we just wrote is
/// actually allowed to run.
fn clear_startup_override() {
    unsafe {
        let mut key = HKEY::default();
        let sub = wide(APPROVED_KEY);
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut key,
        ) != ERROR_SUCCESS
        {
            return;
        }
        let name = wide(VALUE_NAME);
        let res = RegSetValueExW(
            key,
            PCWSTR(name.as_ptr()),
            0,
            windows::Win32::System::Registry::REG_BINARY,
            Some(&APPROVED_ENABLED),
        );
        let _ = RegCloseKey(key);
        if res != ERROR_SUCCESS {
            // Not fatal: the Run entry alone is enough unless the user had
            // switched the item off in Task Manager.
        }
    }
}

/// Called on every start. Reinstalling into another folder leaves the Run
/// entry pointing at a path that no longer exists, which looks exactly like
/// "autostart is broken"; rewrite it when the target moved.
pub fn heal() -> bool {
    let Some(current) = read() else { return false };
    if current == command_line() {
        return false;
    }
    enable().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trips the real HKCU Run entry, restoring whatever state the
    /// machine had before the test ran.
    #[test]
    fn enable_disable_roundtrip() {
        let was_enabled = is_enabled();
        enable().expect("enable");
        assert!(is_enabled(), "entry should exist after enable");
        assert_eq!(
            read().unwrap(),
            command_line(),
            "entry must carry the quoted exe path"
        );
        disable().expect("disable");
        assert!(!is_enabled(), "entry should be gone after disable");
        if was_enabled {
            enable().unwrap();
        }
    }
}

