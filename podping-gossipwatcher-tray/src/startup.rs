use anyhow::{Context, Result};
use std::path::Path;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_SZ,
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "PodpingGossipWatcher";
pub const STARTUP_ARG: &str = "--minimized";

/// True when the process was launched with the startup registry argument.
pub fn launched_minimized() -> bool {
    std::env::args().any(|a| a == STARTUP_ARG)
}

/// Register or remove the tray app in the current-user Run key.
pub fn apply(enabled: bool) -> Result<()> {
    if enabled {
        let exe = std::env::current_exe().context("resolving tray executable path")?;
        set_run_value(&exe)
    } else {
        remove_run_value()
    }
}

fn set_run_value(exe: &Path) -> Result<()> {
    let command = to_wide(&format!("\"{}\" {}", exe.display(), STARTUP_ARG));
    let key_path = to_wide(RUN_KEY);
    let value_name = to_wide(VALUE_NAME);

    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_path.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if status != ERROR_SUCCESS {
            return Err(anyhow::anyhow!(
                "could not open Run registry key (error {})",
                status
            ));
        }

        let status = RegSetValueExW(
            hkey,
            value_name.as_ptr(),
            0,
            REG_SZ,
            command.as_ptr().cast(),
            (command.len() * 2) as u32,
        );
        RegCloseKey(hkey);

        if status != ERROR_SUCCESS {
            return Err(anyhow::anyhow!(
                "could not write startup registry value (error {})",
                status
            ));
        }
    }

    Ok(())
}

fn remove_run_value() -> Result<()> {
    let key_path = to_wide(RUN_KEY);
    let value_name = to_wide(VALUE_NAME);

    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_path.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if status != ERROR_SUCCESS {
            // Nothing to remove.
            return Ok(());
        }

        let status = RegDeleteValueW(hkey, value_name.as_ptr());
        RegCloseKey(hkey);

        // ERROR_FILE_NOT_FOUND — value was already absent.
        if status != ERROR_SUCCESS && status != 2 {
            return Err(anyhow::anyhow!(
                "could not remove startup registry value (error {})",
                status
            ));
        }
    }

    Ok(())
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
