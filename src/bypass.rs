//! Temporary bypass flag, cleared automatically at reboot.

use crate::cli::BypassAction;
use crate::paths::{self, BYPASS};
use anyhow::{Context, Result};
use std::fs;
use std::io::ErrorKind;

pub fn enabled() -> bool {
    paths::at(BYPASS).exists()
}

pub fn set(action: BypassAction) -> Result<()> {
    let path = paths::at(BYPASS);
    match action {
        BypassAction::On => fs::write(&path, "").with_context(|| path.display().to_string())?,
        BypassAction::Off => match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| path.display().to_string()),
        },
    }
    let state = if enabled() { "enabled" } else { "disabled" };
    println!("bypass {state}; the running daemon applies the change on its next poll");
    Ok(())
}
