//! Discover calling conventions offline with ACPICA; never evaluate AML.

use super::Interface;
use crate::paths;
use anyhow::{Context, Result, bail, ensure};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TARGETS: [&str; 4] = [
    "_SB_.WMID.GBCO",
    "_SB_.WMID.SBCO",
    "_SB_.WMID.GBCC",
    "_SB_.WMID.SBCC",
];

// Cache only successful discovery, for this process. Failures remain retryable.
static INTERFACE: Mutex<Option<Interface>> = Mutex::new(None);

pub(super) fn interface() -> Result<Interface> {
    let mut cached = INTERFACE
        .lock()
        .map_err(|_| anyhow::anyhow!("signature cache poisoned"))?;
    if let Some(interface) = *cached {
        return Ok(interface);
    }
    let root = paths::at("/sys/firmware/acpi/tables");
    let tables = collect_tables(&root)?;
    let interface = discover(&tables, Path::new("iasl"))
        .context("ACPI signature discovery failed; no guessed interface will be used")?;
    ensure!(
        tables == collect_tables(&root)?,
        "ACPI table list changed during discovery; retry"
    );
    *cached = Some(interface);
    Ok(interface)
}

fn collect_tables(root: &Path) -> Result<Vec<PathBuf>> {
    let mut tables = Vec::new();
    for directory in [root.to_path_buf(), root.join("dynamic")] {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if directory != root && error.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => return Err(error).with_context(|| directory.display().to_string()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if (name == "DSDT" || name.starts_with("SSDT")) && entry.file_type()?.is_file() {
                tables.push(entry.path());
            }
        }
    }
    tables.sort();
    ensure!(
        tables.contains(&root.join("DSDT")),
        "missing DSDT in {}",
        root.display()
    );
    Ok(tables)
}

fn select([gbco, sbco, gbcc, sbcc]: [Option<u8>; 4]) -> Result<Interface> {
    if gbco.is_some() || sbco.is_some() {
        return match (gbco, sbco) {
            (Some(0), Some(1)) => Ok(Interface::SbcoPacked),
            (Some(0), Some(4)) => Ok(Interface::SbcoBytes),
            _ => bail!("unsupported/incomplete GBCO/SBCO signature: {gbco:?}/{sbco:?}"),
        };
    }
    ensure!(
        gbcc == Some(0) && sbcc == Some(1),
        "unsupported/incomplete GBCC/SBCC signature: {gbcc:?}/{sbcc:?}"
    );
    Ok(Interface::Sbcc)
}

fn declaration(line: &str) -> Option<(&str, u8)> {
    let fields = line
        .trim()
        .strip_prefix("Method")?
        .trim_start()
        .strip_prefix('(')?;
    let mut fields = fields.split(',');
    let name = fields.next()?.trim();
    let arguments = fields.next()?.trim().parse::<u8>().ok()?;
    (arguments <= 7).then_some((name, arguments))
}

fn record_methods(offsets: &str, aml: &[u8], methods: &mut [Option<u8>; 4]) -> Result<()> {
    for line in offsets.lines() {
        let Some((path, fields)) = line
            .trim()
            .strip_prefix("{\"")
            .and_then(|s| s.split_once("\","))
        else {
            continue;
        };
        let Some(index) = TARGETS.iter().position(|target| *target == path) else {
            continue;
        };
        let (fields, _) = fields
            .split_once('}')
            .context("invalid iasl offset entry")?;
        let values = fields
            .split(',')
            .map(|field| {
                let field = field
                    .trim()
                    .strip_prefix("0x")
                    .context("invalid iasl offset field")?;
                usize::from_str_radix(field, 16).context("invalid iasl offset field")
            })
            .collect::<Result<Vec<_>>>()?;
        let [parent, name_offset, opcode, offset, flags]: [usize; 5] = values
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid iasl offset entry: {line}"))?;
        ensure!(
            parent == 0x14 && opcode == 0x14 && flags <= 0xff,
            "target is not a valid method: {line}"
        );
        // ACPICA's Value can omit serialization bits; compare argument bits.
        ensure!(
            aml.get(offset).map(|&byte| usize::from(byte & 7)) == Some(flags & 7)
                && aml.get(name_offset..).and_then(|data| data.get(..4))
                    == Some(path.rsplit('.').next().unwrap().as_bytes()),
            "mismatched iasl method offsets: {line}"
        );
        ensure!(
            methods[index].replace((flags & 7) as u8).is_none(),
            "duplicate method: {}",
            TARGETS[index]
        );
    }
    Ok(())
}

fn run(iasl: &Path, args: &[&str], directory: &Path, deadline: Instant) -> Result<()> {
    ensure!(Instant::now() < deadline, "iasl discovery timed out");
    let log = directory.join("iasl.log");
    let output = File::create(&log)?;
    let mut child = Command::new(iasl)
        .args(args)
        .current_dir(directory)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .spawn()
        .context(
            "cannot run iasl; install ACPICA tools (acpica on Arch, acpica-tools on Debian/Ubuntu)",
        )?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let log = fs::read(&log).unwrap_or_default();
                    let tail = String::from_utf8_lossy(&log[log.len().saturating_sub(2000)..]);
                    bail!("iasl {args:?} failed ({status}): {tail}");
                }
                return Ok(());
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                result?;
                bail!("iasl discovery timed out");
            }
        }
    }
}

fn discover(tables: &[PathBuf], iasl: &Path) -> Result<Interface> {
    let output = tempfile::Builder::new()
        .prefix("hp-charge-signatures-")
        .tempdir()?;
    let directory = output.path();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut methods = [None; 4];
    for (index, table) in tables.iter().enumerate() {
        let data = fs::read(table).with_context(|| format!("{} (root needed)", table.display()))?;
        ensure!(
            data.len() >= 36 && matches!(&data[..4], b"DSDT" | b"SSDT"),
            "not a DSDT/SSDT: {}",
            table.display()
        );
        let length = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        ensure!(
            length == data.len() && data.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0,
            "bad table length/checksum: {}",
            table.display()
        );
        let stem = format!("table-{index:03}");
        let aml = format!("{stem}.aml");
        let dsl = format!("{stem}.dsl");
        fs::write(directory.join(&aml), data)?;
        // A combined namespace fails on duplicate objects in real HP tables.
        run(iasl, &["-d", &aml], directory, deadline)?;
        let source = fs::read_to_string(directory.join(&dsl))?;
        if !source.lines().filter_map(declaration).any(|(name, _)| {
            let name = name
                .rsplit('.')
                .next()
                .unwrap_or(name)
                .trim_start_matches(['\\', '^']);
            TARGETS
                .iter()
                .any(|target| target.rsplit('.').next() == Some(name))
        }) {
            continue;
        }
        // The offset table maps full paths to method flags on both old and
        // new ACPICA versions. Verify its entries against the generated AML,
        // which is never loaded. MethodFlags bits 0..2 are the argument count.
        let prefix = format!("{stem}-offsets");
        run(iasl, &["-so", "-p", &prefix, &dsl], directory, deadline)?;
        let offsets = fs::read_to_string(directory.join(format!("{prefix}.offset.h")))?;
        let aml = fs::read(directory.join(format!("{prefix}.aml")))?;
        record_methods(&offsets, &aml, &mut methods)?;
    }
    select(methods)
}
