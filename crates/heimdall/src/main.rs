//! Heimdall: a fast, minimal launcher for Windows.

#![windows_subsystem = "windows"]

mod actions;
mod app;
mod apps;
mod config;
mod edit;
mod files;
mod flow;
mod gfx;
mod history;
mod items;
mod menu;
mod platform;
mod shell;
mod sources;

use app::{App, WM_APP_SHOW};
use std::cell::RefCell;
use windows::core::{w, BOOL};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED};
use windows::Win32::Graphics::Gdi::ValidateRect;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_BELOW_NORMAL,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub fn log(msg: impl AsRef<str>) {
    use std::io::Write;
    let dir = config::dir();
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("heimdall.log");
    if std::fs::metadata(&path).map(|m| m.len() > 512 << 10).unwrap_or(false) {
        std::fs::remove_file(&path).ok();
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        writeln!(f, "[{}] {}", heimdall_core::query::now_unix(), msg.as_ref()).ok();
    }
}

/// `HEIMDALL_TRACE=1` logs slow messages and frames to heimdall.log.
pub fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("HEIMDALL_TRACE").is_some())
}

/// Run `f` against the app unless it is already borrowed (re-entrant message).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        ValidateRect(Some(h), None).ok().ok();
        return LRESULT(0);
    }
    if msg == WM_MOUSEACTIVATE {
        return LRESULT(MA_ACTIVATE as isize);
    }
    match with_app(|app| app.handle(msg, wp, lp)).flatten() {
        Some(r) => r,
        None => DefWindowProcW(h, msg, wp, lp),
    }
}

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _obj: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    with_app(|app| app.on_foreground(hwnd));
}

fn main() {
    unsafe {
        let _mutex = CreateMutexW(None, true, w!("Local\\Heimdall.Launcher"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // Already running: ask that instance to open instead.
            if let Ok(h) = FindWindowW(w!("HeimdallWindow"), None) {
                PostMessageW(Some(h), WM_APP_SHOW, WPARAM(0), LPARAM(0)).ok();
            }
            return;
        }

        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).ok();
        // The UI thread outranks search work; file search never steals a frame.
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL).ok();
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_sub(1).max(1);
        rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .start_handler(|_| {
                SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL).ok();
            })
            .build_global()
            .ok();
        OleInitialize(None).ok();

        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("HeimdallWindow"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let hwnd = match CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("HeimdallWindow"),
            w!("Heimdall"),
            WS_POPUP,
            0,
            0,
            64,
            64,
            None,
            None,
            Some(instance.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log(format!("CreateWindowEx failed: {e}"));
                return;
            }
        };
        let off = BOOL(1);
        DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as *const _, 4).ok();

        let app = App::new(hwnd);
        APP.with(|a| *a.borrow_mut() = Some(app));

        let _hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(on_foreground), 0, 0, WINEVENT_OUTOFCONTEXT);

        if !std::env::args().any(|a| a == "--background") {
            PostMessageW(Some(hwnd), WM_APP_SHOW, WPARAM(0), LPARAM(0)).ok();
        }

        // Input always goes first: drain every pending message, then draw at
        // most one frame when the compositor says it's ready.
        let mut msg = MSG::default();
        'run: loop {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'run;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            match with_app(|app| app.wants_frame()).flatten() {
                Some(ready) => {
                    let handles = [ready];
                    let r = if ready.is_invalid() {
                        WAIT_OBJECT_0
                    } else {
                        MsgWaitForMultipleObjectsEx(Some(&handles), 100, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
                    };
                    // Ready (or the wait timed out): draw. Otherwise input arrived first.
                    if r == WAIT_OBJECT_0 || r == WAIT_TIMEOUT {
                        with_app(|app| app.tick());
                    }
                }
                None => {
                    WaitMessage().ok();
                }
            }
        }
        with_app(|app| app.shutdown());
    }
}
