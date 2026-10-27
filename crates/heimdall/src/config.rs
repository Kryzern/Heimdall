//! `%APPDATA%\Heimdall\config.toml`, reloaded whenever it changes on disk.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Global hotkey, e.g. "Alt+Space", "F5", "Ctrl+Shift+K".
    pub hotkey: String,
    /// Executables (e.g. "chrome.exe") in which the hotkey is released so the
    /// key keeps its normal meaning there.
    pub passthrough: Vec<String>,
    /// Release the hotkey while a fullscreen app or game has focus.
    pub game_mode: bool,
    /// "system", "dark" or "light".
    pub theme: String,
    /// "system" or a hex colour such as "#7C9CFF".
    pub accent: String,
    /// "liquid" (always glass), "auto" (glass when Windows transparency is on) or "solid".
    pub glass: String,
    /// Shadow under the panel: "normal", "soft" or "off".
    pub shadow: String,
    /// Corner shape: "round", "windows" (Windows 11's smaller radius) or "square".
    pub corners: String,
    /// Animate opening, resizing and moving the selection.
    pub animations: bool,
    /// Width of the search panel in DIPs.
    pub width: f32,
    pub max_results: usize,
    /// Show recent items when the search box is empty.
    pub show_recent: bool,
    pub start_with_windows: bool,
    /// Show an icon in the notification area.
    pub tray: bool,
    pub web: Vec<WebSearch>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebSearch {
    pub key: String,
    pub name: String,
    /// `{q}` is replaced with the URL-encoded query.
    pub url: String,
}

impl Default for Config {
    fn default() -> Self {
        let web = |key: &str, name: &str, url: &str| WebSearch { key: key.into(), name: name.into(), url: url.into() };
        Config {
            hotkey: "Alt+Space".into(),
            passthrough: Vec::new(),
            game_mode: true,
            theme: "system".into(),
            accent: "system".into(),
            glass: "liquid".into(),
            shadow: "normal".into(),
            corners: "round".into(),
            animations: true,
            width: 680.0,
            max_results: 8,
            show_recent: true,
            start_with_windows: true,
            tray: true,
            web: vec![
                web("g", "Google", "https://www.google.com/search?q={q}"),
                web("ddg", "DuckDuckGo", "https://duckduckgo.com/?q={q}"),
                web("yt", "YouTube", "https://www.youtube.com/results?search_query={q}"),
                web("gh", "GitHub", "https://github.com/search?q={q}"),
                web("wiki", "Wikipedia", "https://en.wikipedia.org/w/index.php?search={q}"),
            ],
        }
    }
}

const HEADER: &str = "\
# Heimdall configuration. Saved changes apply the next time Heimdall opens.
# Most of these can also be changed from Heimdall itself: type `settings`.
#
# hotkey       e.g. \"Alt+Space\", \"F5\", \"Ctrl+Shift+K\"
# passthrough  apps where the hotkey keeps its normal meaning, e.g. [\"chrome.exe\"]
# game_mode    release the hotkey while a fullscreen game or video has focus
# theme        \"system\" | \"dark\" | \"light\"
# accent       \"system\" or \"#RRGGBB\"
# glass        \"liquid\" | \"auto\" (follow Windows transparency) | \"solid\"
# shadow       \"normal\" | \"soft\" | \"off\"
# corners      \"round\" | \"windows\" (like Windows 11) | \"square\"
# animations   false makes everything appear instantly
# tray         show an icon in the notification area
# [[web]]      prefix searches: type `g rust lifetimes`

";

pub fn dir() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Heimdall")
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

pub struct Loader {
    stamp: Option<SystemTime>,
}

impl Loader {
    pub fn new() -> Self {
        Loader { stamp: None }
    }

    /// Returns a config when the file is new or changed since the last call.
    pub fn poll(&mut self) -> Option<Config> {
        let p = path();
        let stamp = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        if stamp.is_some() && stamp == self.stamp {
            return None;
        }
        let cfg = match std::fs::read_to_string(&p) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => c,
                Err(e) => {
                    crate::log(format!("config.toml: {e}"));
                    Config::default()
                }
            },
            Err(_) => {
                let c = Config::default();
                save(&c);
                c
            }
        };
        self.stamp = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        Some(cfg)
    }
}

pub fn save(c: &Config) {
    std::fs::create_dir_all(dir()).ok();
    if let Ok(body) = toml::to_string_pretty(c) {
        std::fs::write(path(), format!("{HEADER}{body}")).ok();
    }
}

/// Parse "Ctrl+Shift+K" into (MOD_* flags, virtual-key code).
pub fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    const MOD_ALT: u32 = 1;
    const MOD_CONTROL: u32 = 2;
    const MOD_SHIFT: u32 = 4;
    const MOD_WIN: u32 = 8;
    let mut mods = 0;
    let mut vk = None;
    for part in s.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "alt" => mods |= MOD_ALT,
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "win" | "super" => mods |= MOD_WIN,
            key => vk = key_code(key),
        }
    }
    vk.map(|v| (mods, v))
}

/// The reverse of `parse_hotkey`, for a key combination the user just pressed.
/// Returns None for keys that would make a poor global hotkey: a letter, digit
/// or Space on its own would stop working everywhere else.
pub fn hotkey_name(mods: u32, vk: u32) -> Option<String> {
    let key = match vk {
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x41..=0x5A | 0x30..=0x39 => (vk as u8 as char).to_string(),
        _ => KEYS.iter().find(|(_, v)| *v == vk)?.0.to_string(),
    };
    let plain_ok = matches!(vk, 0x70..=0x87 | 0x13 | 0x91 | 0x2D);
    if mods == 0 && !plain_ok {
        return None;
    }
    let mut out = String::new();
    for (bit, name) in [(2, "Ctrl"), (1, "Alt"), (4, "Shift"), (8, "Win")] {
        if mods & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&key);
    Some(out)
}

/// Named keys, spelled the way `hotkey_name` writes them.
const KEYS: &[(&str, u32)] = &[
    ("Space", 0x20),
    ("Enter", 0x0D),
    ("Tab", 0x09),
    ("`", 0xC0),
    (";", 0xBA),
    ("/", 0xBF),
    ("\\", 0xDC),
    (".", 0xBE),
    (",", 0xBC),
    ("Insert", 0x2D),
    ("Home", 0x24),
    ("End", 0x23),
    ("Pause", 0x13),
    ("ScrollLock", 0x91),
];

fn key_code(k: &str) -> Option<u32> {
    if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then(|| 0x70 + n - 1);
    }
    Some(match k {
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "`" | "backtick" | "grave" => 0xC0,
        ";" => 0xBA,
        "/" => 0xBF,
        "\\" => 0xDC,
        "." => 0xBE,
        "," => 0xBC,
        "insert" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pause" => 0x13,
        "scrolllock" => 0x91,
        _ if k.len() == 1 => {
            let c = k.as_bytes()[0].to_ascii_uppercase();
            if c.is_ascii_alphanumeric() {
                c as u32
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys() {
        assert_eq!(parse_hotkey("F5"), Some((0, 0x74)));
        assert_eq!(parse_hotkey("Alt+Space"), Some((1, 0x20)));
        assert_eq!(parse_hotkey("Ctrl+Shift+K"), Some((6, 0x4B)));
        assert_eq!(parse_hotkey("Ctrl+"), None);
    }

    #[test]
    fn hotkey_names_roundtrip() {
        for s in ["Alt+Space", "F4", "Ctrl+Shift+K", "Win+`", "Ctrl+Alt+Home", "Pause"] {
            let (m, v) = parse_hotkey(s).unwrap();
            assert_eq!(hotkey_name(m, v).as_deref(), Some(s));
        }
        assert_eq!(hotkey_name(0, 0x4B), None);
        assert_eq!(hotkey_name(0, 0x20), None);
    }

    #[test]
    fn default_roundtrips() {
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.hotkey, "Alt+Space");
        assert_eq!(back.web.len(), 5);
    }
}
