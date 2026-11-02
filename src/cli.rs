//! Command line. Daemon options also read environment variables, which is
//! how the Linux service reads /etc/hp-charge-limit-rs.conf; the Windows
//! service gets them as arguments (`service install`).

use crate::daemon::StopAction;
use crate::firmware::Mode;
use anyhow::{Result, ensure};
#[cfg(target_os = "linux")]
use clap::ArgAction;
use clap::{Args, Parser, Subcommand, ValueEnum};

const MAX_PERCENT: i64 = 100;
const DEFAULT_STOP: u8 = 50;
const DEFAULT_START: u8 = 40;
const DEFAULT_INTERVAL_SECS: u64 = 30;
const MIN_INTERVAL_SECS: u64 = 5;
const DEFAULT_OFF_MIN: u8 = 40;

#[derive(Parser)]
#[command(version, about = "Battery charge limiter using the HP WMI charge-control method SBCC")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Show battery, AC and firmware charge mode
    Status {
        /// Also sample energy for this many seconds to see if charge really flows
        #[arg(long, value_name = "SECS")]
        watch: Option<u64>,
    },
    /// Diagnose supplies, firmware access and readback, and service setup
    Diagnose,
    /// Show desktop notifications for daemon D-Bus signals (run as your user, no root)
    #[cfg(target_os = "linux")]
    Notify,
    /// Set a firmware charge mode once
    Set { mode: Mode },
    /// Pause or resume charge limiting until reboot (applied on the next daemon poll)
    Bypass { action: BypassAction },
    /// Run the limiter loop (what the service runs)
    Daemon(DaemonOpts),
    /// Install or remove the Windows service (run as administrator)
    #[cfg(windows)]
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Generate a shell completion script (no root required)
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum BypassAction {
    On,
    Off,
}

#[cfg(windows)]
#[derive(Subcommand)]
pub enum ServiceAction {
    /// Install (or reconfigure) the service with these daemon options, start at boot, and start it now
    Install(DaemonOpts),
    /// Stop the service (applying its stop mode) and remove it
    Uninstall,
}

#[derive(Args, Clone)]
pub struct DaemonOpts {
    /// Stop charging at or above this %
    #[arg(long, env = "HP_CHARGE_LIMIT_RS_STOP", default_value_t = DEFAULT_STOP,
          value_parser = clap::value_parser!(u8).range(1..=MAX_PERCENT))]
    pub stop: u8,
    /// Resume charging at or below this %
    #[arg(long, env = "HP_CHARGE_LIMIT_RS_START", default_value_t = DEFAULT_START,
          value_parser = clap::value_parser!(u8).range(0..MAX_PERCENT))]
    pub start: u8,
    /// Poll interval in seconds
    #[arg(long, env = "HP_CHARGE_LIMIT_RS_INTERVAL", default_value_t = DEFAULT_INTERVAL_SECS,
          value_parser = clap::value_parser!(u64).range(MIN_INTERVAL_SECS..))]
    pub interval: u64,
    /// Mode used while running once --stop is reached
    #[arg(long, env = "HP_CHARGE_LIMIT_RS_RUN_MODE", value_enum, default_value_t = Mode::Inhibit)]
    pub run_mode: Mode,
    /// Mode set when the PC shuts down or reboots
    #[arg(long, env = "HP_CHARGE_LIMIT_RS_OFF_MODE", value_enum, default_value_t = StopAction::Hold)]
    pub off_mode: StopAction,
    /// At shutdown, use auto instead of --off-mode at or below this % (charge while off)
    #[arg(long, env = "HP_CHARGE_LIMIT_RS_OFF_MIN", default_value_t = DEFAULT_OFF_MIN,
          value_parser = clap::value_parser!(u8).range(0..=MAX_PERCENT))]
    pub off_min: u8,
    /// Mode set when only the service is stopped, not the PC
    #[arg(long, env = "HP_CHARGE_LIMIT_RS_STOP_MODE", value_enum, default_value_t = StopAction::Auto)]
    pub stop_mode: StopAction,
    /// Broadcast D-Bus signals for desktop notifications; false never touches the bus
    #[cfg(target_os = "linux")]
    #[arg(long, env = "HP_CHARGE_LIMIT_RS_NOTIFY", default_value_t = true, action = ArgAction::Set)]
    pub notify: bool,
}

impl DaemonOpts {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.start < self.stop, "--start ({}) must be below --stop ({})", self.start, self.stop);
        Ok(())
    }

    /// The same options as `daemon` arguments, for the service command line.
    #[cfg(windows)]
    pub fn to_args(&self) -> Vec<String> {
        vec![
            format!("--stop={}", self.stop),
            format!("--start={}", self.start),
            format!("--interval={}", self.interval),
            format!("--run-mode={}", self.run_mode),
            format!("--off-mode={}", self.off_mode),
            format!("--off-min={}", self.off_min),
            format!("--stop-mode={}", self.stop_mode),
        ]
    }
}
