//! Linux backend: calls `\_SB.WMID.SBCC`/`GBCC` directly through the
//! acpi_call module, or `SBCO`/`GBCO` when `GBCC` is absent.

use super::{Mode, Reported, mode_data};
use crate::paths::{self, ACPI_CALL, LOCK};
use anyhow::{Context, Result, anyhow, ensure};
use std::fs::{self, File};
use std::io::Read;

const SBCC: &str = r"\_SB.WMID.SBCC";
const GBCC: &str = r"\_SB.WMID.GBCC";
const SBCO: &str = r"\_SB.WMID.SBCO";
const GBCO: &str = r"\_SB.WMID.GBCO";

#[derive(Clone, Copy)]
enum Interface {
    Sbcc,
    Sbco,
}

impl Interface {
    fn command(self, mode: Mode) -> String {
        match self {
            Self::Sbcc => sbcc_command(mode),
            // Unlike SBCC, SBCO takes the mode integer, not a WMI buffer.
            Self::Sbco => format!("{SBCO} {:#x}", mode as u32),
        }
    }
}

/// SBCC takes the WMI request buffer WHCM would pass it: a 16-byte header
/// (signature, command, type, size), then the mode dword.
const SBCC_BUFFER_LEN: usize = 20;
const SBCC_MODE_OFFSET: usize = 0x10;
/// Larger than any reply of the methods we call.
const ACPI_REPLY_MAX: usize = 4096;
/// acpi_call reports failures as a reply text starting with this.
const ACPI_ERROR_PREFIX: &str = "Error";

/// The method call that sets `mode`.
fn sbcc_command(mode: Mode) -> String {
    let mut buf = [0u8; SBCC_BUFFER_LEN];
    let word = mode_data(mode);
    buf[SBCC_MODE_OFFSET..SBCC_MODE_OFFSET + word.len()].copy_from_slice(&word);
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!("{SBCC} b{hex}")
}

/// GBCC replies e.g. `[0x0, 0x4, {0x03, 0xff, 0x01, 0x00}]`.
fn parse_gbcc(reply: &str) -> Result<Reported> {
    let bad = || anyhow!("unexpected GBCC reply {reply}");
    let first = reply.split('{').nth(1).and_then(|s| s.split(',').next()).ok_or_else(bad)?;
    let byte = u8::from_str_radix(first.trim().trim_start_matches("0x"), 16).map_err(|_| bad())?;
    Ok(Reported(byte))
}

/// One ACPI method call through acpi_call; returns its reply text.
fn acpi_call(cmd: &str) -> Result<String> {
    let call = paths::at(ACPI_CALL);
    // acpi_call has one reply buffer for everyone: keep other instances
    // (daemon vs status) out between our write and read
    let _lock = File::create(paths::at(LOCK)).and_then(|f| f.lock().map(|()| f));
    fs::write(&call, cmd)
        .with_context(|| format!("{} (root and the acpi_call module needed)", call.display()))?;
    // acpi_call resets the reply after every read(), so take it in one read;
    // read_to_string's small first probe read would truncate and lose it
    let mut buf = vec![0u8; ACPI_REPLY_MAX];
    let n = File::open(&call)
        .and_then(|mut f| f.read(&mut buf))
        .with_context(|| call.display().to_string())?;
    let reply = String::from_utf8_lossy(&buf[..n]);
    Ok(reply.trim_matches(char::from(0)).trim().to_string())
}

fn check_reply(cmd: &str, reply: &str) -> Result<()> {
    ensure!(!reply.starts_with(ACPI_ERROR_PREFIX), "{cmd}: {reply}");
    Ok(())
}

/// Select a matched read/write pair using reads only. Do not fall back on
/// transport errors, other ACPI failures, or malformed firmware replies.
fn detect(call: &mut impl FnMut(&str) -> Result<String>) -> Result<(Interface, Reported)> {
    let reply = call(GBCC)?;
    let (interface, cmd, reply) = if reply == "Error: AE_NOT_FOUND" {
        (Interface::Sbco, GBCO, call(GBCO)?)
    } else {
        (Interface::Sbcc, GBCC, reply)
    };
    check_reply(cmd, &reply)?;
    Ok((interface, parse_gbcc(&reply)?))
}

pub(super) fn read() -> Result<Reported> {
    detect(&mut acpi_call).map(|(_, reported)| reported)
}

fn set_with(mode: Mode, mut call: impl FnMut(&str) -> Result<String>) -> Result<()> {
    // Detect even for standalone `set` commands; never discover an interface
    // by writing, and never retry a failed write through another method.
    let (interface, _) = detect(&mut call)?;
    let cmd = interface.command(mode);
    check_reply(&cmd, &call(&cmd)?)
}

pub(super) fn set(mode: Mode) -> Result<()> {
    set_with(mode, acpi_call)
}
