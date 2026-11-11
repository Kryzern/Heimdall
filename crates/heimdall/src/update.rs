//! Update checks against GitHub releases. A newer release is downloaded in the
//! background and installed with its own setup, run silently, which restarts
//! Heimdall when it's done. Copies installed some other way (Scoop, the zip)
//! are only told about it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_HIDE};

pub const WM_UPDATE: u32 = 0x8000 + 22;

const LATEST: &str = "https://api.github.com/repos/Kryzern/Heimdall/releases/latest";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    /// The release's setup.exe, if it has one.
    pub setup: Option<String>,
    pub page: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Checking,
    Latest,
    Available(Release),
    Downloading(Release),
    /// Downloaded and waiting to be installed.
    Ready(Release, PathBuf),
    Failed,
}

/// How this copy was installed, which decides whether it can update itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// By the setup, which can install the next version over it.
    Setup,
    Scoop,
    /// Unzipped somewhere by hand.
    Portable,
}

pub fn install_kind() -> Install {
    let Ok(exe) = std::env::current_exe() else { return Install::Portable };
    if exe.to_string_lossy().to_ascii_lowercase().contains("\\scoop\\apps\\") {
        Install::Scoop
    } else if exe.with_file_name("unins000.exe").exists() {
        Install::Setup
    } else {
        Install::Portable
    }
}

pub struct Updater {
    state: Arc<Mutex<State>>,
    hwnd: isize,
}

impl Updater {
    pub fn new(hwnd: HWND) -> Self {
        Updater { state: Arc::new(Mutex::new(State::Idle)), hwnd: hwnd.0 as isize }
    }

    pub fn state(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or(State::Idle)
    }

    pub fn busy(&self) -> bool {
        matches!(self.state(), State::Checking | State::Downloading(_))
    }

    /// Ask GitHub for the latest release. Posts WM_UPDATE when done.
    pub fn check(&self) {
        if self.busy() || matches!(self.state(), State::Ready(..)) {
            return;
        }
        self.run(State::Checking, || match latest() {
            Some(r) if newer(&r.version, env!("CARGO_PKG_VERSION")) => State::Available(r),
            Some(_) => State::Latest,
            None => State::Failed,
        });
    }

    /// Download the setup of an available release. Posts WM_UPDATE when done.
    pub fn download(&self) {
        let State::Available(r) = self.state() else { return };
        let Some(url) = r.setup.clone() else { return };
        self.run(State::Downloading(r.clone()), move || {
            let dir = std::env::temp_dir().join("Heimdall-update");
            std::fs::create_dir_all(&dir).ok();
            let path = dir.join(format!("Heimdall-{}-x64-setup.exe", r.version));
            match get(&url) {
                // A setup is a few megabytes; anything tiny is an error page.
                Some(body) if body.len() > 100_000 && std::fs::write(&path, &body).is_ok() => State::Ready(r, path),
                _ => {
                    crate::log("update: download failed");
                    State::Failed
                }
            }
        });
    }

    /// The setup has been started: it takes it from here.
    pub fn clear(&self) {
        if let Ok(mut s) = self.state.lock() {
            *s = State::Idle;
        }
    }

    fn run(&self, now: State, work: impl FnOnce() -> State + Send + 'static) {
        if let Ok(mut s) = self.state.lock() {
            *s = now;
        }
        let state = self.state.clone();
        let hwnd = self.hwnd;
        std::thread::Builder::new()
            .name("update".into())
            .spawn(move || {
                let next = work();
                if let Ok(mut s) = state.lock() {
                    *s = next;
                }
                unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_UPDATE, WPARAM(0), LPARAM(0)).ok() };
            })
            .ok();
    }
}

/// Start the downloaded setup. It closes Heimdall, installs over it and starts
/// it again in the background; the file indexer service is left as it is.
pub fn install(setup: &Path, version: &str) -> bool {
    std::fs::write(attempt_file(), version).ok();
    let args = w!("/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /UPDATE");
    let r = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(setup.as_os_str()), args, PCWSTR::null(), SW_HIDE) };
    r.0 as isize > 32
}

/// Is the indexer service running an older copy than the one beside us? A
/// quiet update doesn't touch the service, since replacing it needs admin.
pub fn indexer_stale() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let ours = exe.with_file_name("heimdall-indexer.exe");
    let installed = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
        .join("Heimdall")
        .join("heimdall-indexer.exe");
    match (std::fs::metadata(ours), std::fs::metadata(installed)) {
        // The service copies itself there with its timestamp intact.
        (Ok(a), Ok(b)) => a.len() != b.len() || a.modified().ok() != b.modified().ok(),
        _ => false,
    }
}

/// Was the user already told about `version`? Each version is announced once.
pub fn announced(version: &str) -> bool {
    std::fs::read_to_string(crate::config::dir().join("update-announced.txt")).is_ok_and(|v| v.trim() == version)
}

pub fn set_announced(version: &str) {
    std::fs::write(crate::config::dir().join("update-announced.txt"), version).ok();
}

/// Did an automatic update just install this version? True once.
pub fn just_updated() -> bool {
    let done = std::fs::read_to_string(attempt_file()).is_ok_and(|v| v.trim() == env!("CARGO_PKG_VERSION"));
    if done {
        std::fs::remove_file(attempt_file()).ok();
    }
    done
}

/// The release notes for `version`.
pub fn page(version: &str) -> String {
    format!("https://github.com/Kryzern/Heimdall/releases/tag/v{version}")
}

fn attempt_file() -> PathBuf {
    crate::config::dir().join("update-attempt.txt")
}

/// Was installing `version` already tried? Still running the old version
/// afterwards means it failed, so it isn't tried quietly again.
pub fn tried(version: &str) -> bool {
    std::fs::read_to_string(attempt_file()).is_ok_and(|v| v.trim() == version)
}

/// Is version `a` ("1.2.0" or "v1.2.0") later than `b`?
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.trim().trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
    };
    parse(a) > parse(b)
}

fn latest() -> Option<Release> {
    let body = get(LATEST)?;
    let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
    let version = v["tag_name"].as_str()?.trim_start_matches('v').to_string();
    let setup = v["assets"].as_array()?.iter().find_map(|a| {
        let name = a["name"].as_str()?;
        name.ends_with("-x64-setup.exe").then(|| a["browser_download_url"].as_str().map(str::to_string))?
    });
    let page = v["html_url"].as_str().unwrap_or("https://github.com/Kryzern/Heimdall/releases/latest").to_string();
    Some(Release { version, setup, page })
}

/// HTTPS GET, following redirects. None on any failure or a non-200 status.
fn get(url: &str) -> Option<Vec<u8>> {
    struct H(*mut core::ffi::c_void);
    impl Drop for H {
        fn drop(&mut self) {
            unsafe { WinHttpCloseHandle(self.0).ok() };
        }
    }
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    let agent = HSTRING::from(format!("Heimdall/{}", env!("CARGO_PKG_VERSION")));
    unsafe {
        let session = H(WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return None;
        }
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000).ok();
        let conn = H(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if conn.0.is_null() {
            return None;
        }
        let req = H(WinHttpOpenRequest(
            conn.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            return None;
        }
        WinHttpSendRequest(req.0, None, None, 0, 0, 0).ok()?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).ok()?;
        let mut status = 0u32;
        let mut len = 4u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .ok()?;
        if status != 200 {
            crate::log(format!("update: {url} returned {status}"));
            return None;
        }
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 << 10];
        loop {
            let mut read = 0u32;
            WinHttpReadData(req.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut read).ok()?;
            if read == 0 {
                break;
            }
            out.extend_from_slice(&buf[..read as usize]);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn versions() {
        assert!(newer("v1.0.5", "1.0.4"));
        assert!(newer("1.1.0", "1.0.10"));
        assert!(newer("2.0", "1.9.9"));
        assert!(!newer("1.0.4", "1.0.4"));
        assert!(!newer("v1.0.3", "1.0.4"));
        assert!(!newer("1.0.10", "1.1.0"));
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore]
    fn latest_release() {
        let r = super::latest().expect("reach GitHub");
        eprintln!("{r:?}");
        assert!(r.setup.is_some());
        let body = super::get(r.setup.as_deref().unwrap()).expect("download");
        eprintln!("setup: {} bytes, starts {:?}", body.len(), &body[..2]);
        assert!(body.len() > 100_000 && &body[..2] == b"MZ");
    }
}
