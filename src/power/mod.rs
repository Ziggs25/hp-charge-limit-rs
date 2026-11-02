//! Battery and AC adapter readings: sysfs on Linux, the Windows power APIs
//! and WMI battery classes on Windows. Both report the same units.

#[cfg(target_os = "linux")]
mod sysfs;
#[cfg(target_os = "linux")]
pub use sysfs::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

use std::fmt;
use std::ops::Sub;

const MICRO_PER_MILLI: i64 = 1_000;
const MICRO_PER_UNIT: f64 = 1_000_000.0;

/// Battery energy in µWh, the unit of sysfs `energy_now`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Energy(i64);

impl Energy {
    pub const fn from_mwh(mwh: i64) -> Self {
        Self(mwh * MICRO_PER_MILLI)
    }
    pub const fn from_uwh(uwh: i64) -> Self {
        Self(uwh)
    }
    pub const fn uwh(self) -> i64 {
        self.0
    }
    pub const fn mwh(self) -> i64 {
        self.0 / MICRO_PER_MILLI
    }
    pub fn wh(self) -> f64 {
        self.0 as f64 / MICRO_PER_UNIT
    }
    pub const fn abs(self) -> Self {
        Self(self.0.abs())
    }
}

impl Sub for Energy {
    type Output = Energy;
    fn sub(self, rhs: Energy) -> Energy {
        Energy(self.0 - rhs.0)
    }
}

impl fmt::Display for Energy {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:.2} Wh", self.wh())
    }
}

/// Battery voltage in µV, the unit of sysfs `voltage_now`.
#[derive(Clone, Copy, Debug)]
pub struct Voltage(i64);

impl Voltage {
    pub const fn from_uv(uv: i64) -> Self {
        Self(uv)
    }
}

impl fmt::Display for Voltage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:.2} V", self.0 as f64 / MICRO_PER_UNIT)
    }
}

/// What the limiter needs each cycle.
#[derive(Clone, Copy, Debug)]
pub struct Reading {
    pub capacity: u8,
    pub on_ac: bool,
}

