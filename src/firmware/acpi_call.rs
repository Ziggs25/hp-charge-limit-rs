//! Linux backend: calls `\_SB.WMID.SBCC`/`GBCC` directly through the
//! acpi_call module, preferring `SBCO`/`GBCO` when available.

use super::{Mode, Reported, mode_data};
use crate::paths::{self, ACPI_CALL, LOCK};
use anyhow::{Context, Result, anyhow, ensure};
use std::fs::{self, File};
use std::io::Read;

mod signature;

const SBCC: &str = r"\_SB.WMID.SBCC";
const GBCC: &str = r"\_SB.WMID.GBCC";
const SBCO: &str = r"\_SB.WMID.SBCO";
const GBCO: &str = r"\_SB.WMID.GBCO";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interface {
    Sbcc,
    SbcoPacked,
    SbcoBytes,
}

impl Interface {
    fn getter(self) -> &'static str {
        match self {
            Self::Sbcc => GBCC,
            Self::SbcoPacked | Self::SbcoBytes => GBCO,
        }
    }

    fn command(self, mode: Mode) -> String {
        match self {
            Self::Sbcc => sbcc_command(mode),
            Self::SbcoPacked => format!("{SBCO} {:#x}", mode as u32),
            Self::SbcoBytes => {
                let [a, b, c, d] = mode_data(mode);
                format!("{SBCO} {a:#x} {b:#x} {c:#x} {d:#x}")
            }
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

struct Reply {
    size: u32,
    data: Option<[u8; 4]>,
}

/// Known replies are [status, size] or [status, size, {four bytes}].
fn checked_reply(cmd: &str, reply: &str) -> Result<Reply> {
    ensure!(!reply.starts_with(ACPI_ERROR_PREFIX), "{cmd}: {reply}");
    let bad = || anyhow!("{cmd}: unexpected firmware reply {reply}");
    let hex = |s: &str| -> Result<u32> {
        u32::from_str_radix(s.trim().strip_prefix("0x").ok_or_else(bad)?, 16).map_err(|_| bad())
    };
    let mut fields = reply
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(bad)?
        .splitn(3, ',');
    let status = hex(fields.next().ok_or_else(bad)?)?;
    let size = hex(fields.next().ok_or_else(bad)?)?;
    let data = fields
        .next()
        .map(|s| -> Result<[u8; 4]> {
            let bytes = s
                .trim()
                .strip_prefix('{')
                .and_then(|s| s.strip_suffix('}'))
                .ok_or_else(bad)?;
            let bytes: Vec<u8> = bytes
                .split(',')
                .map(|s| u8::try_from(hex(s)?).map_err(|_| bad()))
                .collect::<Result<_>>()?;
            bytes.try_into().map_err(|_| bad())
        })
        .transpose()?;
    ensure!(status == 0, "{cmd}: firmware status {status:#x}: {reply}");
    Ok(Reply { size, data })
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

fn check_write_reply(interface: Interface, cmd: &str, reply: &str) -> Result<()> {
    let parsed = checked_reply(cmd, reply)?;
    ensure!(parsed.size == 0, "{cmd}: unexpected write reply {reply}");
    match interface {
        // SBCO(1) returns a two-element package, without a buffer.
        Interface::SbcoPacked => ensure!(
            parsed.data.is_none(),
            "{cmd}: unexpected write reply {reply}"
        ),
        Interface::SbcoBytes => ensure!(
            parsed.data.is_some(),
            "{cmd}: missing status buffer: {reply}"
        ),
        // SBCC(1) on 8C99 returns [0, 0], without a buffer.
        Interface::Sbcc => {}
    }
    if let Some(data) = parsed.data {
        let status = u32::from_le_bytes(data);
        ensure!(status == 0, "{cmd}: firmware status {status:#x}: {reply}");
    }
    Ok(())
}

fn read_with(
    interface: Interface,
    call: &mut impl FnMut(&str) -> Result<String>,
) -> Result<Reported> {
    let cmd = interface.getter();
    let reply = call(cmd)?;
    let parsed = checked_reply(cmd, &reply)?;
    ensure!(parsed.size == 4, "{cmd}: unexpected read reply {reply}");
    let data = parsed
        .data
        .ok_or_else(|| anyhow!("{cmd}: missing mode buffer: {reply}"))?;
    Ok(Reported(data[0]))
}

pub(super) fn read() -> Result<Reported> {
    read_with(signature::interface()?, &mut acpi_call)
}

fn set_with(
    interface: Interface,
    mode: Mode,
    mut call: impl FnMut(&str) -> Result<String>,
) -> Result<()> {
    // Check the matched getter before writing. Never retry a failed read or
    // write through another method, or discover an interface by writing.
    read_with(interface, &mut call)?;
    let cmd = interface.command(mode);
    check_write_reply(interface, &cmd, &call(&cmd)?)
}

pub(super) fn set(mode: Mode) -> Result<()> {
    set_with(signature::interface()?, mode, acpi_call)
}
