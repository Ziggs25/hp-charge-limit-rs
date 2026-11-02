//! Temporary bypass flag, cleared automatically at reboot: a file under
//! /run on Linux, a volatile registry key on Windows (kept in memory only,
//! so a restart drops it; a Fast Startup "shut down" hibernates and keeps
//! it).

use crate::cli::BypassAction;
use anyhow::Result;

#[cfg(target_os = "linux")]
mod store {
    use crate::paths::{self, BYPASS};
    use anyhow::{Context, Result};
    use std::fs;
    use std::io::ErrorKind;

    pub fn enabled() -> bool {
        paths::at(BYPASS).exists()
    }

    pub fn on() -> Result<()> {
        let path = paths::at(BYPASS);
        fs::write(&path, "").with_context(|| path.display().to_string())
    }

    pub fn off() -> Result<()> {
        let path = paths::at(BYPASS);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| path.display().to_string()),
        }
    }
}

#[cfg(windows)]
mod store {
    use anyhow::{Context, Result};
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_OPTION_VOLATILE, RegCloseKey, RegCreateKeyExW,
        RegDeleteKeyW, RegOpenKeyExW,
    };
    use windows::core::{PCWSTR, w};

    /// Writable by administrators only, like HKLM\SOFTWARE.
    const KEY: PCWSTR = w!(r"SOFTWARE\hp-charge-limit-rs-bypass");
    const KEY_NAME: &str = r"HKLM\SOFTWARE\hp-charge-limit-rs-bypass";

    pub fn enabled() -> bool {
        let mut key = HKEY::default();
        let found = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, KEY, None, KEY_READ, &mut key) }.is_ok();
        if found {
            unsafe {
                let _ = RegCloseKey(key);
            }
        }
        found
    }

    pub fn on() -> Result<()> {
        let mut key = HKEY::default();
        unsafe {
            RegCreateKeyExW(HKEY_LOCAL_MACHINE, KEY, None, PCWSTR::null(), REG_OPTION_VOLATILE, KEY_WRITE, None, &mut key, None)
                .ok()
                .with_context(|| format!("create {KEY_NAME} (run as administrator)"))?;
            let _ = RegCloseKey(key);
        }
        Ok(())
    }

    pub fn off() -> Result<()> {
        match unsafe { RegDeleteKeyW(HKEY_LOCAL_MACHINE, KEY) } {
            e if e.is_ok() || e == ERROR_FILE_NOT_FOUND => Ok(()),
            e => e.ok().with_context(|| format!("delete {KEY_NAME} (run as administrator)")),
        }
    }
}

pub fn enabled() -> bool {
    store::enabled()
}

pub fn set(action: BypassAction) -> Result<()> {
    match action {
        BypassAction::On => store::on()?,
        BypassAction::Off => store::off()?,
    }
    let state = if enabled() { "enabled" } else { "disabled" };
    println!("bypass {state}; the running daemon applies the change on its next poll");
    Ok(())
}
