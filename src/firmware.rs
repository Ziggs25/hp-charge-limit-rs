//! The WMI battery-charge-control methods SBCC (set) and GBCC (get),
//! called through the acpi_call module.

use crate::paths::{self, ACPI_CALL, LOCK};
use anyhow::{Context, Result, anyhow, ensure};
use clap::ValueEnum;
use std::fmt;
use std::fs::{self, File};
use std::io::Read;

const SBCC: &str = r"\_SB.WMID.SBCC";
const GBCC: &str = r"\_SB.WMID.GBCC";
/// SBCC takes a 20-byte buffer with the mode as a little-endian dword here.
const SBCC_BUFFER_LEN: usize = 20;
const SBCC_MODE_OFFSET: usize = 0x10;
/// Larger than any reply of the methods we call.
const ACPI_REPLY_MAX: usize = 4096;
/// acpi_call reports failures as a reply text starting with this.
const ACPI_ERROR_PREFIX: &str = "Error";

/// The modes this tool writes, as their SBCC mode dword.
/// Others exist (0x0200 force-discharge, 0x0600/0x0700 ship mode) and are
/// deliberately not representable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[repr(u32)]
pub enum Mode {
    /// Normal charging; clears WACL/SBIS/SBTC/WPBS
    Auto = 0x0000,
    /// SBIS: no charging, white LED; the EC clears it at shutdown
    Inhibit = 0x0500,
    /// SBTC: no charging, survives shutdown; LED/status still say charging
    Hold = 0x0300,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let value = self.to_possible_value().expect("no skipped variants");
        f.write_str(value.get_name())
    }
}

/// The method call that sets `mode`.
pub fn sbcc_command(mode: Mode) -> String {
    let mut buf = [0u8; SBCC_BUFFER_LEN];
    let word = (mode as u32).to_le_bytes();
    buf[SBCC_MODE_OFFSET..SBCC_MODE_OFFSET + word.len()].copy_from_slice(&word);
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!("{SBCC} b{hex}")
}

/// GBCC mode byte values. GBCC derives them from the EC charge state and
/// flags (last match wins); mainline hp-wmi reads the same byte on Spectre
/// boards (GBCO) and names 1, 5 and 6 the same way.
mod reported {
    pub const AUTO: u8 = 0;
    /// Auto while charging is in progress
    pub const CHARGING: u8 = 1;
    /// The battery is discharging: running on battery, or force-discharge
    pub const DISCHARGING: u8 = 2;
    pub const HOLD: u8 = 3;
    pub const INHIBIT: u8 = 4;
    /// Inhibit variant (firmware-specific flag)
    pub const INHIBIT_VARIANT: u8 = 5;
    pub const INHIBIT_SHIP_ARMED: u8 = 6;
    /// GBCC found no battery
    pub const NO_BATTERY: u8 = 0xFF;
}

/// The mode byte GBCC reports (first byte of its buffer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reported(pub u8);

impl Reported {
    pub fn name(self) -> &'static str {
        match self.0 {
            reported::AUTO => "auto",
            reported::CHARGING => "auto (charging)",
            reported::DISCHARGING => "discharging (on battery, or force-discharge)",
            reported::HOLD => "hold (SBTC)",
            reported::INHIBIT => "inhibit (SBIS)",
            reported::INHIBIT_VARIANT => "inhibit variant",
            reported::INHIBIT_SHIP_ARMED => "inhibit/ship-mode armed",
            reported::NO_BATTERY => "no battery",
            _ => "unknown",
        }
    }
    pub fn is_hold(self) -> bool {
        self.0 == reported::HOLD
    }
}

impl fmt::Display for Reported {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.0)
    }
}

/// GBCC replies e.g. `[0x0, 0x4, {0x03, 0xff, 0x01, 0x00}]`.
pub fn parse_gbcc(reply: &str) -> Result<Reported> {
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

/// Something that can switch the firmware charge mode.
pub trait ChargeControl {
    fn set(&mut self, mode: Mode) -> Result<()>;
}

/// The real firmware, through acpi_call.
pub struct Wmi;

impl Wmi {
    pub fn read(&self) -> Result<Reported> {
        parse_gbcc(&acpi_call(GBCC)?)
    }
}

impl ChargeControl for Wmi {
    fn set(&mut self, mode: Mode) -> Result<()> {
        acpi_call(&sbcc_command(mode)).map(drop)
    }
}
