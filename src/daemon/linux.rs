//! Linux: SIGTERM (systemd stop, shutdown) or SIGINT ends the loop; systemd
//! says which of the two it was.

use super::Control;
use super::limiter::Exit;
use crate::cli::DaemonOpts;
use crate::service;
use anyhow::Result;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::sync::mpsc::{self, Receiver};
use std::thread;

pub fn run(o: &DaemonOpts) -> Result<()> {
    let controls = termination()?;
    super::run_loop(o, controls)
}

/// A channel that receives an exit on SIGTERM or SIGINT. Registered before
/// it returns, so a signal during startup is kept.
fn termination() -> Result<Receiver<Control>> {
    let (tx, rx) = mpsc::channel();
    let mut signals = Signals::new([SIGTERM, SIGINT])?;
    thread::spawn(move || {
        for _ in signals.forever() {
            let exit = if service::system_stopping() {
                Exit::Shutdown
            } else {
                Exit::ServiceStop
            };
            if tx.send(Control::Exit(exit)).is_err() {
                break;
            }
        }
    });
    Ok(rx)
}
