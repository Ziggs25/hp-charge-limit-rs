//! The WMI battery-charge-control methods SBCC (set) and GBCC (get).
//!
//! Both platforms reach the same firmware code: Linux calls the ACPI
//! methods directly through acpi_call, Windows goes through the HP BIOS WMI
//! class `hpqBIntM`, whose dispatcher (WHCM) routes command type 0x2B to
//! GBCC (read) and SBCC (write).

#[cfg(target_os = "linux")]
mod acpi_call;
#[cfg(target_os = "linux")]
use acpi_call as backend;
#[cfg(windows)]
mod hpqbint;
#[cfg(windows)]
use hpqbint as backend;
#[cfg(windows)]
pub use hpqbint::interface;

use anyhow::Result;
use clap::ValueEnum;
use std::fmt;

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
    /// SBTC: no charging, survives shutdown; on board 88ED Linux reports
    /// "Not charging" with a white LED, but other boards may say charging
    Hold = 0x0300,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let value = self.to_possible_value().expect("no skipped variants");
        f.write_str(value.get_name())
    }
}

/// The 4 data bytes SBCC reads: the mode as a little-endian dword.
#[allow(dead_code)]
fn mode_data(mode: Mode) -> [u8; 4] {
    (mode as u32).to_le_bytes()
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
    pub fn matches(self, mode: Mode) -> bool {
        match mode {
            Mode::Auto => matches!(self.0, reported::AUTO | reported::CHARGING),
            // Board 88ED has no usable inhibit-while-charging state, so the
            // backend writes hold for both modes and GBCC reports 3. Byte 1 is
            // the firmware's true inhibit state; 4/5 are inhibit variants.
            Mode::Inhibit => {
                self.is_hold()
                    || matches!(
                        self.0,
                        reported::CHARGING | reported::INHIBIT | reported::INHIBIT_VARIANT
                    )
            }
            Mode::Hold => self.is_hold(),
        }
    }
}

impl fmt::Display for Reported {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.0)
    }
}

/// Something that can switch the firmware charge mode.
pub trait ChargeControl {
    fn set(&mut self, mode: Mode) -> Result<()>;
    fn read(&self) -> Result<Reported>;
}

/// The real firmware.
pub struct Wmi;

impl Wmi {
    pub fn read(&self) -> Result<Reported> {
        backend::read()
    }
}

impl ChargeControl for Wmi {
    fn set(&mut self, mode: Mode) -> Result<()> {
        backend::set(mode)
    }
    fn read(&self) -> Result<Reported> {
        backend::read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_readback_satisfies_both_requested_modes() {
        assert!(Reported(3).matches(Mode::Inhibit));
        assert!(Reported(3).matches(Mode::Hold));
    }

    #[test]
    fn force_discharge_is_never_accepted_as_success() {
        assert!(!Reported(2).matches(Mode::Inhibit));
        assert!(!Reported(2).matches(Mode::Hold));
    }

    #[test]
    fn true_inhibit_readback_still_satisfies_inhibit() {
        assert!(Reported(1).matches(Mode::Inhibit));
        assert!(Reported(4).matches(Mode::Inhibit));
        assert!(Reported(5).matches(Mode::Inhibit));
        assert!(!Reported(1).matches(Mode::Hold));
    }
}
