//! `hp-charge-limit-rs daemon`: the limiter loop the service runs.
//!
//! The loop is shared; the platform only feeds it `Control` messages
//! (Linux: signals, with systemd telling shutdown from a service stop).

mod limiter;
mod linux;

pub use limiter::StopAction;

use self::linux as platform;

use crate::cli::DaemonOpts;
use crate::events::Emitter;
use crate::firmware::Wmi;
use crate::{bypass, offstate, power};
use anyhow::{Result, ensure};
use limiter::{Band, Exit, ExitPolicy, Limiter};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

/// Used when capacity can't be read at start or exit: counts as empty, so
/// the battery charges rather than holds.
const UNKNOWN_CAPACITY: u8 = 0;

/// What the platform asks of the running loop.
pub enum Control {
    /// Leave the loop and set the exit mode
    Exit(Exit),
}

/// Sends the limiter's new events, or drops them with notifications off;
/// taken either way so they never pile up.
fn publish(limiter: &mut Limiter, emitter: Option<&mut Emitter>) {
    let events = limiter.take_events();
    if let Some(emitter) = emitter {
        emitter.emit_all(events);
    }
}

fn exit_policy(o: &DaemonOpts) -> ExitPolicy {
    if bypass::enabled() {
        ExitPolicy { off_mode: StopAction::Auto, off_min: o.off_min, stop_mode: StopAction::Auto }
    } else {
        ExitPolicy { off_mode: o.off_mode, off_min: o.off_min, stop_mode: o.stop_mode }
    }
}

fn capacity() -> u8 {
    power::capacity().unwrap_or(UNKNOWN_CAPACITY)
}

/// `hold`, `auto`, ... or `keep` for what an exit left set.
fn action(left: Option<crate::firmware::Mode>) -> String {
    left.map_or(StopAction::Keep.to_string(), |mode| mode.to_string())
}

pub fn run(o: &DaemonOpts) -> Result<()> {
    ensure!(o.start < o.stop, "--start ({}) must be below --stop ({})", o.start, o.stop);
    platform::run(o)
}

/// The limiter loop until `Control::Exit` (or the sender is gone).
fn run_loop(o: &DaemonOpts, controls: Receiver<Control>) -> Result<()> {
    println!(
        "hp-charge-limit-rs: stop={}% start={}% poll={}s run={} off={} (min {}%) stop-mode={} notify={}",
        o.stop, o.start, o.interval, o.run_mode, o.off_mode, o.off_min, o.stop_mode, o.notify
    );
    if let Some(line) = offstate::report() {
        println!("{line}");
    }

    let band = Band { stop: o.stop, start: o.start, run_mode: o.run_mode };
    let interval = Duration::from_secs(o.interval);
    let mut wmi = Wmi;
    let mut emitter = o.notify.then(Emitter::new);
    let mut limiter = Limiter::start(band, capacity(), bypass::enabled(), &mut wmi);
    publish(&mut limiter, emitter.as_mut());
    // wait one interval for a control message; a timeout is the next tick
    let exit = loop {
        match controls.recv_timeout(interval) {
            Err(RecvTimeoutError::Timeout) => {
                if let Some(reading) = power::reading() {
                    limiter.tick(reading, bypass::enabled(), &mut wmi);
                    publish(&mut limiter, emitter.as_mut());
                }
            }
            Ok(Control::Exit(exit)) => break exit,
            Err(RecvTimeoutError::Disconnected) => break Exit::ServiceStop,
        }
    };

    let capacity = capacity();
    let left = limiter.finish(exit, &exit_policy(o), capacity, &mut wmi);
    if exit == Exit::Shutdown {
        offstate::save(capacity, &action(left));
    }
    Ok(())
}
