//! Minimal client for Everything's WM_COPYDATA query interface (QUERY2).

use std::cell::Cell;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const COPYDATA_QUERY2W: usize = 18;
const REPLY_ID: u32 = 0x4844_4C00;
const REQUEST_FILE_NAME: u32 = 0x1;
const REQUEST_PATH: u32 = 0x2;
const SORT_NAME_ASCENDING: u32 = 1;

thread_local! {
    static REPLY: Cell<Option<u32>> = const { Cell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct Client {
    target: HWND,
    reply: HWND,
    pub class: String,
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        let cds = &*(lp as *const COPYDATASTRUCT);
        if cds.dwData == REPLY_ID as usize && cds.cbData >= 8 {
            // EVERYTHING_IPC_LIST2 starts with totitems, numitems.
            let total = *(cds.lpData as *const u32);
            REPLY.with(|r| r.set(Some(total)));
            return 1;
        }
    }
    DefWindowProcW(h, msg, wp, lp)
}

impl Client {
    pub fn connect() -> Option<Client> {
        for class in ["EVERYTHING_TASKBAR_NOTIFICATION_(1.5a)", "EVERYTHING_TASKBAR_NOTIFICATION"] {
            let target = unsafe { FindWindowW(wide(class).as_ptr(), null()) };
            if target.is_null() {
                continue;
            }
            let cls = wide("HeimdallBenchReply");
            unsafe {
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: GetModuleHandleW(null()),
                    lpszClassName: cls.as_ptr(),
                    ..std::mem::zeroed()
                };
                RegisterClassW(&wc);
                let reply = CreateWindowExW(0, cls.as_ptr(), null(), 0, 0, 0, 0, 0, HWND_MESSAGE, null_mut(), wc.hInstance, null());
                if reply.is_null() {
                    return None;
                }
                return Some(Client { target, reply, class: class.to_string() });
            }
        }
        None
    }

    /// Send a query; returns Everything's total match count once it replies.
    pub fn query(&mut self, search: &str, max: u32) -> Option<u32> {
        let header = [
            self.reply as usize as u32,
            REPLY_ID,
            0,
            0,
            max,
            REQUEST_FILE_NAME | REQUEST_PATH,
            SORT_NAME_ASCENDING,
        ];
        let mut buf: Vec<u8> = header.iter().flat_map(|v| v.to_le_bytes()).collect();
        buf.extend(search.encode_utf16().chain(Some(0)).flat_map(|c| c.to_le_bytes()));
        let cds = COPYDATASTRUCT { dwData: COPYDATA_QUERY2W, cbData: buf.len() as u32, lpData: buf.as_mut_ptr() as *mut _ };
        REPLY.with(|r| r.set(None));
        let ok = unsafe { SendMessageW(self.target, WM_COPYDATA, self.reply as WPARAM, &cds as *const _ as LPARAM) };
        if ok == 0 {
            return None;
        }
        // The reply normally arrives during SendMessage; pump briefly if not.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(n) = REPLY.with(|r| r.get()) {
                return Some(n);
            }
            if std::time::Instant::now() > deadline {
                return None;
            }
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    DispatchMessageW(&msg);
                }
            }
            std::thread::yield_now();
        }
    }
}

/// Translate Heimdall filter syntax to Everything syntax where they differ.
pub fn translate(q: &str) -> String {
    q.split_whitespace()
        .map(|w| match w.split_once(':') {
            Some(("in", v)) => format!("path:{v}"),
            Some(("type", "dir" | "folder")) => "folder:".into(),
            Some(("type", "file")) => "file:".into(),
            _ => w.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// (working set, private bytes) of Everything.exe.
pub fn memory() -> Option<(usize, usize)> {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let target = ["EVERYTHING_TASKBAR_NOTIFICATION_(1.5a)", "EVERYTHING_TASKBAR_NOTIFICATION"]
        .iter()
        .map(|c| unsafe { FindWindowW(wide(c).as_ptr(), null()) })
        .find(|h| !h.is_null())?;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(target, &mut pid) };
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let mut pmc: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(h, &mut pmc as *mut _ as *mut _, pmc.cb) };
    unsafe { CloseHandle(h) };
    (ok != 0).then_some((pmc.WorkingSetSize, pmc.PrivateUsage))
}
