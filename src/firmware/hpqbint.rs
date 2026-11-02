//! Windows backend: the HP BIOS WMI interface `root\wmi:hpqBIntM`.
//!
//! Windows' wmiacpi driver turns `hpqBIOSInt<N>` calls into the firmware's
//! `\_SB.WMID.WMAA`, which hands the request buffer to WHCM:
//!
//! ```text
//!   0x00 Sign "SECU"   0x04 Command   0x08 CommandType   0x0C Size   0x10 data
//! ```
//!
//! Command 1 (read) / 2 (write) with CommandType 0x2B reach GBCC / SBCC,
//! and SBCC reads its mode dword at 0x10: exactly the buffer the Linux
//! backend passes to SBCC through acpi_call. The method number sets the
//! reply size: hpqBIOSInt0 returns no data, hpqBIOSInt4 four bytes, which
//! is what GBCC answers and SBCC needs.
//!
//! This is the same slot mainline Linux hp-wmi uses for charge_behaviour
//! (`HPWMI_BATTERY_CHARGE_MODE`, Spectre boards 8C15/8C17, where the AML
//! names it GBCO/SBCO): mode in bits 15:8, 0x00 auto, 0x05 inhibit. Only
//! the legacy 1/2 commands dispatch this way; under the 0x20008 command
//! family that OMEN tools use, type 0x2B is something else.

use super::{Mode, Reported, mode_data};
use crate::wmi::{Namespace, Object, Variant};
use anyhow::{Context, Result, anyhow};

const NAMESPACE: &str = r"root\wmi";
const INTERFACE_CLASS: &str = "hpqBIntM";
const INPUT_CLASS: &str = "hpqBDataIn";
const SIGN: &[u8; 4] = b"SECU";
const READ: u32 = 1;
const WRITE: u32 = 2;
/// Battery charge control: GBCC on read, SBCC on write.
const CHARGE_CONTROL: u32 = 0x2B;
const NO_REPLY: &str = "hpqBIOSInt0";
const REPLY_4: &str = "hpqBIOSInt4";

/// rwReturnCode: input size or data the firmware won't take.
const INVALID_PARAMETERS: u32 = 5;

/// What WHCM's return code means (rwReturnCode).
fn describe(code: u32) -> &'static str {
    match code {
        0 => "ok",
        2 => "signature rejected",
        3 => "command not supported",
        4 => "command type not supported on this firmware",
        INVALID_PARAMETERS => "data size rejected",
        _ => "unknown error",
    }
}

/// The interface instance, e.g. `ACPI\PNP0C14\0_0`; errors if absent.
pub fn interface() -> Result<String> {
    let wmi = Namespace::connect(NAMESPACE)?;
    let object = interface_object(&wmi)?;
    Ok(object.string("InstanceName").unwrap_or_else(|| "?".into()))
}

fn interface_object(wmi: &Namespace) -> Result<Object> {
    wmi.first(INTERFACE_CLASS)?.ok_or_else(|| {
        anyhow!("no {INTERFACE_CLASS} instance in {NAMESPACE} (not an HP laptop, or not run as administrator)")
    })
}

/// One BIOS call; returns the reply data, or the BIOS return code when it
/// is not 0. An empty `data` sends Size 0 and no data.
fn call(command: u32, data: &[u8], method: &str) -> Result<Result<Vec<u8>, u32>> {
    let wmi = Namespace::connect(NAMESPACE)?;
    let target = interface_object(&wmi)?.relative_path()?;

    let request = wmi.class(INPUT_CLASS)?.spawn()?;
    request.put("Sign", &Variant::from_bytes(SIGN)?)?;
    request.put("Command", &Variant::from_u32(command))?;
    request.put("CommandType", &Variant::from_u32(CHARGE_CONTROL))?;
    request.put("Size", &Variant::from_u32(data.len() as u32))?;
    if !data.is_empty() {
        request.put("hpqBData", &Variant::from_bytes(data)?)?;
    }

    let input = wmi.class(INTERFACE_CLASS)?.method_input(method)?;
    input.put("InData", &Variant::from_object(&request)?)?;
    let output = wmi.exec_method(&target, method, &input)?;
    let reply = output.get("OutData")?.object().context("OutData")?;

    let code = reply.get("rwReturnCode")?.u32().context("rwReturnCode")?;
    if code != 0 {
        return Ok(Err(code));
    }
    if method == NO_REPLY {
        return Ok(Ok(Vec::new()));
    }
    Ok(Ok(reply.get("Data")?.bytes().context("Data")?))
}

fn bios_ok(command: u32, reply: Result<Vec<u8>, u32>) -> Result<Vec<u8>> {
    reply.map_err(|code| {
        anyhow!("BIOS command {command} type {CHARGE_CONTROL:#04x}: return code {code} ({})", describe(code))
    })
}

pub(super) fn read() -> Result<Reported> {
    // Four zero bytes, like mainline hp-wmi: an empty input faults on some
    // firmware. Firmware that insists on none answers 5; mainline then
    // retries with size 0 (zero_insize_support), and so do we.
    let reply = match call(READ, &[0; 4], REPLY_4)? {
        Err(INVALID_PARAMETERS) => call(READ, &[], REPLY_4)?,
        reply => reply,
    };
    let data = bios_ok(READ, reply)?;
    let first = data.first().copied().ok_or_else(|| anyhow!("empty GBCC reply"))?;
    Ok(Reported(first))
}

pub(super) fn set(mode: Mode) -> Result<()> {
    bios_ok(WRITE, call(WRITE, &mode_data(mode), NO_REPLY)?).map(drop)
}
