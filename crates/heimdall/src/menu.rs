//! Native popup menus (right-click on a result, the tray icon) and the tray
//! icon itself.

use windows::core::{w, HSTRING, PCSTR, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_TRAY: u32 = WM_APP + 3;
const TRAY_ID: u32 = 1;

pub struct Menu(HMENU);

impl Menu {
    pub fn new() -> Option<Menu> {
        unsafe { CreatePopupMenu().ok().map(Menu) }
    }

    pub fn item(&self, id: u32, text: &str) {
        self.add(MF_STRING, id, text);
    }

    pub fn check(&self, id: u32, text: &str, checked: bool) {
        self.add(if checked { MF_STRING | MF_CHECKED } else { MF_STRING }, id, text);
    }

    pub fn sep(&self) {
        unsafe { AppendMenuW(self.0, MF_SEPARATOR, 0, PCWSTR::null()).ok() };
    }

    fn add(&self, flags: MENU_ITEM_FLAGS, id: u32, text: &str) {
        unsafe { AppendMenuW(self.0, flags, id as usize, &HSTRING::from(text)).ok() };
    }

    /// Show at a screen point and wait. Returns the chosen id, or 0.
    pub fn show(&self, hwnd: HWND, x: i32, y: i32, dark: bool) -> u32 {
        menu_theme(dark);
        unsafe {
            // Without this a tray menu stays open after clicking elsewhere.
            SetForegroundWindow(hwnd).ok().ok();
            let r = TrackPopupMenuEx(self.0, (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY).0, x, y, hwnd, None);
            PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0)).ok();
            r.0 as u32
        }
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        unsafe { DestroyMenu(self.0).ok() };
    }
}

/// Match popup menus to Heimdall's theme. Windows only exposes this through
/// two unnamed uxtheme exports (135 and 136), which Explorer, Notepad++ and
/// Everything all use; on builds without them the menu simply stays light.
fn menu_theme(dark: bool) {
    type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
    type FlushMenuThemes = unsafe extern "system" fn();
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        let (Some(set), Some(flush)) = (GetProcAddress(ux, PCSTR(135 as *const u8)), GetProcAddress(ux, PCSTR(136 as *const u8))) else {
            return;
        };
        let set: SetPreferredAppMode = std::mem::transmute(set);
        let flush: FlushMenuThemes = std::mem::transmute(flush);
        // 2 = force dark, 3 = force light.
        set(if dark { 2 } else { 3 });
        flush();
    }
}

pub struct Tray {
    hwnd: HWND,
    shown: bool,
    icon: HICON,
    /// Explorer broadcasts this after it restarts; the icon must be re-added.
    pub taskbar_created: u32,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        unsafe {
            let inst = GetModuleHandleW(None).unwrap_or_default();
            let size = GetSystemMetrics(SM_CXSMICON);
            let icon = LoadImageW(Some(inst.into()), PCWSTR(1 as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
                .map(|h| HICON(h.0))
                .unwrap_or_default();
            Tray { hwnd, shown: false, icon, taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")) }
        }
    }

    /// Show, update or remove the icon.
    pub fn set(&mut self, on: bool, tip: &str) {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        for (d, c) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *d = c;
        }
        unsafe {
            if !on {
                if self.shown {
                    Shell_NotifyIconW(NIM_DELETE, &nid).ok().ok();
                }
                self.shown = false;
            } else if !(self.shown && Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool()) {
                self.shown = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
            }
        }
    }

    /// Explorer restarted: the old icon is gone.
    pub fn readd(&mut self, on: bool, tip: &str) {
        self.shown = false;
        self.set(on, tip);
    }
}

/// "Open with..." for a file.
pub fn open_with(path: &str) {
    let file = HSTRING::from(path);
    let info = OPENASINFO { pcszFile: PCWSTR(file.as_ptr()), pcszClass: PCWSTR::null(), oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_EXEC };
    unsafe { SHOpenWithDialog(None, &info).ok() };
}

/// The Explorer properties dialog for a file or folder.
pub fn properties(path: &str) {
    unsafe { SHObjectProperties(None, SHOP_FILEPATH, &HSTRING::from(path), PCWSTR::null()).ok().ok() };
}
