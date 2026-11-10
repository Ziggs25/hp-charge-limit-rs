//! Battery charge limiter using the firmware's own
//! ACPI WMI battery-charge-control method `\_SB.WMID.SBCC` through
//! `acpi_call` on Linux.
//!
//! Firmware modes (see `firmware`):
//!   auto     normal charging
//!   inhibit  SBIS: no charging, white LED; cleared by the EC at shutdown
//!   hold     SBTC: no charging, survives shutdown (ec-re/SBTC_HOLD.md);
//!            board 88ED reports "Not charging", other boards may say "Charging"
//! Only these three are ever written: never discharge or battery ship mode.

pub mod cli;
mod bypass;
mod daemon;
mod diagnose;
mod events;
mod firmware;
mod notify;
mod offstate;
mod paths;
mod power;
mod service;
mod status;

#[cfg(not(target_os = "linux"))]
compile_error!("hp-charge-limit-rs supports Linux only");

use anyhow::Result;
use cli::{Cli, Cmd};
use clap::CommandFactory;
use firmware::{ChargeControl, Wmi};

pub fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Status { watch } => status::show(watch),
        Cmd::Diagnose => diagnose::run(),
        Cmd::Notify => notify::run(),
        Cmd::Set { mode } => {
            let mut wmi = Wmi;
            wmi.set(mode)?;
            println!("set {mode} -> firmware reports {}", wmi.read()?);
            Ok(())
        }
        Cmd::Bypass { action } => bypass::set(action),
        Cmd::Daemon(opts) => daemon::run(&opts),
        Cmd::Completions { shell } => {
            let mut command = Cli::command();
            clap_complete::generate(shell, &mut command, "hp-charge-limit-rs", &mut std::io::stdout());
            Ok(())
        }
    }
}
