//! The background service: systemd on Linux, the Service Control Manager
//! on Windows.

#[cfg(target_os = "linux")]
mod systemd;
#[cfg(target_os = "linux")]
pub use systemd::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;
