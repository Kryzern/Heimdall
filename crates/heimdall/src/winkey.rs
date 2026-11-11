//! The Windows key on its own as the hotkey. RegisterHotKey can't take a bare
//! modifier, so a low-level keyboard hook watches for Win going down and up
//! with no other key in between. Before letting that key-up through it slips
//! in an unassigned key, which is how Windows tells Win+something from a tap,
//! so the Start menu stays closed while every Win+key shortcut keeps working.
//!
//! The same hook also spots Ctrl pressed twice in a row on its own, the other
//! hotkey a bare modifier can't be registered for. Ctrl is never swallowed.
//!
//! The hook lives on its own thread. Windows gives a low-level hook a few
//! hundred milliseconds to answer and then lets the key through without it,
//! so it can't share a thread with drawing, searching or loading icons.
//! The hook is only installed while it's needed.

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Posted to the window when a gesture happened: WPARAM is [`WIN`] for a tap
/// of the Windows key, [`CTRL`] for Ctrl pressed twice.
pub const WM_APP_WINKEY: u32 = WM_APP + 4;

/// Gestures the hook watches for, as bit flags.
pub const WIN: u32 = 1;
pub const CTRL: u32 = 2;

/// Each Ctrl press must be shorter than this, in milliseconds...
const CTRL_HOLD: u32 = 300;
/// ...and the second must follow the first within this.
const CTRL_GAP: u32 = 400;

/// Reserved, unassigned virtual key: what AutoHotkey uses for the same job.
const MASK: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

static TARGET: AtomicIsize = AtomicIsize::new(0);
/// Thread id of the hook thread, or 0 when the hook is off.
static THREAD: AtomicU32 = AtomicU32::new(0);
/// Which gestures are wanted ([`WIN`], [`CTRL`]).
static MODE: AtomicU32 = AtomicU32::new(0);
static TAP: Mutex<Tap> = Mutex::new(Tap { held: 0, other: false });
static TWICE: Mutex<Twice> = Mutex::new(Twice { down_at: None, last_up: None });

/// Watch for the gestures in `mode` (0 removes the hook). Returns whether the
/// hook is installed.
pub fn set(hwnd: HWND, mode: u32) -> bool {
    TARGET.store(hwnd.0 as isize, Ordering::Relaxed);
    MODE.store(mode, Ordering::Relaxed);
    let cur = THREAD.load(Ordering::Relaxed);
    if mode == 0 {
        if cur != 0 {
            unsafe { PostThreadMessageW(cur, WM_QUIT, WPARAM(0), LPARAM(0)).ok() };
            THREAD.store(0, Ordering::Relaxed);
        }
        if let Ok(mut t) = TAP.lock() {
            *t = Tap::default();
        }
        if let Ok(mut t) = TWICE.lock() {
            *t = Twice::default();
        }
        return false;
    }
    if cur != 0 {
        return true;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("winkey".into()).spawn(move || unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), Some(inst.into()), 0) {
            Ok(h) => h,
            Err(e) => {
                crate::log(format!("could not install the Windows key hook: {e}"));
                tx.send(0).ok();
                return;
            }
        };
        tx.send(GetCurrentThreadId()).ok();
        // Low-level hooks are called from this thread's message loop.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            DispatchMessageW(&msg);
        }
        UnhookWindowsHookEx(hook).ok();
    });
    let id = spawned.ok().and_then(|_| rx.recv().ok()).unwrap_or(0);
    THREAD.store(id, Ordering::Relaxed);
    id != 0
}

/// What the hook does with a physical key event.
#[derive(Debug, PartialEq)]
enum Act {
    Pass,
    /// Win was tapped alone: swallow this key-up, mask it, open Heimdall.
    Tapped,
}

/// Tracks one press of the Win key.
#[derive(Default)]
struct Tap {
    /// The Win key currently held, or 0.
    held: u32,
    /// Another key went down while Win was held: a shortcut, not a tap.
    other: bool,
}

impl Tap {
    /// `was_down` is whether Windows thought `vk` was already down before this
    /// event, which tells a fresh press from auto-repeat.
    fn key(&mut self, vk: u32, down: bool, was_down: bool) -> Act {
        let win = vk == VK_LWIN.0 as u32 || vk == VK_RWIN.0 as u32;
        if win && down {
            // A fresh press always starts over. Trusting `held` alone breaks
            // when a key-up never reached us (Win+L, UAC, an admin window,
            // a timed-out hook): the next tap was read as a shortcut and
            // opened the Start menu instead.
            if self.held != vk || !was_down {
                self.held = vk;
                self.other = false;
            }
        } else if down {
            if self.held != 0 {
                self.other = true;
            }
        } else if win && self.held == vk {
            self.held = 0;
            if !self.other {
                return Act::Tapped;
            }
        }
        Act::Pass
    }
}

/// Tracks Ctrl pressed twice on its own. Times are the hook's millisecond
/// timestamps.
#[derive(Default)]
struct Twice {
    /// When the Ctrl being held went down, if it's a fresh press.
    down_at: Option<u32>,
    /// When the last short, clean press of Ctrl ended.
    last_up: Option<u32>,
}

impl Twice {
    /// Returns true on the key-up that completes the second press.
    fn key(&mut self, vk: u32, down: bool, was_down: bool, time: u32) -> bool {
        let ctrl = matches!(vk, 0x11 | 0xA2 | 0xA3);
        if !ctrl {
            // Any other key (Ctrl+C, typing) breaks the sequence.
            if down {
                *self = Twice::default();
            }
            return false;
        }
        if down {
            if !was_down {
                self.down_at = Some(time);
            }
            return false;
        }
        let Some(at) = self.down_at.take() else { return false };
        if time.wrapping_sub(at) > CTRL_HOLD {
            // Held down: used with the mouse or just resting on it.
            self.last_up = None;
            return false;
        }
        if self.last_up.is_some_and(|up| at.wrapping_sub(up) <= CTRL_GAP) {
            self.last_up = None;
            return true;
        }
        self.last_up = Some(time);
        false
    }
}

unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        // Our own injected keys, and anything another tool sends, pass untouched.
        if k.flags.0 & LLKHF_INJECTED.0 == 0 {
            let down = matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            // Inside the hook the key state isn't updated for this event yet.
            let was_down = GetAsyncKeyState(k.vkCode as i32) as u16 & 0x8000 != 0;
            let mode = MODE.load(Ordering::Relaxed);
            let target = HWND(TARGET.load(Ordering::Relaxed) as *mut _);
            if mode & CTRL != 0 && TWICE.lock().is_ok_and(|mut t| t.key(k.vkCode, down, was_down, k.time)) {
                PostMessageW(Some(target), WM_APP_WINKEY, WPARAM(CTRL as usize), LPARAM(0)).ok();
            }
            let act = if mode & WIN != 0 { TAP.lock().map(|mut t| t.key(k.vkCode, down, was_down)).unwrap_or(Act::Pass) } else { Act::Pass };
            if act == Act::Tapped {
                // Swallow the real key-up and replay it after the mask key.
                let key = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: flags, ..Default::default() } },
                };
                let inputs = [
                    key(MASK, KEYBD_EVENT_FLAGS(0)),
                    key(MASK, KEYEVENTF_KEYUP),
                    key(VIRTUAL_KEY(k.vkCode as u16), KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY),
                ];
                SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
                PostMessageW(Some(target), WM_APP_WINKEY, WPARAM(WIN as usize), LPARAM(0)).ok();
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

#[cfg(test)]
mod tests {
    use super::*;
    const WIN: u32 = 0x5B;
    const RWIN: u32 = 0x5C;
    const L: u32 = 0x4C;

    #[test]
    fn tap_opens() {
        let mut t = Tap::default();
        assert_eq!(t.key(WIN, true, false), Act::Pass);
        assert_eq!(t.key(WIN, false, true), Act::Tapped);
    }

    #[test]
    fn auto_repeat_is_still_a_tap() {
        let mut t = Tap::default();
        t.key(WIN, true, false);
        t.key(WIN, true, true);
        t.key(WIN, true, true);
        assert_eq!(t.key(WIN, false, true), Act::Tapped);
    }

    #[test]
    fn shortcut_passes() {
        let mut t = Tap::default();
        t.key(WIN, true, false);
        t.key(0x45, true, false);
        t.key(0x45, false, true);
        assert_eq!(t.key(WIN, false, true), Act::Pass);
    }

    #[test]
    fn lost_key_up_does_not_eat_the_next_tap() {
        // Win+L: the key-up happens on the lock screen and never reaches us.
        let mut t = Tap::default();
        t.key(WIN, true, false);
        t.key(L, true, false);
        // Back at the desktop, a plain tap.
        assert_eq!(t.key(WIN, true, false), Act::Pass);
        assert_eq!(t.key(WIN, false, true), Act::Tapped);
    }

    const CTRL_L: u32 = 0xA2;
    const CTRL_R: u32 = 0xA3;

    /// Press and release `vk` at `t`, held for `hold` ms. True if it fired.
    fn tap(t: &mut Twice, vk: u32, at: u32, hold: u32) -> bool {
        t.key(vk, true, false, at);
        t.key(vk, false, true, at + hold)
    }

    #[test]
    fn ctrl_twice_fires() {
        let mut t = Twice::default();
        assert!(!tap(&mut t, CTRL_L, 1000, 80));
        assert!(tap(&mut t, CTRL_L, 1250, 80));
        // A third press starts a new pair.
        assert!(!tap(&mut t, CTRL_L, 1500, 80));
    }

    #[test]
    fn either_ctrl_counts() {
        let mut t = Twice::default();
        tap(&mut t, CTRL_L, 0, 50);
        assert!(tap(&mut t, CTRL_R, 200, 50));
    }

    #[test]
    fn ctrl_too_slow_or_held() {
        let mut t = Twice::default();
        tap(&mut t, CTRL_L, 0, 80);
        assert!(!tap(&mut t, CTRL_L, 900, 80));
        let mut t = Twice::default();
        tap(&mut t, CTRL_L, 0, 600);
        assert!(!tap(&mut t, CTRL_L, 700, 80));
    }

    #[test]
    fn ctrl_shortcut_breaks_it() {
        // Ctrl, then Ctrl+C: the second press is a shortcut.
        let mut t = Twice::default();
        tap(&mut t, CTRL_L, 0, 80);
        t.key(CTRL_L, true, false, 200);
        t.key(0x43, true, false, 230);
        t.key(0x43, false, true, 260);
        assert!(!t.key(CTRL_L, false, true, 280));
    }

    #[test]
    fn ctrl_auto_repeat_keeps_the_press() {
        let mut t = Twice::default();
        tap(&mut t, CTRL_L, 0, 80);
        t.key(CTRL_L, true, false, 200);
        t.key(CTRL_L, true, true, 230);
        assert!(t.key(CTRL_L, false, true, 260));
    }

    #[test]
    fn other_win_key_starts_over() {
        let mut t = Tap::default();
        t.key(WIN, true, false);
        t.key(L, true, false);
        t.key(RWIN, true, false);
        assert_eq!(t.key(RWIN, false, true), Act::Tapped);
    }
}
