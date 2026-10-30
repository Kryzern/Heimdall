//! Doing things on macOS: opening, revealing in Finder, the clipboard, power
//! actions, running apps, the screen under the mouse and login items.

use heimdall_common::items::SysCmd;
use objc2::rc::Retained;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::*;
use objc2_foundation::*;
use std::process::Command;

/// Open a file, folder or app, or a URL like `x-apple.systempreferences:`.
pub fn open(path: &str) -> bool {
    if !path.starts_with('/') && path.contains(':') {
        return open_url(path);
    }
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    NSWorkspace::sharedWorkspace().openURL(&url)
}

pub fn open_url(s: &str) -> bool {
    match NSURL::URLWithString(&NSString::from_str(s)) {
        Some(url) => NSWorkspace::sharedWorkspace().openURL(&url),
        None => false,
    }
}

/// Open Finder with the item selected.
pub fn reveal(path: &str) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
}

pub fn copy_text(s: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    unsafe { pb.setString_forType(&NSString::from_str(s), NSPasteboardTypeString) };
}

pub fn clipboard_text() -> Option<String> {
    let pb = NSPasteboard::generalPasteboard();
    unsafe { pb.stringForType(NSPasteboardTypeString) }.map(|s| s.to_string())
}

/// Bring a running app forward by process id.
pub fn activate(pid: i32) {
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        #[allow(deprecated)]
        app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    }
}

/// Regular apps that are running, frontmost first is not available, so in
/// launch order: `(name, bundle path, pid)`. Heimdall itself is left out.
pub fn running_apps() -> Vec<(String, String, i32)> {
    let ws = NSWorkspace::sharedWorkspace();
    let apps: Retained<NSArray<NSRunningApplication>> = unsafe { msg_send![&*ws, runningApplications] };
    let me = std::process::id() as i32;
    apps.iter()
        .filter(|a| a.activationPolicy() == NSApplicationActivationPolicy::Regular && a.processIdentifier() != me)
        .filter_map(|a| {
            let name = a.localizedName()?.to_string();
            let path = a.bundleURL().and_then(|u| u.path()).map(|p| p.to_string()).unwrap_or_default();
            Some((name, path, a.processIdentifier()))
        })
        .collect()
}

pub fn system(cmd: SysCmd) {
    let osa = |script: &str| {
        Command::new("/usr/bin/osascript").args(["-e", script]).spawn().ok();
    };
    match cmd {
        SysCmd::Lock => lock_screen(),
        SysCmd::Sleep => {
            Command::new("/usr/bin/pmset").arg("sleepnow").spawn().ok();
        }
        SysCmd::Restart => osa("tell application \"System Events\" to restart"),
        SysCmd::Shutdown => osa("tell application \"System Events\" to shut down"),
        SysCmd::SignOut => osa("tell application \"System Events\" to log out"),
        SysCmd::EmptyBin => osa("tell application \"Finder\" to empty trash"),
        SysCmd::Settings => {
            open("/System/Applications/System Settings.app");
        }
    }
}

/// The same call the Lock Screen menu item makes; the framework is private
/// but has been stable for years. Falls back to sleeping the display.
fn lock_screen() {
    unsafe {
        let lib = libc::dlopen(c"/System/Library/PrivateFrameworks/login.framework/Versions/Current/login".as_ptr(), libc::RTLD_LAZY);
        if !lib.is_null() {
            let f = libc::dlsym(lib, c"SACLockScreenImmediate".as_ptr());
            if !f.is_null() {
                let lock: extern "C" fn() -> i32 = std::mem::transmute(f);
                lock();
                return;
            }
        }
    }
    Command::new("/usr/bin/pmset").arg("displaysleepnow").spawn().ok();
}

/// Local time as "2026-10-30 14:05", for notes.
pub fn local_timestamp() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
    }
}

/// The `.app` bundle Heimdall runs from, if it runs from one.
pub fn bundle_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let s = exe.to_string_lossy();
    let i = s.find(".app/")?;
    Some(s[..i + 4].to_string())
}

/// Visible frame (no menu bar or Dock) of the screen under the mouse, in
/// Cocoa coordinates.
pub fn active_screen(mtm: MainThreadMarker) -> NSRect {
    let mouse = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);
    for s in screens.iter() {
        let f = s.frame();
        if mouse.x >= f.origin.x && mouse.x < f.origin.x + f.size.width && mouse.y >= f.origin.y && mouse.y < f.origin.y + f.size.height {
            return s.visibleFrame();
        }
    }
    NSScreen::mainScreen(mtm).map(|s| s.visibleFrame()).unwrap_or(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0)))
}

/// Whether views in `window` are drawn dark.
pub fn is_dark(window: &NSWindow) -> bool {
    let name = window.effectiveAppearance().name();
    name.to_string().contains("Dark")
}

/// The user's accent colour as sRGB.
pub fn accent() -> (f64, f64, f64) {
    let c = NSColor::controlAccentColor();
    match c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()) {
        Some(c) => (c.redComponent(), c.greenComponent(), c.blueComponent()),
        None => (0.0, 0.48, 1.0),
    }
}

/// Force the panel's appearance: "dark", "light", or follow the system.
pub fn set_theme(window: &NSWindow, theme: &str) {
    let name = match theme {
        "dark" => Some(unsafe { NSAppearanceNameDarkAqua }),
        "light" => Some(unsafe { NSAppearanceNameAqua }),
        _ => None,
    };
    let appearance = name.and_then(NSAppearance::appearanceNamed);
    window.setAppearance(appearance.as_deref());
}

const AGENT: &str = "com.kryzern.heimdall";

/// Open at login, through a per-user LaunchAgent. It works for an app that
/// isn't notarised, and shows up under Login Items like any other.
pub fn set_autostart(on: bool) {
    let path = heimdall_common::home().join(format!("Library/LaunchAgents/{AGENT}.plist"));
    if !on {
        std::fs::remove_file(path).ok();
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.to_string_lossy();
    // Opened straight from a download, macOS runs a copy from a random path
    // that's gone after a restart. Wait until it's been moved somewhere real.
    if exe.contains("/AppTranslocation/") {
        return;
    }
    let exe = exe.replace('&', "&amp;").replace('<', "&lt;");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{AGENT}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>--background</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if std::fs::read_to_string(&path).ok().as_deref() != Some(plist.as_str()) {
        std::fs::write(path, plist).ok();
    }
}

/// Size of `text` drawn with the system font.
pub fn text_width(text: &str, size: f64, weight: f64) -> f64 {
    let s = super::draw::attributed(text, size, weight, (0.0, 0.0, 0.0, 1.0), &[], (0.0, 0.0, 0.0, 1.0), false);
    let plain: &NSAttributedString = &s;
    plain.size().width
}

