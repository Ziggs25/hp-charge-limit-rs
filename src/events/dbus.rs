//! D-Bus signals the daemon emits on the system bus, for desktop notifiers.
//!
//! Signals only: no methods, so nothing on the bus can change charging.
//! Emitting is strictly best effort. A missing or broken bus never affects
//! charge control, and only root may own `NAME` (see the bus policy file).

use super::Event;
use crate::firmware::Mode;
use clap::ValueEnum;
use std::time::{Duration, Instant};
use zbus::{Connection, Message, block_on};

pub const NAME: &str = "io.github.kralonur.HpChargeLimit";
pub const PATH: &str = "/io/github/kralonur/HpChargeLimit";
pub const INTERFACE: &str = "io.github.kralonur.HpChargeLimit";

/// Wait this long before trying to reach the bus again after a failure.
const RETRY_AFTER: Duration = Duration::from_secs(60);

impl Event {
    fn signal(&self) -> &'static str {
        match self {
            Event::LimitReached { .. } => "LimitReached",
            Event::ChargingResumed { .. } => "ChargingResumed",
            Event::BypassChanged { .. } => "BypassChanged",
            Event::WriteFailed { .. } => "WriteFailed",
        }
    }

    /// The event a received signal carries; None for unknown members or
    /// malformed bodies. Does not check the sender.
    pub fn parse(msg: &Message) -> Option<Event> {
        let header = msg.header();
        let body = msg.body();
        let event = match header.member()?.as_str() {
            "LimitReached" => Event::LimitReached {
                capacity: body.deserialize::<(u8,)>().ok()?.0,
            },
            "ChargingResumed" => Event::ChargingResumed {
                capacity: body.deserialize::<(u8,)>().ok()?.0,
            },
            "BypassChanged" => Event::BypassChanged {
                enabled: body.deserialize::<(bool,)>().ok()?.0,
            },
            "WriteFailed" => {
                let (mode, error): (String, String) = body.deserialize().ok()?;
                Event::WriteFailed {
                    mode: Mode::from_str(&mode, false).ok()?,
                    error,
                }
            }
            _ => return None,
        };
        Some(event)
    }

    /// The broadcast signal carrying this event; `parse` reverses it.
    fn message(&self) -> zbus::Result<Message> {
        let signal = Message::signal(PATH, INTERFACE, self.signal());
        match self {
            Event::LimitReached { capacity } | Event::ChargingResumed { capacity } => {
                signal.build(&(*capacity,))
            }
            Event::BypassChanged { enabled } => signal.build(&(*enabled,)),
            Event::WriteFailed { mode, error } => signal.build(&(mode.to_string(), error.as_str())),
        }
    }
}

pub struct Emitter {
    bus: Option<Connection>,
    next_try: Instant,
    reported_down: bool,
}

impl Emitter {
    pub fn new() -> Self {
        let mut emitter = Emitter {
            bus: None,
            next_try: Instant::now(),
            reported_down: false,
        };
        emitter.connect();
        emitter
    }

    fn connect(&mut self) {
        let attempt: zbus::Result<Connection> = block_on(async {
            let bus = Connection::system().await?;
            bus.request_name(NAME).await?;
            Ok(bus)
        });
        match attempt {
            Ok(bus) => {
                self.bus = Some(bus);
                if self.reported_down {
                    println!("D-Bus: connected, signals enabled");
                }
                self.reported_down = false;
            }
            Err(e) => {
                if !self.reported_down {
                    eprintln!("D-Bus: signals disabled, will retry: {e}");
                    self.reported_down = true;
                }
                self.next_try = Instant::now() + RETRY_AFTER;
            }
        }
    }

    pub fn emit_all(&mut self, events: impl IntoIterator<Item = Event>) {
        for event in events {
            self.emit(&event);
        }
    }

    fn emit(&mut self, event: &Event) {
        if self.bus.is_none() && Instant::now() >= self.next_try {
            self.connect();
        }
        let Some(bus) = &self.bus else { return };
        if let Err(e) = event.message().and_then(|msg| block_on(bus.send(&msg))) {
            eprintln!("D-Bus: {} not sent: {e}", event.signal());
            self.bus = None;
            self.next_try = Instant::now() + RETRY_AFTER;
        }
    }
}
