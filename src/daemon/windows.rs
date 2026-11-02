//! Windows: run as a service when the Service Control Manager started us,
//! otherwise in the console until Ctrl+C.
//!
//! The service tells exits apart directly: STOP is a service stop,
//! PRESHUTDOWN a shutdown or restart. Sleep and hibernate arrive as power
//! events. Windows' default "shut down" with Fast Startup is a hibernate
//! that never stops services, so the suspend event is what sets the
//! off-state mode then.

use super::Control;
use super::limiter::Exit;
use crate::cli::DaemonOpts;
use crate::paths;
use crate::service::{self, SERVICE};
use anyhow::{Context, Result, ensure};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::os::windows::io::IntoRawHandle;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetStdHandle,
};
use windows::core::BOOL;
use windows_service::service::{
    PowerEventParam, ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle};
use windows_service::{define_windows_service, service_dispatcher};

/// ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: not started by the SCM.
const NOT_A_SERVICE: i32 = 1063;
/// How long a sleep may wait for the firmware write.
const SUSPEND_WAIT: Duration = Duration::from_secs(5);
/// How long the loop gets to set the stop mode when the console closes.
const CLOSE_GRACE: Duration = Duration::from_secs(3);
/// The log is restarted (previous kept as .old) beyond this size.
const LOG_MAX_BYTES: u64 = 1024 * 1024;
const FAILED: u32 = 1;

static OPTS: OnceLock<DaemonOpts> = OnceLock::new();
static CONSOLE: OnceLock<Mutex<Sender<Control>>> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

pub fn run(o: &DaemonOpts) -> Result<()> {
    let _ = OPTS.set(o.clone());
    match service_dispatcher::start(SERVICE, ffi_service_main) {
        Ok(()) => Ok(()),
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(NOT_A_SERVICE) => console(o),
        Err(e) => Err(e).context("service dispatcher"),
    }
}

fn console(o: &DaemonOpts) -> Result<()> {
    let state = service::service_state();
    ensure!(
        !matches!(state.as_str(), "active" | "starting"),
        "the {SERVICE} service is {state}; two loops would fight over the mode, so stop it first (sc.exe stop {SERVICE})"
    );
    paths::secure_data_dir()?;
    let (tx, rx) = mpsc::channel();
    let _ = CONSOLE.set(Mutex::new(tx));
    unsafe { SetConsoleCtrlHandler(Some(console_ctrl), true) }.context("Ctrl+C handler")?;
    println!("running in the console; Ctrl+C stops and sets --stop-mode");
    super::run_loop(o, rx)
}

unsafe extern "system" fn console_ctrl(event: u32) -> BOOL {
    if let Some(tx) = CONSOLE.get()
        && let Ok(tx) = tx.lock()
    {
        let _ = tx.send(Control::Exit(Exit::ServiceStop));
    }
    // Ctrl+C/Break let the process continue; close, logoff and shutdown
    // end it when this returns, so give the loop time to write.
    if event != CTRL_C_EVENT && event != CTRL_BREAK_EVENT {
        sleep(CLOSE_GRACE);
    }
    true.into()
}

/// Sends stdout/stderr to the log file: a service has no console. The
/// data directory must already be secured.
fn log_to_file() {
    let path = paths::at(paths::LOG);
    if fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
        let _ = fs::rename(&path, path.with_extension("log.old"));
    }
    let Ok(out) = OpenOptions::new().create(true).append(true).open(&path) else { return };
    let Ok(err) = out.try_clone() else { return };
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, HANDLE(out.into_raw_handle()));
        let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE(err.into_raw_handle()));
    }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    println!("--- service start (unix time {secs})");
}

fn service_main(_arguments: Vec<OsString>) {
    let Some(o) = OPTS.get() else { return };
    if let Err(e) = run_service(o) {
        eprintln!("service failed: {e:#}");
    }
}

fn status(state: ServiceState, accept: ServiceControlAccept, code: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: if code == 0 { ServiceExitCode::Win32(0) } else { ServiceExitCode::ServiceSpecific(code) },
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    }
}

fn run_service(o: &DaemonOpts) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let handler = move |control| match control {
        ServiceControl::Stop => {
            let _ = tx.send(Control::Exit(Exit::ServiceStop));
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Preshutdown => {
            let _ = tx.send(Control::Exit(Exit::Shutdown));
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::PowerEvent(PowerEventParam::Suspend) => {
            // Windows sleeps once this returns: wait for the write
            let (done, written) = mpsc::channel();
            if tx.send(Control::Suspend(done)).is_ok() {
                let _ = written.recv_timeout(SUSPEND_WAIT);
            }
            ServiceControlHandlerResult::NoError
        }
        // ResumeAutomatic comes on every wake; the others too in some
        // cases. Limiting waits for a resume, so any of them counts (twice
        // is harmless).
        ServiceControl::PowerEvent(
            PowerEventParam::ResumeAutomatic | PowerEventParam::ResumeSuspend | PowerEventParam::ResumeCritical,
        ) => {
            let _ = tx.send(Control::Resume);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::PowerEvent(_) | ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let handle: ServiceStatusHandle = service_control_handler::register(SERVICE, handler).context("register service")?;
    // Without a directory only SYSTEM and Administrators control, the
    // service writes nothing and does not run (see `acl`).
    let result = paths::secure_data_dir().and_then(|_| {
        log_to_file();
        let accept = ServiceControlAccept::STOP | ServiceControlAccept::PRESHUTDOWN | ServiceControlAccept::POWER_EVENT;
        handle.set_service_status(status(ServiceState::Running, accept, 0)).context("report running")?;
        super::run_loop(o, rx)
    });
    if let Err(e) = &result {
        eprintln!("daemon failed: {e:#}");
    }
    let code = if result.is_ok() { 0 } else { FAILED };
    handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty(), code))?;
    result
}
