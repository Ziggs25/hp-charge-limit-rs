//! `hp-charge-limit-rs daemon`: the limiter loop the service runs.
//!
//! The loop is shared; each platform only feeds it `Control` messages:
//! signals and systemd state on Linux, Service Control Manager requests
//! (stop, pre-shutdown, sleep/resume) or Ctrl+C on Windows.

mod limiter;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

pub use limiter::StopAction;

#[cfg(target_os = "linux")]
use self::linux as platform;
#[cfg(windows)]
use self::windows as platform;

use crate::cli::DaemonOpts;
use crate::events::Event;
#[cfg(target_os = "linux")]
use crate::events::Emitter;
use crate::firmware::Wmi;
use crate::{bypass, offstate, power};
use anyhow::Result;
use limiter::{Band, Exit, ExitPolicy, Limiter};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

/// Used when capacity can't be read at start or exit: counts as empty, so
/// the battery charges rather than holds.
const UNKNOWN_CAPACITY: u8 = 0;

/// What the platform asks of the running loop.
pub enum Control {
    /// Leave the loop and set the exit mode
    Exit(Exit),
    /// Set the shutdown mode before sleep/hibernate, then answer on the sender
    #[cfg_attr(not(windows), allow(dead_code))]
    Suspend(Sender<()>),
    /// Back from sleep/hibernate: report the off period, limit afresh
    #[cfg_attr(not(windows), allow(dead_code))]
    Resume,
}

/// Where limiter events go: D-Bus signals on Linux when enabled.
struct Publisher {
    #[cfg(target_os = "linux")]
    emitter: Option<Emitter>,
}

impl Publisher {
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    fn new(o: &DaemonOpts) -> Self {
        Publisher {
            #[cfg(target_os = "linux")]
            emitter: o.notify.then(Emitter::new),
        }
    }

    /// Sends the limiter's new events, or drops them with notifications
    /// off; taken either way so they never pile up.
    fn publish(&mut self, limiter: &mut Limiter) {
        let events: Vec<Event> = limiter.take_events();
        #[cfg(target_os = "linux")]
        if let Some(emitter) = self.emitter.as_mut() {
            emitter.emit_all(events);
        }
        #[cfg(not(target_os = "linux"))]
        drop(events);
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
    o.validate()?;
    platform::run(o)
}

/// The limiter loop until `Control::Exit` (or the sender is gone).
fn run_loop(o: &DaemonOpts, controls: Receiver<Control>) -> Result<()> {
    #[cfg(target_os = "linux")]
    let notify = format!(" notify={}", o.notify);
    #[cfg(not(target_os = "linux"))]
    let notify = String::new();
    println!(
        "hp-charge-limit-rs: stop={}% start={}% poll={}s run={} off={} (min {}%) stop-mode={}{notify}",
        o.stop, o.start, o.interval, o.run_mode, o.off_mode, o.off_min, o.stop_mode
    );
    if let Some(line) = offstate::report() {
        println!("{line}");
    }

    let band = Band { stop: o.stop, start: o.start, run_mode: o.run_mode };
    let interval = Duration::from_secs(o.interval);
    let mut wmi = Wmi;
    let mut publisher = Publisher::new(o);
    let mut limiter = Limiter::start(band, capacity(), bypass::enabled(), &mut wmi);
    publisher.publish(&mut limiter);
    let tick = |limiter: &mut Limiter, publisher: &mut Publisher, wmi: &mut Wmi| {
        if let Some(reading) = power::reading() {
            limiter.tick(reading, bypass::enabled(), wmi);
            publisher.publish(limiter);
        }
    };
    // wait one interval for a control message; a timeout is the next tick
    let exit = loop {
        match controls.recv_timeout(interval) {
            Err(RecvTimeoutError::Timeout) => tick(&mut limiter, &mut publisher, &mut wmi),
            Ok(Control::Suspend(done)) => {
                let capacity = capacity();
                let left = limiter.suspend(&exit_policy(o), capacity, &mut wmi);
                offstate::save(capacity, &action(left));
                let _ = done.send(());
            }
            Ok(Control::Resume) => {
                if let Some(line) = offstate::report() {
                    println!("{line}");
                }
                limiter.resume();
                tick(&mut limiter, &mut publisher, &mut wmi);
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
