//! Setup diagnostics: queries only, never SBCC writes or service changes.

use crate::firmware::Wmi;
use crate::{paths, power, service};
use anyhow::{Result, ensure};
use std::fmt::Display;
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

fn machine() -> Machine {
    let dmi = |name: &str| paths::read_trimmed(&paths::at(&format!("/sys/class/dmi/id/{name}")));
    Machine { vendor: dmi("sys_vendor"), model: dmi("product_name"), board: dmi("board_name"), bios: dmi("bios_version") }
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
    println!("No charge modes changed. Readback does not prove firmware compatibility or charge blocking.");
    ensure!(ok, "diagnostics found problems; see checks above");
    Ok(())
}

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
    ok &= check(!adapters.is_empty(), "external power supplies", adapters.len());
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

fn firmware() -> bool {
    let call = paths::at(paths::ACPI_CALL);
    if !call.exists() {
        report("fail", "acpi_call", "interface missing; install/load acpi_call for the running kernel");
        report("skip", "permissions and firmware readback", "ACPI interface missing");
        return false;
    }
    report("ok", "acpi_call", "interface present");
    // Opening without truncate checks access without issuing an ACPI call.
    if let Err(e) = OpenOptions::new().read(true).write(true).open(&call) {
        report("fail", "permissions", format!("{e}; try sudo hp-charge-limit-rs diagnose"));
        report("skip", "firmware readback", "ACPI interface inaccessible");
        return false;
    }
    report("ok", "permissions", "ACPI interface readable and writable");
    match Wmi.read() {
        Ok(mode) => check(true, "firmware readback", mode),
        Err(e) => check(false, "firmware readback", format!("{e:#}")),
    }
}

/// Reported only: the service is optional for manual commands.
fn service() {
    let state = service::service_state();
    let mark = if state == "active" { "ok" } else { "warn" };
    report(mark, "service", format!("{state} (optional for manual commands)"));
    match service::service_config() {
        Some(config) => {
            println!("[info] service configuration (reported, not validated):");
            for line in config.lines() {
                println!("  {line}");
            }
        }
        None => report("warn", "service configuration", "systemctl unavailable or query failed"),
    }
}
