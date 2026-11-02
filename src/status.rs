//! `hp-charge-limit-rs status`: one look at battery, AC, firmware and service.

use crate::firmware::Wmi;
use crate::power::{self, Energy};
use crate::{bypass, offstate, service};
use anyhow::{Context, Result};
use std::thread::sleep;
use std::time::{Duration, Instant};

const WATCH_SAMPLE: Duration = Duration::from_secs(5);
/// Reported energy moves in 10 mWh steps (sysfs energy_now; Windows
/// RemainingCapacity is often coarser); smaller changes are noise.
const ENERGY_RESOLUTION: Energy = Energy::from_mwh(10);

pub fn show(watch_secs: Option<u64>) -> Result<()> {
    let unknown = || "?".to_string();
    println!("AC:       {}", if power::on_ac() { "online" } else { "offline" });
    println!(
        "battery:  {}%  {}  {}  {}",
        power::capacity().map_or_else(unknown, |c| c.to_string()),
        power::status().unwrap_or_else(unknown),
        power::energy().map_or_else(unknown, |e| e.to_string()),
        power::voltage().map_or_else(unknown, |v| v.to_string()),
    );
    println!(
        "pack:     {}  {}",
        power::manufacturer().unwrap_or_else(unknown),
        power::model().unwrap_or_else(unknown),
    );
    println!(
        "health:   {}  cycles: {}  power: {}",
        power::health().map_or_else(unknown, |h| format!("{h:.1}%")),
        power::cycles().map_or_else(unknown, |c| c.to_string()),
        power::power_watts().map_or_else(unknown, |p| format!("{p:.2} W")),
    );
    println!("bypass:   {}", if bypass::enabled() { "enabled (requested)" } else { "disabled" });
    match Wmi.read() {
        Ok(mode) => {
            println!("firmware: {mode}");
            if mode.is_hold() {
                println!("          (in hold, LED and status show charging although no charge flows)");
            }
        }
        Err(e) => println!("firmware: unreadable: {e:#}"),
    }
    println!("service:  {} {}", service::SERVICE, service::service_state());
    if let Some(line) = offstate::last() {
        println!("last off: {line}");
    }
    if let Some(secs) = watch_secs {
        watch(Duration::from_secs(secs))?;
    }
    Ok(())
}

/// Samples remaining energy: the only reliable sign of charge flow, since
/// LED and status say "Charging" under hold.
fn watch(span: Duration) -> Result<()> {
    let start = power::energy().context("battery energy unreadable")?;
    let t0 = Instant::now();
    println!("watching energy for {} s ...", span.as_secs());
    while t0.elapsed() < span {
        sleep(WATCH_SAMPLE);
        let e = power::energy().unwrap_or(start);
        println!("  +{:>3} s  {e}  ({:+} mWh)", t0.elapsed().as_secs(), (e - start).mwh());
    }
    let delta = power::energy().unwrap_or(start) - start;
    let verdict = if delta.abs() <= ENERGY_RESOLUTION {
        "no charge flow"
    } else if delta > Energy::from_uwh(0) {
        "CHARGING"
    } else {
        "DISCHARGING"
    };
    println!("result: {verdict} ({:+} mWh in {} s)", delta.mwh(), span.as_secs());
    Ok(())
}
