//! Doing things: launching, revealing, copying, switching windows, power actions.

use crate::items::SysCmd;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::LockWorkStation;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub fn open(target: &str, admin: bool) -> bool {
    let verb = if admin { w!("runas") } else { w!("open") };
    let r = unsafe { ShellExecuteW(None, verb, &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    r.0 as usize > 32
}

/// Open Explorer with the item selected.
pub fn reveal(path: &str) {
    unsafe {
        let pidl = ILCreateFromPathW(&HSTRING::from(path));
        if pidl.is_null() {
            return;
        }
        SHOpenFolderAndSelectItems(pidl, None, 0).ok();
        ILFree(Some(pidl));
    }
}

pub fn copy_text(hwnd: HWND, s: &str) {
    let wide: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        EmptyClipboard().ok();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                GlobalUnlock(h).ok();
                // CF_UNICODETEXT; the clipboard owns the memory after this.
                SetClipboardData(13, Some(HANDLE(h.0))).ok();
            }
        }
        CloseClipboard().ok();
    }
}

pub fn clipboard_text(hwnd: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let r = (|| {
            let h = GetClipboardData(13).ok()?;
            let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            GlobalUnlock(HGLOBAL(h.0)).ok();
            Some(s)
        })();
        CloseClipboard().ok();
        r
    }
}

pub fn switch_to(hwnd: isize) {
    let h = HWND(hwnd as *mut _);
    unsafe {
        if IsIconic(h).as_bool() {
            ShowWindow(h, SW_RESTORE).ok().ok();
        }
        SetForegroundWindow(h).ok().ok();
    }
}

pub fn system(cmd: SysCmd, hwnd: HWND) {
    let shutdown = |args: &str| {
        unsafe { ShellExecuteW(None, w!("open"), w!("shutdown.exe"), &HSTRING::from(args), PCWSTR::null(), SW_HIDE) };
    };
    unsafe {
        match cmd {
            SysCmd::Lock => {
                LockWorkStation().ok();
            }
            SysCmd::Sleep => {
                SetSuspendState(false, false, false);
            }
            SysCmd::Restart => shutdown("/r /t 0"),
            SysCmd::Shutdown => shutdown("/s /t 0"),
            SysCmd::SignOut => shutdown("/l"),
            SysCmd::EmptyBin => {
                SHEmptyRecycleBinW(Some(hwnd), PCWSTR::null(), 0).ok();
            }
            SysCmd::Settings => {
                open("ms-settings:", false);
            }
        }
    }
}

/// Drag a file out of Heimdall into any app (Explorer, browsers, chat apps).
pub fn drag(hwnd: HWND, path: &str) -> bool {
    unsafe {
        let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path), None) else {
            return false;
        };
        let Ok(data) = item.BindToHandler::<_, IDataObject>(None, &BHID_DataObject) else {
            return false;
        };
        SHDoDragDrop(Some(hwnd), &data, None, DROPEFFECT_COPY | DROPEFFECT_LINK | DROPEFFECT_MOVE).is_ok()
    }
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
