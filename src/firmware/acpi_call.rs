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
///
/// On HP Victus 16-e0xxx (board 88ED), `\_SB.WMID.SBCC` takes four integer
/// arguments (`/tmp/dsdt.dsl:8416-8522`):
///
/// - `SBCC 0 0 0 0`: auto; clears MBDC. Works on AC or battery.
/// - `SBCC 0 1 0 0`: true inhibit; rejected with `0x36` while charging.
/// - `SBCC 0 2 0 0`: force-discharge; never send.
/// - `SBCC 0x63 0 0 0`: auto; requires AC (`ADPP == 1`).
/// - `SBCC 0x63 1 0 0`: force-discharge (GBCC reports 2); never send.
/// - `SBCC 0x63 2 0 0`: hold (GBCC reports 3); requires AC.
///
/// The EC turns hold into `MBDC=0x1C`, isolates the battery from charging, and
/// Linux reports `Not charging`. Because true inhibit is rejected while
/// charging, both `Mode::Inhibit` and `Mode::Hold` deliberately use the native
/// hold command on this board.
fn sbcc_command(mode: Mode) -> String {
    let args = match mode {
        Mode::Auto => "0 0 0 0",
        Mode::Inhibit | Mode::Hold => "0x63 2 0 0",
    };
    format!("{SBCC} {args}")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_uses_the_native_zero_command() {
        assert_eq!(sbcc_command(Mode::Auto), r"\_SB.WMID.SBCC 0 0 0 0");
    }

    #[test]
    fn inhibit_and_hold_use_hold_but_never_force_discharge() {
        assert_eq!(sbcc_command(Mode::Inhibit), r"\_SB.WMID.SBCC 0x63 2 0 0");
        assert_eq!(sbcc_command(Mode::Hold), r"\_SB.WMID.SBCC 0x63 2 0 0");
    }
}
