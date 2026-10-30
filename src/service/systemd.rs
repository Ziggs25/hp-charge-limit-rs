//! What systemd says about the system and our service.

use std::process::Command;

pub const SERVICE: &str = "hp-charge-limit-rs.service";
/// `is-system-running` state during shutdown and reboot. A plain
/// `systemctl stop` of one service leaves it "running"/"degraded".
const STOPPING: &str = "stopping";

fn systemctl(args: &[&str]) -> Option<String> {
    let out = Command::new("systemctl").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// True during shutdown/reboot; also when systemd can't be asked, since a
/// hold is the safer mistake.
pub fn system_stopping() -> bool {
    systemctl(&["is-system-running"]).is_none_or(|state| state == STOPPING)
}

pub fn service_state() -> String {
    systemctl(&["is-active", SERVICE]).unwrap_or_else(|| "?".into())
}

/// Where the service's unit, drop-ins and environment come from, as
/// `Key=value` lines; None if systemctl failed.
pub fn service_config() -> Option<String> {
    let out = Command::new("systemctl")
        .args(["show", "--no-pager", "--property=LoadState,UnitFileState,FragmentPath,DropInPaths,ExecStart,EnvironmentFiles", SERVICE])
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}
