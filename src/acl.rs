//! Windows: keeping the data directory out of reach of normal users.
//!
//! `%ProgramData%` lets every user create folders, and files inside its
//! subfolders. The service runs as LocalSystem and writes, renames and
//! deletes files in its data directory, so a folder a user created or can
//! write to would let them redirect those operations (junctions, links)
//! onto system files. The directory is therefore created owned by
//! Administrators with a protected DACL, and an existing one is only used
//! when SYSTEM or Administrators own it, it is a plain directory and holds
//! no links (junctions, symbolic or hard links).

use anyhow::{Context, Result, ensure};
use std::fs;
use std::io::ErrorKind;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::null_mut;
use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, WinBuiltinAdministratorsSid,
    WinLocalSystemSid,
};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT, GetFileInformationByHandle,
};
use windows::core::{BOOL, HSTRING};

/// Owner Administrators, which only an administrator or SYSTEM may assign,
/// whatever the default-owner policy says; DACL protected (nothing
/// inherited from ProgramData): SYSTEM and Administrators full control,
/// Users read and list (for `status`).
const DATA_DIR_SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";

/// A security descriptor allocated by the system, freed on drop.
struct LocalDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalDescriptor {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            unsafe {
                LocalFree(Some(HLOCAL(self.0.0)));
            }
        }
    }
}

fn descriptor(sddl: &str) -> Result<LocalDescriptor> {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), SDDL_REVISION_1, &mut sd, None) }
        .context("build security descriptor")?;
    Ok(LocalDescriptor(sd))
}

fn wide(path: &Path) -> HSTRING {
    HSTRING::from(path.as_os_str())
}

fn owner_trusted(path: &Path) -> Result<bool> {
    let mut owner = PSID::default();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetNamedSecurityInfoW(&wide(path), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, &mut sd)
    };
    let _sd = LocalDescriptor(sd);
    status.ok().with_context(|| format!("read owner of {}", path.display()))?;
    Ok(unsafe { IsWellKnownSid(owner, WinLocalSystemSid).as_bool() || IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool() })
}

fn is_reparse_point(meta: &fs::Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

/// A file with more than one name: a hard link, maybe to a system file.
fn is_hard_linked(path: &Path) -> Result<bool> {
    // no access rights: reads attributes of any file, never its data
    let file = fs::OpenOptions::new().access_mode(0).open(path).with_context(|| path.display().to_string())?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .with_context(|| path.display().to_string())?;
    Ok(info.nNumberOfLinks > 1)
}

/// Replaces the DACL, also on the files inside (they inherit it).
fn protect(path: &Path) -> Result<()> {
    let sd = descriptor(DATA_DIR_SDDL)?;
    let mut present = BOOL::default();
    let mut defaulted = BOOL::default();
    let mut dacl: *mut ACL = null_mut();
    unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) }.context("read DACL")?;
    let info = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    unsafe { SetNamedSecurityInfoW(&wide(path), SE_FILE_OBJECT, info, None, None, Some(dacl), None) }
        .ok()
        .with_context(|| format!("set permissions on {} (run as administrator)", path.display()))
}

fn create(path: &Path) -> Result<()> {
    let sd = descriptor(DATA_DIR_SDDL)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0.0,
        bInheritHandle: false.into(),
    };
    // unelevated, assigning the owner fails (ERROR_INVALID_OWNER)
    unsafe { CreateDirectoryW(&wide(path), Some(&attributes)) }
        .with_context(|| format!("create {} (run as administrator)", path.display()))
}

/// Refuses links planted in a directory someone else created, while
/// users could still add entries there (before `protect`).
fn no_links_inside(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| dir.display().to_string())? {
        let entry = entry.with_context(|| dir.display().to_string())?;
        let path = entry.path();
        // DirEntry metadata describes a link itself, not its target
        let meta = entry.metadata().with_context(|| path.display().to_string())?;
        ensure!(
            !is_reparse_point(&meta) && !(meta.is_file() && is_hard_linked(&path)?),
            "{} is a link this tool never creates; delete {} and retry",
            path.display(),
            dir.display()
        );
    }
    Ok(())
}

/// Makes `dir` a directory only SYSTEM and Administrators can change, or
/// refuses one that someone else could have prepared.
pub fn secure_dir(dir: &Path) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => create(dir),
        Err(e) => Err(e).with_context(|| dir.display().to_string()),
        Ok(meta) => {
            ensure!(
                meta.is_dir() && !is_reparse_point(&meta),
                "{} is not a plain directory; delete it and retry",
                dir.display()
            );
            ensure!(
                owner_trusted(dir)?,
                "{} is not owned by SYSTEM or Administrators, so another user may have prepared it; delete it and retry",
                dir.display()
            );
            no_links_inside(dir)?;
            protect(dir)
        }
    }
}
