//! Setup diagnostics: queries only, never SBCC writes or service changes.

use crate::firmware::Wmi;
#[cfg(target_os = "linux")]
use crate::paths;
use crate::{power, service};
use anyhow::{Result, ensure};
use std::fmt::Display;
#[cfg(target_os = "linux")]
use std::fs::OpenOptions;

fn report(mark: &str, label: impl Display, detail: impl Display) {
    println!("[{mark}] {label}: {detail}");
}

/// Reports a pass/fail check; returns `ok`.
fn check(ok: bool, label: impl Display, detail: impl Display) -> bool {
    report(if ok { "ok" } else { "fail" }, label, detail);
    ok
}

/// Vendor, model, board and BIOS version, as far as readable.
struct Machine {
    vendor: Option<String>,
    model: Option<String>,
    board: Option<String>,
    bios: Option<String>,
}

#[cfg(target_os = "linux")]
fn machine() -> Machine {
    let dmi = |name: &str| paths::read_trimmed(&paths::at(&format!("/sys/class/dmi/id/{name}")));
    Machine {
        vendor: dmi("sys_vendor"),
        model: dmi("product_name"),
        board: dmi("board_name"),
        bios: dmi("bios_version"),
    }
}

#[cfg(windows)]
fn machine() -> Machine {
    let cimv2 = crate::wmi::Namespace::connect(r"root\cimv2").ok();
    let first = |class: &str| cimv2.as_ref()?.first(class).ok().flatten();
    let (system, board, bios) = (
        first("Win32_ComputerSystem"),
        first("Win32_BaseBoard"),
        first("Win32_BIOS"),
    );
    Machine {
        vendor: system.as_ref().and_then(|o| o.string("Manufacturer")),
        model: system.as_ref().and_then(|o| o.string("Model")),
        board: board.as_ref().and_then(|o| o.string("Product")),
        bios: bios.as_ref().and_then(|o| o.string("SMBIOSBIOSVersion")),
    }
}

/// Reported only: what testers should include in a report.
fn identify() {
    let m = machine();
    let unknown = || "?".to_string();
    report(
        "info",
        "machine",
        format!(
            "{} {}, board {}, BIOS {}",
            m.vendor.unwrap_or_else(unknown),
            m.model.unwrap_or_else(unknown),
            m.board.unwrap_or_else(unknown),
            m.bios.unwrap_or_else(unknown)
        ),
    );
}

pub fn run() -> Result<()> {
    identify();
    let mut ok = supplies();
    ok &= firmware();
    service();
    println!(
        "No charge modes changed. Readback does not prove firmware compatibility or charge blocking."
    );
    ensure!(ok, "diagnostics found problems; see checks above");
    Ok(())
}

#[cfg(target_os = "linux")]
fn supplies() -> bool {
    let mut ok = match power::battery() {
        Some(path) => {
            report("ok", "battery", path.display());
            match power::capacity().filter(|&capacity| capacity <= 100) {
                Some(capacity) => check(true, "capacity", format!("{capacity}%")),
                None => check(false, "capacity", "missing or invalid sysfs capacity"),
            }
        }
        None => check(false, "battery", "no present system battery found"),
    };
    let adapters = power::adapters();
    ok &= check(
        !adapters.is_empty(),
        "external power supplies",
        adapters.len(),
    );
    for adapter in adapters {
        let (online, detail) = match paths::read_trimmed(&adapter.join("online")).as_deref() {
            Some("1") => (true, "online"),
            Some("0") => (true, "offline (normal when unplugged)"),
            _ => (false, "missing or invalid online attribute"),
        };
        ok &= check(online, adapter.display(), detail);
    }
    ok
}

#[cfg(windows)]
fn supplies() -> bool {
    let mut ok = check(
        power::present(),
        "battery",
        if power::present() {
            "present"
        } else {
            "no system battery"
        },
    );
    match power::capacity().filter(|&capacity| capacity <= 100) {
        Some(capacity) => ok &= check(true, "capacity", format!("{capacity}%")),
        None => ok &= check(false, "capacity", "unknown"),
    }
    let ac = match power::ac_status() {
        Some(true) => (true, "online"),
        Some(false) => (true, "offline (normal when unplugged)"),
        None => (false, "unknown"),
    };
    ok &= check(ac.0, "AC adapter", ac.1);
    let details = power::energy().is_some();
    report(
        if details { "ok" } else { "warn" },
        "battery details (root\\wmi BatteryStatus)",
        if details {
            "readable"
        } else {
            "unreadable; status shows less, limiting still works"
        },
    );
    ok
}

#[cfg(target_os = "linux")]
fn firmware() -> bool {
    let call = paths::at(paths::ACPI_CALL);
    if !call.exists() {
        report(
            "fail",
            "acpi_call",
            "interface missing; install/load acpi_call for the running kernel",
        );
        report(
            "skip",
            "permissions and firmware readback",
            "ACPI interface missing",
        );
        return false;
    }
    report("ok", "acpi_call", "interface present");
    // Opening without truncate checks access without issuing an ACPI call.
    if let Err(e) = OpenOptions::new().read(true).write(true).open(&call) {
        report(
            "fail",
            "permissions",
            format!("{e}; try sudo hp-charge-limit-rs diagnose"),
        );
        report("skip", "firmware readback", "ACPI interface inaccessible");
        return false;
    }
    report("ok", "permissions", "ACPI interface readable and writable");
    match Wmi.read() {
        Ok(mode) => check(true, "firmware readback", mode),
        Err(e) => check(false, "firmware readback", format!("{e:#}")),
    }
}

#[cfg(windows)]
fn firmware() -> bool {
    match crate::firmware::interface() {
        Ok(instance) => report("ok", "HP BIOS WMI (root\\wmi hpqBIntM)", instance),
        Err(e) => {
            report("fail", "HP BIOS WMI (root\\wmi hpqBIntM)", format!("{e:#}"));
            report("skip", "firmware readback", "interface unavailable");
            return false;
        }
    }
    match Wmi.read() {
        Ok(mode) => check(true, "firmware readback (command 1, type 0x2B)", mode),
        Err(e) => check(
            false,
            "firmware readback (command 1, type 0x2B)",
            format!("{e:#}"),
        ),
    }
}

/// Reported only: the service is optional for manual commands.
fn service() {
    let state = service::service_state();
    let mark = if state == "active" { "ok" } else { "warn" };
    report(
        mark,
        "service",
        format!("{state} (optional for manual commands)"),
    );
    match service::service_config() {
        Some(config) => {
            println!("[info] service configuration (reported, not validated):");
            for line in config.lines() {
                println!("  {line}");
            }
        }
        None => report(
            "warn",
            "service configuration",
            "service not installed or query failed",
        ),
    }
}
