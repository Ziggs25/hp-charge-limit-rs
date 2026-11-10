//! Events the limiter reports. The daemon also broadcasts them as D-Bus
//! signals (see `dbus`).

mod dbus;
pub use dbus::*;

use crate::firmware::Mode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Charging was blocked at the stop threshold
    LimitReached { capacity: u8 },
    /// Charging was allowed again at the start threshold
    ChargingResumed { capacity: u8 },
    /// Bypass took effect (true) or ended (false)
    BypassChanged { enabled: bool },
    /// A firmware write failed; sent once per run of failures
    WriteFailed { mode: Mode, error: String },
}
