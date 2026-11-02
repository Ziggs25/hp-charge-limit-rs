//! Energy before shutdown is saved, so the next start can report what
//! happened while the PC was off.

use crate::paths::{self, OFF_STATE};
use crate::power::{self, Energy};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SECS_PER_MIN: u64 = 60;
/// The last report is kept here for `status`.
const LAST_SUFFIX: &str = ".last";

/// One line: `<unix secs> <energy µWh> <capacity %> <action>`.
struct Record {
    at: SystemTime,
    energy: Energy,
    capacity: u8,
    action: String,
}

impl Record {
    fn to_line(&self) -> String {
        let secs = self.at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        format!("{secs} {} {} {}\n", self.energy.uwh(), self.capacity, self.action)
    }

    fn parse(line: &str) -> Option<Self> {
        let mut f = line.split_whitespace();
        Some(Record {
            at: UNIX_EPOCH + Duration::from_secs(f.next()?.parse().ok()?),
            energy: Energy::from_uwh(f.next()?.parse().ok()?),
            capacity: f.next()?.parse().ok()?,
            action: f.next()?.to_string(),
        })
    }
}

fn last_path() -> PathBuf {
    let mut path = paths::at(OFF_STATE).into_os_string();
    path.push(LAST_SUFFIX);
    path.into()
}

pub fn save(capacity: u8, action: &str) {
    let Some(energy) = power::energy() else { return };
    let record = Record { at: SystemTime::now(), energy, capacity, action: action.into() };
    let path = paths::at(OFF_STATE);
    if let Err(e) = paths::ensure_dir_for(&path) {
        eprintln!("off-state not saved: {e:#}");
        return;
    }
    let _ = fs::write(path, record.to_line());
}

/// Consumes the saved record and describes the off period, e.g.
/// `off 12 min in hold: 51% -> 51%, energy +0 mWh`.
pub fn report() -> Option<String> {
    let path = paths::at(OFF_STATE);
    let record = Record::parse(&fs::read_to_string(&path).ok()?);
    let _ = fs::remove_file(&path);
    let record = record?;
    let energy = power::energy()?;
    let mins = record.at.elapsed().unwrap_or_default().as_secs() / SECS_PER_MIN;
    let line = format!(
        "off {mins} min in {}: {}% -> {}%, energy {:+} mWh",
        record.action,
        record.capacity,
        power::capacity().map_or("?".into(), |c| c.to_string()),
        (energy - record.energy).mwh()
    );
    let _ = fs::write(last_path(), format!("{line}\n"));
    Some(line)
}

/// The last `report` line, if any.
pub fn last() -> Option<String> {
    paths::read_trimmed(&last_path())
}
