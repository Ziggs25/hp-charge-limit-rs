//! Battery charge limiter using the firmware's own
//! ACPI WMI battery-charge-control method `\_SB.WMID.SBCC`: via acpi_call
//! on Linux, via the HP BIOS WMI class `hpqBIntM` on Windows.
//!
//! Firmware modes (see `firmware`):
//!   auto     normal charging
//!   inhibit  SBIS: no charging, white LED; cleared by the EC at shutdown
//!   hold     SBTC: no charging, survives shutdown (ec-re/SBTC_HOLD.md),
//!            but LED and sysfs status say "Charging"
//! Only these three are ever written: never discharge or battery ship mode.

#[cfg(windows)]
mod acl;
mod bypass;
pub mod cli;
mod daemon;
mod diagnose;
mod events;
mod firmware;
#[cfg(target_os = "linux")]
mod notify;
mod offstate;
mod paths;
mod power;
mod service;
mod status;
#[cfg(windows)]
mod wmi;

#[cfg(not(any(target_os = "linux", windows)))]
compile_error!("hp-charge-limit-rs supports Linux and Windows only");

use anyhow::Result;
use clap::CommandFactory;
use cli::{Cli, Cmd};
use firmware::{ChargeControl, Wmi};

pub fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Status { watch } => status::show(watch),
        Cmd::Diagnose => diagnose::run(),
        #[cfg(target_os = "linux")]
        Cmd::Notify => zbus::block_on(notify::run()),
        Cmd::Set { mode } => {
            let mut wmi = Wmi;
            wmi.set(mode)?;
            println!("set {mode} -> firmware reports {}", wmi.read()?);
            Ok(())
        }
        Cmd::Bypass { action } => bypass::set(action),
        Cmd::Daemon(opts) => daemon::run(&opts),
        #[cfg(windows)]
        Cmd::Service { action } => match action {
            cli::ServiceAction::Install(opts) => service::install(&opts),
            cli::ServiceAction::Uninstall => service::uninstall(),
        },
        Cmd::Completions { shell } => {
            let mut command = Cli::command();
            clap_complete::generate(
                shell,
                &mut command,
                "hp-charge-limit-rs",
                &mut std::io::stdout(),
            );
            Ok(())
        }
    }
}
