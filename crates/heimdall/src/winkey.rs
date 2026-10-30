//! The Windows key on its own as the hotkey. RegisterHotKey can't take a bare
//! modifier, so a low-level keyboard hook watches for Win going down and up
//! with no other key in between. Before letting that key-up through it slips
//! in an unassigned key, which is how Windows tells Win+something from a tap,
//! so the Start menu stays closed while every Win+key shortcut keeps working.
//! The hook is only installed while it's needed.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Posted to the window when Win was tapped on its own.
pub const WM_APP_WINKEY: u32 = WM_APP + 4;

/// Reserved, unassigned virtual key: what AutoHotkey uses for the same job.
const MASK: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

static HOOK: AtomicIsize = AtomicIsize::new(0);
static TARGET: AtomicIsize = AtomicIsize::new(0);
/// The Win key currently held, or 0.
static HELD: AtomicU32 = AtomicU32::new(0);
/// Another key went down while Win was held: a shortcut, not a tap.
static OTHER: AtomicBool = AtomicBool::new(false);

/// Install or remove the hook. Returns whether it is installed.
pub fn set(hwnd: HWND, on: bool) -> bool {
    let cur = HOOK.load(Ordering::Relaxed);
    if on {
        TARGET.store(hwnd.0 as isize, Ordering::Relaxed);
        if cur != 0 {
            return true;
        }
        let inst = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
        match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), Some(inst.into()), 0) } {
            Ok(h) => {
                HOOK.store(h.0 as isize, Ordering::Relaxed);
                true
            }
            Err(e) => {
                crate::log(format!("could not install the Windows key hook: {e}"));
                false
            }
        }
    } else {
        if cur != 0 {
            unsafe { UnhookWindowsHookEx(HHOOK(cur as *mut _)).ok() };
            HOOK.store(0, Ordering::Relaxed);
        }
        HELD.store(0, Ordering::Relaxed);
        false
    }
}

unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        // Our own injected keys, and anything another tool sends, pass untouched.
        if k.flags.0 & LLKHF_INJECTED.0 == 0 {
            let vk = k.vkCode;
            let win = vk == VK_LWIN.0 as u32 || vk == VK_RWIN.0 as u32;
            let down = matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let held = HELD.load(Ordering::Relaxed);
            if win && down {
                // Auto-repeat sends more key-downs; only the first one starts a tap.
                if held == 0 {
                    HELD.store(vk, Ordering::Relaxed);
                    OTHER.store(false, Ordering::Relaxed);
                }
            } else if down {
                if held != 0 {
                    OTHER.store(true, Ordering::Relaxed);
                }
            } else if win && held == vk {
                HELD.store(0, Ordering::Relaxed);
                if !OTHER.load(Ordering::Relaxed) {
                    // Swallow the real key-up and replay it after the mask key.
                    let key = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: flags, ..Default::default() } },
                    };
                    let inputs = [
                        key(MASK, KEYBD_EVENT_FLAGS(0)),
                        key(MASK, KEYEVENTF_KEYUP),
                        key(VIRTUAL_KEY(vk as u16), KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY),
                    ];
                    SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
                    let target = HWND(TARGET.load(Ordering::Relaxed) as *mut _);
                    PostMessageW(Some(target), WM_APP_WINKEY, WPARAM(0), LPARAM(0)).ok();
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}
