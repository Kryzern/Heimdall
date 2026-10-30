//! The global hotkey, through Carbon's `RegisterEventHotKey`. It's old, but
//! it's still what every launcher uses: it needs no Accessibility or Input
//! Monitoring permission, and the key press never reaches other apps.
//!
//! Config names follow the Windows spelling so one config reads the same on
//! both: `Alt` is Option, `Win`/`Cmd` is Command, e.g. "Alt+Space".

use std::ffi::c_void;
use std::ptr::null_mut;

#[repr(C)]
struct EventTypeSpec {
    class: u32,
    kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

type Handler = extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: Handler,
        count: u32,
        list: *const EventTypeSpec,
        user: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32;
    fn RegisterEventHotKey(code: u32, mods: u32, id: EventHotKeyId, target: *mut c_void, options: u32, out: *mut *mut c_void) -> i32;
    fn UnregisterEventHotKey(hotkey: *mut c_void) -> i32;
}

const KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const HOTKEY_PRESSED: u32 = 5;

// Carbon modifier bits.
const CMD: u32 = 0x0100;
const SHIFT: u32 = 0x0200;
const OPTION: u32 = 0x0800;
const CONTROL: u32 = 0x1000;

thread_local! {
    static CURRENT: std::cell::Cell<*mut c_void> = const { std::cell::Cell::new(null_mut()) };
    static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

extern "C" fn on_hotkey(_call: *mut c_void, _event: *mut c_void, _user: *mut c_void) -> i32 {
    super::with_app(|a| a.toggle());
    0
}

/// Register `name` (e.g. "Alt+Space"), replacing any previous hotkey.
/// Returns false when it can't be parsed or another app already owns it.
pub fn register(name: &str) -> bool {
    unregister();
    let Some((mods, code)) = parse(name) else { return false };
    unsafe {
        if !INSTALLED.get() {
            let spec = EventTypeSpec { class: KEYBOARD, kind: HOTKEY_PRESSED };
            InstallEventHandler(GetApplicationEventTarget(), on_hotkey, 1, &spec, null_mut(), null_mut());
            INSTALLED.set(true);
        }
        let mut out = null_mut();
        let id = EventHotKeyId { signature: u32::from_be_bytes(*b"hmdl"), id: 1 };
        let ok = RegisterEventHotKey(code, mods, id, GetApplicationEventTarget(), 0, &mut out) == 0;
        if ok {
            CURRENT.set(out);
        }
        ok
    }
}

pub fn unregister() {
    let cur = CURRENT.replace(null_mut());
    if !cur.is_null() {
        unsafe { UnregisterEventHotKey(cur) };
    }
}

/// "Ctrl+Alt+K" -> (Carbon modifiers, virtual key code).
pub fn parse(s: &str) -> Option<(u32, u32)> {
    let mut mods = 0;
    let mut code = None;
    for part in s.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "alt" | "option" | "opt" => mods |= OPTION,
            "ctrl" | "control" => mods |= CONTROL,
            "shift" => mods |= SHIFT,
            "cmd" | "command" | "win" | "super" => mods |= CMD,
            key => code = KEYS.iter().find(|(n, _)| n.eq_ignore_ascii_case(key)).map(|(_, c)| *c as u32),
        }
    }
    code.map(|c| (mods, c))
}

/// The reverse, for a key pressed while recording a new hotkey. `flags` are
/// NSEvent modifier flags. None for keys that would make a poor hotkey: a
/// letter, digit or Space on its own would stop working everywhere else.
pub fn name(flags: usize, code: u16) -> Option<String> {
    let key = KEYS.iter().find(|(_, c)| *c == code)?.0;
    let mut out = String::new();
    for (bit, n) in [(1 << 18, "Ctrl"), (1 << 19, "Alt"), (1 << 17, "Shift"), (1 << 20, "Cmd")] {
        if flags & bit != 0 {
            out.push_str(n);
            out.push('+');
        }
    }
    let plain_ok = key.starts_with('F') && key.len() > 1;
    if out.is_empty() && !plain_ok {
        return None;
    }
    out.push_str(key);
    Some(out)
}

/// Key names and macOS virtual key codes (ANSI layout positions).
const KEYS: &[(&str, u16)] = &[
    ("A", 0), ("S", 1), ("D", 2), ("F", 3), ("H", 4), ("G", 5), ("Z", 6), ("X", 7), ("C", 8), ("V", 9),
    ("B", 11), ("Q", 12), ("W", 13), ("E", 14), ("R", 15), ("Y", 16), ("T", 17), ("1", 18), ("2", 19),
    ("3", 20), ("4", 21), ("6", 22), ("5", 23), ("=", 24), ("9", 25), ("7", 26), ("-", 27), ("8", 28),
    ("0", 29), ("]", 30), ("O", 31), ("U", 32), ("[", 33), ("I", 34), ("P", 35), ("Enter", 36), ("L", 37),
    ("J", 38), ("'", 39), ("K", 40), (";", 41), ("\\", 42), (",", 43), ("/", 44), ("N", 45), ("M", 46),
    (".", 47), ("Tab", 48), ("Space", 49), ("`", 50), ("F17", 64), ("F18", 79), ("F19", 80), ("F20", 90),
    ("F5", 96), ("F6", 97), ("F7", 98), ("F3", 99), ("F8", 100), ("F9", 101), ("F11", 103), ("F13", 105),
    ("F16", 106), ("F14", 107), ("F10", 109), ("F12", 111), ("F15", 113), ("Home", 115), ("PageUp", 116),
    ("F4", 118), ("End", 119), ("F2", 120), ("PageDown", 121), ("F1", 122),
];

/// Modifier keys on their own, which recording ignores.
pub fn is_modifier(code: u16) -> bool {
    matches!(code, 54..=63)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(parse("Alt+Space"), Some((OPTION, 49)));
        assert_eq!(parse("Cmd+Shift+K"), Some((CMD | SHIFT, 40)));
        assert_eq!(parse("F4"), Some((0, 118)));
        assert_eq!(name(1 << 19, 49).as_deref(), Some("Alt+Space"));
        assert_eq!(name(0, 118).as_deref(), Some("F4"));
        assert_eq!(name(0, 49), None);
        for s in ["Alt+Space", "Ctrl+Alt+K", "Cmd+`", "F5"] {
            let (m, c) = parse(s).unwrap();
            let flags = [(OPTION, 1 << 19), (CONTROL, 1 << 18), (SHIFT, 1 << 17), (CMD, 1 << 20)]
                .iter()
                .filter(|(b, _)| m & b != 0)
                .map(|(_, f)| f)
                .sum::<usize>();
            assert_eq!(name(flags, c as u16).as_deref(), Some(s));
        }
    }
}
