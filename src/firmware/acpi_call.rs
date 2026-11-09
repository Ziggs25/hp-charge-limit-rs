//! Linux backend: calls `\_SB.WMID.SBCC`/`GBCC` directly through the
//! acpi_call module.

use super::{Mode, Reported};
use crate::paths::{self, ACPI_CALL, LOCK};
use anyhow::{Context, Result, anyhow, ensure};
use std::fs::{self, File};
use std::io::Read;

const SBCC: &str = r"\_SB.WMID.SBCC";
const GBCC: &str = r"\_SB.WMID.GBCC";
/// Larger than any reply of the methods we call.
const ACPI_REPLY_MAX: usize = 4096;
/// acpi_call reports failures as a reply text starting with this.
const ACPI_ERROR_PREFIX: &str = "Error";

/// The method call that sets `mode`.
fn sbcc_command(mode: Mode) -> String {
    let arg1 = match mode {
        Mode::Auto => 0,
        Mode::Inhibit => 1,
        Mode::Hold => 2,
    };
    format!("{SBCC} 0x63 {arg1} 0 0")
}

/// GBCC replies e.g. `[0x0, 0x4, {0x03, 0xff, 0x01, 0x00}]`.
fn parse_gbcc(reply: &str) -> Result<Reported> {
    let bad = || anyhow!("unexpected GBCC reply {reply}");
    let first = reply.split('{').nth(1).and_then(|s| s.split(',').next()).ok_or_else(bad)?;
    let byte = u8::from_str_radix(first.trim().trim_start_matches("0x"), 16).map_err(|_| bad())?;
    Ok(Reported(byte))
}

/// One ACPI method call through acpi_call; returns its reply text.
fn acpi_call(cmd: &str) -> Result<String> {
    let call = paths::at(ACPI_CALL);
    // acpi_call has one reply buffer for everyone: keep other instances
    // (daemon vs status) out between our write and read
    let _lock = File::create(paths::at(LOCK)).and_then(|f| f.lock().map(|()| f));
    fs::write(&call, cmd)
        .with_context(|| format!("{} (root and the acpi_call module needed)", call.display()))?;
    // acpi_call resets the reply after every read(), so take it in one read;
    // read_to_string's small first probe read would truncate and lose it
    let mut buf = vec![0u8; ACPI_REPLY_MAX];
    let n = File::open(&call)
        .and_then(|mut f| f.read(&mut buf))
        .with_context(|| call.display().to_string())?;
    let reply = String::from_utf8_lossy(&buf[..n]);
    let reply = reply.trim_matches(char::from(0)).trim().to_string();
    ensure!(!reply.starts_with(ACPI_ERROR_PREFIX), "{cmd}: {reply}");
    Ok(reply)
}

pub(super) fn read() -> Result<Reported> {
    parse_gbcc(&acpi_call(GBCC)?)
}

pub(super) fn set(mode: Mode) -> Result<()> {
    acpi_call(&sbcc_command(mode)).map(drop)
}
