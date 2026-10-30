//! Small OS queries: theme, power state, monitors, foreground app, screen capture,
//! open windows, autostart.

use crate::gfx::Color;
use windows::core::{w, BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

fn reg_dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_DWORD, None, Some(&mut data as *mut _ as *mut _), Some(&mut size))
            .ok()
            .ok()?;
    }
    Some(data)
}

const PERSONALIZE: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");

pub fn system_dark() -> bool {
    reg_dword(PERSONALIZE, w!("AppsUseLightTheme")).map(|v| v == 0).unwrap_or(true)
}

pub fn transparency_enabled() -> bool {
    reg_dword(PERSONALIZE, w!("EnableTransparency")).map(|v| v != 0).unwrap_or(true)
}

pub fn system_accent() -> Color {
    // AccentColor is stored as 0xAABBGGRR.
    match reg_dword(w!(r"Software\Microsoft\Windows\DWM"), w!("AccentColor")) {
        Some(v) => Color::rgba(
            (v & 0xFF) as f32 / 255.0,
            ((v >> 8) & 0xFF) as f32 / 255.0,
            ((v >> 16) & 0xFF) as f32 / 255.0,
            1.0,
        ),
        None => Color::rgba(0.38, 0.55, 1.0, 1.0),
    }
}

pub fn parse_hex(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Color::rgba(((v >> 16) & 0xFF) as f32 / 255.0, ((v >> 8) & 0xFF) as f32 / 255.0, (v & 0xFF) as f32 / 255.0, 1.0))
}

pub fn battery_saver() -> bool {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s).is_ok() && s.SystemStatusFlag == 1 }
}

pub fn remote_session() -> bool {
    unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
}

pub struct Monitor {
    pub work: RECT,
    pub dpi: u32,
}

/// The monitor under the mouse cursor.
pub fn active_monitor() -> Monitor {
    unsafe {
        let mut pt = POINT::default();
        GetCursorPos(&mut pt).ok();
        let hm = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetMonitorInfoW(hm, &mut mi).ok().ok();
        let (mut dx, mut dy) = (96u32, 96u32);
        GetDpiForMonitor(hm, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).ok();
        Monitor { work: mi.rcWork, dpi: dx }
    }
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn process_path(h: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        CloseHandle(proc).ok();
        ok.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Is the foreground window covering its whole monitor (a game or a video)?
pub fn foreground_fullscreen(fg: HWND) -> bool {
    if fg.0.is_null() {
        return false;
    }
    let cls = class_name(fg);
    if matches!(cls.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
        return false;
    }
    unsafe {
        // A maximized window's rect spills past the monitor by its resize
        // borders, so on a screen without a visible taskbar it looks
        // fullscreen. Real fullscreen (games, F11, video players) is never
        // maximized: browsers restore the window before going fullscreen.
        if IsZoomed(fg).as_bool() {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let hm = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(hm, &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

/// Copy a screen rectangle into top-down BGRA.
pub fn capture(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let out = match CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(dib) => {
                let old = SelectObject(mem, dib.into());
                let ok = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT).is_ok();
                let px = (ok && !bits.is_null())
                    .then(|| std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec());
                SelectObject(mem, old);
                DeleteObject(dib.into()).ok().ok();
                px
            }
            Err(_) => None,
        };
        DeleteDC(mem).ok().ok();
        ReleaseDC(None, screen);
        out
    }
}

pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub exe: Option<String>,
}

/// Top-level, visible, switchable windows (what Alt+Tab shows).
pub fn windows(skip: HWND) -> Vec<WindowInfo> {
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<HWND>);
        out.push(h);
        BOOL(1)
    }
    let mut all: Vec<HWND> = Vec::new();
    unsafe { EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize)).ok() };
    let mut out = Vec::new();
    for h in all {
        if h == skip {
            continue;
        }
        unsafe {
            if !IsWindowVisible(h).as_bool() {
                continue;
            }
            if GetWindow(h, GW_OWNER).is_ok_and(|o| !o.0.is_null()) {
                continue;
            }
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TOOLWINDOW.0 != 0 {
                continue;
            }
            let mut cloaked = 0u32;
            DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4).ok();
            if cloaked != 0 {
                continue;
            }
            let len = GetWindowTextLengthW(h);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(h, &mut buf);
            let title = String::from_utf16_lossy(&buf[..n as usize]);
            if title == "Program Manager" {
                continue;
            }
            out.push(WindowInfo { hwnd: h.0 as isize, title, exe: process_path(h) });
        }
    }
    out
}

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");

pub fn set_autostart(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE | KEY_QUERY_VALUE, &mut key).is_err() {
            return;
        }
        if on {
            if let Ok(exe) = std::env::current_exe() {
                let v: Vec<u16> = format!("\"{}\" --background", exe.display()).encode_utf16().chain(Some(0)).collect();
                let bytes = std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2);
                RegSetValueExW(key, w!("Heimdall"), None, REG_SZ, Some(bytes)).ok().ok();
            }
        } else {
            RegDeleteValueW(key, w!("Heimdall")).ok().ok();
        }
        RegCloseKey(key).ok().ok();
    }
}

pub fn file_name(path: &str) -> &str {
    path.trim_end_matches('\\').rsplit('\\').next().unwrap_or(path)
}

pub fn parent_dir(path: &str) -> &str {
    let p = path.trim_end_matches('\\');
    match p.rfind('\\') {
        Some(i) if i > 2 => &p[..i],
        Some(i) => &p[..=i],
        None => p,
    }
}

/// Replace the user profile prefix with `~` for compact subtitles.
pub fn pretty_path(path: &str) -> String {
    if let Some(home) = std::env::var_os("USERPROFILE").and_then(|h| h.into_string().ok()) {
        if path.get(..home.len()).is_some_and(|p| p.eq_ignore_ascii_case(&home)) {
            return format!("~{}", &path[home.len()..]);
        }
    }
    path.to_string()
}

pub fn hstring(s: &str) -> HSTRING {
    HSTRING::from(s)
}

/// Local time as "2026-10-30 14:05", for notes.
pub fn local_timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{}-{:02}-{:02} {:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute)
}
