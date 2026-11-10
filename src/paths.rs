//! System paths, all resolved through `at` so tests can redirect them.

use std::fs;
use std::path::{Path, PathBuf};

pub const POWER_SUPPLY: &str = "/sys/class/power_supply";
pub const ACPI_CALL: &str = "/proc/acpi/call";
pub const OFF_STATE: &str = "/var/lib/hp-charge-limit-rs/off-state";
pub const LOCK: &str = "/run/hp-charge-limit-rs.lock";
pub const BYPASS: &str = "/run/hp-charge-limit-rs.bypass";

/// Prefixed to every path when set, to run against fake sysfs/procfs files.
const TEST_ROOT_VAR: &str = "HP_CHARGE_LIMIT_RS_TEST_ROOT";

pub fn at(path: &str) -> PathBuf {
    PathBuf::from(std::env::var(TEST_ROOT_VAR).unwrap_or_default() + path)
}

/// Makes sure the directory `path` will be written to exists.
pub fn ensure_dir_for(path: &Path) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

pub fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}
