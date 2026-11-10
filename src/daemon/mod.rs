//! `hp-charge-limit-rs daemon`: the limiter loop the service runs.
//!
//! Linux feeds the loop `Control` messages from signals and systemd state.

mod limiter;
mod linux;

pub use limiter::StopAction;

use self::linux as platform;

use crate::cli::DaemonOpts;
use crate::events::{Emitter, Event};
use crate::firmware::Wmi;
use crate::{bypass, offstate, power};
use anyhow::Result;
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

/// Where limiter events go: D-Bus signals when enabled.
struct Publisher {
    emitter: Option<Emitter>,
}

impl Publisher {
    fn new(o: &DaemonOpts) -> Self {
        Publisher { emitter: o.notify.then(Emitter::new) }
    }

    /// Sends the limiter's new events, or drops them with notifications
    /// off; taken either way so they never pile up.
    fn publish(&mut self, limiter: &mut Limiter) {
        let events: Vec<Event> = limiter.take_events();
        if let Some(emitter) = self.emitter.as_mut() {
            emitter.emit_all(events);
        }
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
