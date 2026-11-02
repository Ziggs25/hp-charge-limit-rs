//! System paths, all resolved through `at` so tests can redirect them.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
pub const POWER_SUPPLY: &str = "/sys/class/power_supply";
#[cfg(target_os = "linux")]
pub const ACPI_CALL: &str = "/proc/acpi/call";
#[cfg(target_os = "linux")]
pub const OFF_STATE: &str = "/var/lib/hp-charge-limit-rs/off-state";
#[cfg(target_os = "linux")]
pub const LOCK: &str = "/run/hp-charge-limit-rs.lock";
#[cfg(target_os = "linux")]
pub const BYPASS: &str = "/run/hp-charge-limit-rs.bypass";

/// On Windows these live in the data directory (see `at`); the bypass
/// flag is a volatile registry key there (see `bypass`).
#[cfg(windows)]
pub const OFF_STATE: &str = "off-state";
#[cfg(windows)]
pub const LOG: &str = "daemon.log";

/// Prefixed to every path when set, to run against fake sysfs/procfs files.
/// On Windows it replaces the data directory instead.
const TEST_ROOT_VAR: &str = "HP_CHARGE_LIMIT_RS_TEST_ROOT";

#[cfg(target_os = "linux")]
pub fn at(path: &str) -> PathBuf {
    PathBuf::from(std::env::var(TEST_ROOT_VAR).unwrap_or_default() + path)
}

/// `%ProgramData%\hp-charge-limit-rs\<name>`.
#[cfg(windows)]
pub fn at(name: &str) -> PathBuf {
    data_dir().join(name)
}

#[cfg(windows)]
pub fn data_dir() -> PathBuf {
    if let Some(root) = std::env::var_os(TEST_ROOT_VAR) {
        return PathBuf::from(root);
    }
    let program_data = std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into());
    PathBuf::from(program_data).join("hp-charge-limit-rs")
}

/// The data directory, created or checked so only SYSTEM and
/// Administrators can change it (see `acl`). Call before writing there.
#[cfg(windows)]
pub fn secure_data_dir() -> anyhow::Result<PathBuf> {
    let dir = data_dir();
    if std::env::var_os(TEST_ROOT_VAR).is_some() {
        fs::create_dir_all(&dir)?;
    } else {
        crate::acl::secure_dir(&dir)?;
    }
    Ok(dir)
}

/// Makes sure the directory `path` will be written to exists.
#[cfg(target_os = "linux")]
pub fn ensure_dir_for(path: &Path) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn ensure_dir_for(_path: &Path) -> anyhow::Result<()> {
    secure_data_dir().map(drop)
}

pub fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}
