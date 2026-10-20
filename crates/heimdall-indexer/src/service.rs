//! Windows service plumbing: dispatcher, control handler, install/uninstall.

use crate::{engine::Engine, log, pipe, SERVICE_NAME};
use std::io;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Services::*;

static CONSOLE: AtomicBool = AtomicBool::new(false);
static ENGINE: OnceLock<std::sync::Arc<Engine>> = OnceLock::new();
static mut STATUS_HANDLE: SERVICE_STATUS_HANDLE = null_mut();

pub fn set_console() {
    CONSOLE.store(true, Ordering::Relaxed);
}

pub fn is_console() -> bool {
    CONSOLE.load(Ordering::Relaxed)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Hand the main thread to the service control manager. Returns false when
/// the process was not started by the SCM.
pub fn dispatch() -> bool {
    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW { lpServiceName: name.as_mut_ptr(), lpServiceProc: Some(service_main) },
        SERVICE_TABLE_ENTRYW { lpServiceName: null_mut(), lpServiceProc: None },
    ];
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) != 0 }
}

fn set_state(state: SERVICE_STATUS_CURRENT_STATE, wait_hint: u32) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_PRESHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: NO_ERROR,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: wait_hint,
    };
    unsafe { SetServiceStatus(STATUS_HANDLE, &status) };
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let name = wide(SERVICE_NAME);
    STATUS_HANDLE = RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(handler), null());
    if STATUS_HANDLE.is_null() {
        return;
    }
    set_state(SERVICE_RUNNING, 0);
    log("service started");
    let engine = ENGINE.get_or_init(Engine::start).clone();
    std::thread::Builder::new().name("pipe".into()).spawn(move || pipe::serve(engine)).ok();
    // The control handler ends the process after saving; park this thread.
    loop {
        std::thread::park();
    }
}

unsafe extern "system" fn handler(control: u32, _event: u32, _data: *mut core::ffi::c_void, _ctx: *mut core::ffi::c_void) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN | SERVICE_CONTROL_PRESHUTDOWN => {
            set_state(SERVICE_STOP_PENDING, 5000);
            std::thread::spawn(|| {
                if let Some(e) = ENGINE.get() {
                    e.save_if_dirty();
                }
                log("service stopped");
                set_state(SERVICE_STOPPED, 0);
                std::process::exit(0);
            });
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

struct Sc(SC_HANDLE);
impl Drop for Sc {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseServiceHandle(self.0) };
        }
    }
}

fn check(h: SC_HANDLE) -> io::Result<Sc> {
    if h.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Sc(h))
    }
}

fn install_dir() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
        .join("Heimdall")
}

fn stop(svc: &Sc) {
    let mut st: SERVICE_STATUS = unsafe { std::mem::zeroed() };
    unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut st) };
    for _ in 0..50 {
        if unsafe { QueryServiceStatus(svc.0, &mut st) } == 0 || st.dwCurrentState == SERVICE_STOPPED {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

pub fn install() -> io::Result<String> {
    let scm = check(unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_ALL_ACCESS) })?;
    let name = wide(SERVICE_NAME);

    // Stop an existing copy so its executable can be replaced.
    let existing = unsafe { OpenServiceW(scm.0, name.as_ptr(), SERVICE_ALL_ACCESS) };
    let existing = (!existing.is_null()).then_some(Sc(existing));
    if let Some(svc) = &existing {
        stop(svc);
    }

    let dir = install_dir();
    std::fs::create_dir_all(&dir)?;
    let target = dir.join("heimdall-indexer.exe");
    let me = std::env::current_exe()?;
    if me != target {
        std::fs::copy(&me, &target)?;
    }
    let bin = wide(&format!("\"{}\"", target.display()));
    let display = wide("Heimdall Indexer");

    let svc = match existing {
        Some(svc) => {
            let ok = unsafe {
                ChangeServiceConfigW(
                    svc.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    bin.as_ptr(),
                    null(),
                    null_mut(),
                    null(),
                    null(),
                    null(),
                    display.as_ptr(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            svc
        }
        None => check(unsafe {
            CreateServiceW(
                scm.0,
                name.as_ptr(),
                display.as_ptr(),
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                bin.as_ptr(),
                null(),
                null_mut(),
                null(),
                null(),
                null(),
            )
        })?,
    };

    let mut desc = wide("Keeps Heimdall's file index current by reading the NTFS change journal.");
    let info = SERVICE_DESCRIPTIONW { lpDescription: desc.as_mut_ptr() };
    unsafe { ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_DESCRIPTION, &info as *const _ as *const _) };

    if unsafe { StartServiceW(svc.0, 0, null()) } == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(ERROR_SERVICE_ALREADY_RUNNING as i32) {
            return Err(e);
        }
    }
    Ok(format!("Heimdall Indexer installed to {} and started.", target.display()))
}

pub fn uninstall() -> io::Result<String> {
    let scm = check(unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_ALL_ACCESS) })?;
    let name = wide(SERVICE_NAME);
    let svc = check(unsafe { OpenServiceW(scm.0, name.as_ptr(), SERVICE_ALL_ACCESS) })?;
    stop(&svc);
    if unsafe { DeleteService(svc.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    std::fs::remove_file(install_dir().join("heimdall-indexer.exe")).ok();
    std::fs::remove_dir_all(crate::data_dir()).ok();
    Ok("Heimdall Indexer removed.".into())
}
