//! The charge band:
//!
//! ```text
//!   Charging --(capacity >= stop, run mode set)--> Limiting
//!   Limiting --(capacity <= start, auto set)-----> Charging
//! ```
//!
//! A transition only happens once the firmware accepted the new mode;
//! otherwise the state stays and the next tick retries. `finish` consumes
//! the limiter, so nothing can tick after it.

use crate::events::Event;
use crate::firmware::{ChargeControl, Mode};
use crate::power::Reading;
use clap::ValueEnum;
use std::fmt;

#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub stop: u8,
    pub start: u8,
    pub run_mode: Mode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    Charging,
    Limiting,
    Bypassed,
    /// Between `suspend` and `resume`: ticks leave the off-mode alone
    #[cfg_attr(not(windows), allow(dead_code))]
    Suspended {
        /// One tick may fall between suspend and the actual sleep; a second
        /// means the PC is awake and its resume event was missed.
        ticked: bool,
    },
}

pub struct Limiter {
    band: Band,
    state: State,
    events: Vec<Event>,
    /// The last firmware write failed; keeps a failing run to one event
    failing: bool,
}

impl Limiter {
    /// Sets `mode`; a failure queues one `WriteFailed` per run of failures.
    fn attempt(&mut self, fw: &mut impl ChargeControl, mode: Mode) -> Result<(), String> {
        match fw.set(mode) {
            Ok(()) => {
                self.failing = false;
                Ok(())
            }
            Err(e) => {
                let error = format!("{e:#}");
                if !self.failing {
                    self.events.push(Event::WriteFailed { mode, error: error.clone() });
                }
                self.failing = true;
                Err(error)
            }
        }
    }

    /// Sets `mode` and logs `<label>: cap=..% -> <mode>`; true if it took.
    fn write(&mut self, fw: &mut impl ChargeControl, mode: Mode, label: &str, capacity: u8) -> bool {
        match self.attempt(fw, mode) {
            Ok(()) => {
                println!("{label}: cap={capacity}% -> {mode}");
                true
            }
            Err(e) => {
                eprintln!("{label}: cap={capacity}% -> {mode} FAILED: {e}");
                false
            }
        }
    }

    /// Re-set every cycle while in its zone: harmless when already set, and
    /// repairs a mode changed behind our back.
    fn reassert(&mut self, fw: &mut impl ChargeControl, mode: Mode) {
        if let Err(e) = self.attempt(fw, mode) {
            eprintln!("set {mode}: {e}");
        }
    }

    /// Events since the last call, oldest first.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Always sets a mode, replacing whatever was left (e.g. hold from the
    /// last shutdown); above `start` it keeps limiting unless bypassed.
    pub fn start(band: Band, capacity: u8, bypass: bool, fw: &mut impl ChargeControl) -> Self {
        let mut limiter = Limiter { band, state: State::Starting, events: Vec::new(), failing: false };
        limiter.enter(capacity, bypass, fw);
        limiter
    }

    /// Sets the mode `capacity` and `bypass` call for, ignoring hysteresis:
    /// used at start (retried while it fails) and when bypass begins or
    /// ends. Only bypass changes after a successful start notify.
    fn enter(&mut self, capacity: u8, bypass: bool, fw: &mut impl ChargeControl) {
        let (mode, state) = if bypass {
            (Mode::Auto, State::Bypassed)
        } else if capacity <= self.band.start {
            (Mode::Auto, State::Charging)
        } else {
            (self.band.run_mode, State::Limiting)
        };
        let label = match self.state {
            _ if bypass => "bypass",
            State::Bypassed => "resume",
            _ => "start",
        };
        if self.write(fw, mode, label, capacity) {
            if self.state != State::Starting {
                self.events.push(Event::BypassChanged { enabled: bypass });
            }
            self.state = state;
        }
    }

    pub fn tick(&mut self, reading: Reading, bypass: bool, fw: &mut impl ChargeControl) {
        match self.state {
            // a tick between suspend and the actual sleep must not replace
            // the off-mode
            State::Suspended { ticked: false } => {
                self.state = State::Suspended { ticked: true };
                return;
            }
            State::Suspended { ticked: true } => {
                println!("awake without a resume event since suspend: limiting again");
                self.resume();
            }
            _ => {}
        }
        // on battery nothing charges: leave the mode alone
        if !reading.on_ac {
            return;
        }
        let capacity = reading.capacity;
        let bypassed = self.state == State::Bypassed;
        if bypass && bypassed {
            self.reassert(fw, Mode::Auto);
            return;
        }
        if bypass || bypassed || self.state == State::Starting {
            self.enter(capacity, bypass, fw);
            return;
        }
        let Band { stop, start, run_mode } = self.band;
        match self.state {
            State::Charging if capacity >= stop => {
                if self.write(fw, run_mode, "limit", capacity) {
                    self.state = State::Limiting;
                    self.events.push(Event::LimitReached { capacity });
                }
            }
            State::Charging if capacity <= start => self.reassert(fw, Mode::Auto),
            State::Limiting if capacity <= start => {
                if self.write(fw, Mode::Auto, "release", capacity) {
                    self.state = State::Charging;
                    self.events.push(Event::ChargingResumed { capacity });
                }
            }
            State::Limiting if capacity >= stop => self.reassert(fw, run_mode),
            State::Starting | State::Charging | State::Limiting | State::Bypassed | State::Suspended { .. } => {}
        }
    }

    /// Sets the mode `policy` picks for `exit`; returns that mode, or None
    /// if the mode was kept (by policy, or because the write failed).
    fn leave(&mut self, exit: Exit, policy: &ExitPolicy, capacity: u8, fw: &mut impl ChargeControl) -> Option<Mode> {
        let label = exit.label();
        match policy.mode(exit, capacity) {
            Some(mode) => self.write(fw, mode, label, capacity).then_some(mode),
            None => {
                println!("{label}: cap={capacity}% -> mode kept");
                None
            }
        }
    }

    /// Ends the limiter with the mode `policy` picks for `exit`; returns
    /// that mode, or None if the mode was kept.
    pub fn finish(mut self, exit: Exit, policy: &ExitPolicy, capacity: u8, fw: &mut impl ChargeControl) -> Option<Mode> {
        self.leave(exit, policy, capacity, fw)
    }

    /// The PC is going to sleep or hibernate: sets the shutdown mode, since
    /// hibernation (also Windows Fast Startup's "shut down") powers off like
    /// a shutdown. Ticks change nothing until `resume`, or until a second
    /// tick shows the resume event was missed.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn suspend(&mut self, policy: &ExitPolicy, capacity: u8, fw: &mut impl ChargeControl) -> Option<Mode> {
        let left = self.leave(Exit::Suspend, policy, capacity, fw);
        self.state = State::Suspended { ticked: false };
        left
    }

    /// After `suspend`: the next AC tick sets the mode afresh, as at start.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn resume(&mut self) {
        self.state = State::Starting;
    }
}

/// What to do with the firmware mode when the limiter ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StopAction {
    Auto,
    Inhibit,
    Hold,
    /// Leave whatever mode is set
    Keep,
}

impl StopAction {
    pub fn mode(self) -> Option<Mode> {
        match self {
            StopAction::Auto => Some(Mode::Auto),
            StopAction::Inhibit => Some(Mode::Inhibit),
            StopAction::Hold => Some(Mode::Hold),
            StopAction::Keep => None,
        }
    }
}

impl fmt::Display for StopAction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let value = self.to_possible_value().expect("no skipped variants");
        f.write_str(value.get_name())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The PC is shutting down or rebooting
    Shutdown,
    /// Only the service is stopped
    ServiceStop,
    /// Sleep or hibernate; the limiter continues afterwards
    #[cfg_attr(not(windows), allow(dead_code))]
    Suspend,
}

impl Exit {
    fn label(self) -> &'static str {
        match self {
            Exit::Shutdown => "shutdown",
            Exit::ServiceStop => "service stop",
            Exit::Suspend => "suspend",
        }
    }
}

pub struct ExitPolicy {
    pub off_mode: StopAction,
    /// At or below this %, shutdown uses auto so the battery charges while off
    pub off_min: u8,
    pub stop_mode: StopAction,
}

impl ExitPolicy {
    fn mode(&self, exit: Exit, capacity: u8) -> Option<Mode> {
        match exit {
            Exit::ServiceStop => self.stop_mode.mode(),
            Exit::Shutdown | Exit::Suspend => self
                .off_mode
                .mode()
                .map(|mode| if capacity <= self.off_min { Mode::Auto } else { mode }),
        }
    }
}
