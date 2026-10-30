//! Result rows on macOS: apps, System Settings panes, system commands, the
//! calculator, Base64, notes, web searches, running apps and path browsing.
//! The same rules and scores as the Windows version, with Mac targets.

use super::apps::App;
use super::files::from_index_path;
use heimdall_common::config::Config;
use heimdall_common::items::Setting as S;
use heimdall_common::items::*;
use heimdall_core::{b64, calc, Results};

/// SF Symbols used for row icons. `Icon::Glyph` holds an index into this
/// list (offset into a private-use range), so only ever append to it: pins
/// remember the glyph.
const SYMBOLS: &[&str] = &[
    "magnifyingglass",
    "equal.square",
    "chevron.left.forwardslash.chevron.right",
    "globe",
    "lock",
    "moon.zzz",
    "arrow.clockwise",
    "power",
    "rectangle.portrait.and.arrow.right",
    "trash",
    "gearshape",
    "pencil",
    "keyboard",
    "arrow.up.forward.app",
    "menubar.rectangle",
    "circle.lefthalf.filled",
    "sparkles",
    "clock",
    "lightbulb",
    "line.3.horizontal.decrease",
    "note.text",
    "xmark.circle",
    "wifi",
    "antenna.radiowaves.left.and.right",
    "network",
    "display",
    "speaker.wave.2",
    "bell",
    "moon",
    "battery.100",
    "photo",
    "dock.rectangle",
    "switch.2",
    "computermouse",
    "rectangle.and.hand.point.up.left",
    "hand.raised",
    "touchid",
    "person.2",
    "info.circle",
    "arrow.triangle.2.circlepath",
    "internaldrive",
    "printer",
    "accessibility",
    "square.and.arrow.up",
    "clock.arrow.circlepath",
    "arrow.down.circle",
    "folder",
];

const SYM_BASE: u32 = 0xF0000;

/// The row icon for an SF Symbol name.
pub fn sym(name: &str) -> Icon {
    let i = SYMBOLS.iter().position(|s| *s == name).expect("symbol listed in SYMBOLS");
    Icon::Glyph(char::from_u32(SYM_BASE + i as u32).unwrap())
}

/// The SF Symbol name behind an `Icon::Glyph`.
pub fn symbol_name(c: char) -> &'static str {
    SYMBOLS.get((c as u32).wrapping_sub(SYM_BASE) as usize).copied().unwrap_or("circle")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Empty query: pins and recent items.
    Recent,
    Normal,
    /// `>`: switch between running apps.
    Windows,
    /// A path like `/Users/et` or `~/Doc`: browse the filesystem.
    Path,
    /// `?`: tips and filters.
    Help,
    /// Heimdall's own settings page.
    Settings,
}

pub fn mode(q: &str) -> Mode {
    let t = q.trim_start();
    if t.is_empty() {
        Mode::Recent
    } else if t == "?" {
        Mode::Help
    } else if t.starts_with('>') {
        Mode::Windows
    } else if t.starts_with('/') || t.starts_with("~/") {
        Mode::Path
    } else {
        Mode::Normal
    }
}

pub struct Ctx<'a> {
    pub apps: &'a [App],
    pub cfg: &'a Config,
}

/// Rows computed instantly on the main thread, and whether the file index
/// should also be asked.
pub fn instant(q: &str, ctx: &Ctx, running: impl Fn() -> Vec<(String, String, i32)>) -> (Vec<Item>, bool) {
    let mut out = Vec::new();
    match mode(q) {
        Mode::Recent | Mode::Settings => return (out, false),
        Mode::Help => return (tips(), false),
        Mode::Windows => {
            switcher(q.trim_start()[1..].trim(), running(), &mut out);
            return (out, false);
        }
        Mode::Path => {
            paths(q.trim(), &mut out);
            return (out, false);
        }
        Mode::Normal => {}
    }
    let q = q.trim();

    if let Some((key, rest)) = q.split_once(' ') {
        let rest = rest.trim();
        // Base64: `b64 text` encodes, `b64d text` decodes.
        let decode = key.eq_ignore_ascii_case("b64d");
        if (decode || key.eq_ignore_ascii_case("b64")) && !rest.is_empty() {
            let it = match decode.then(|| b64::decode_text(rest)) {
                None => {
                    let s = b64::encode(rest.as_bytes());
                    Item::new(&s, "Base64 encoded  ·  Enter to copy", Kind::Base64, sym("chevron.left.forwardslash.chevron.right"), Action::Copy(s.clone()))
                }
                Some(Some(s)) => {
                    Item::new(&s, "Base64 decoded  ·  Enter to copy", Kind::Base64, sym("chevron.left.forwardslash.chevron.right"), Action::Copy(s.clone()))
                }
                Some(None) => Item::new("Not valid Base64 text", rest, Kind::Base64, sym("chevron.left.forwardslash.chevron.right"), Action::Insert(q.to_string())),
            };
            out.push(it);
            return (out, false);
        }
        // Quick note: `note call the dentist`.
        if key.eq_ignore_ascii_case("note") && !rest.is_empty() {
            let file = heimdall_common::notes::path(ctx.cfg);
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut it = Item::new(rest, format!("Add to {name}"), Kind::Note, sym("note.text"), Action::Note(rest.to_string()));
            it.score = 9500;
            out.push(it);
            return (out, false);
        }
        // Web prefix: `g rust lifetimes`.
        if let Some(w) = ctx.cfg.web.iter().find(|w| w.key.eq_ignore_ascii_case(key)) {
            if !rest.is_empty() {
                let url = w.url.replace("{q}", &url_encode(rest));
                let mut it = Item::new(format!("Search {} for \u{201C}{rest}\u{201D}", w.name), &url, Kind::Web, sym("globe"), Action::Url(url.clone()));
                it.score = 9500;
                out.push(it);
                return (out, false);
            }
        }
    }

    if let Some(v) = calc::eval(q) {
        let s = calc::format(v);
        let mut it = Item::new(&s, format!("{q} =  ·  Enter to copy"), Kind::Calc, sym("equal.square"), Action::Copy(s.clone()));
        it.score = 10_000;
        out.push(it);
    }

    for app in ctx.apps {
        if let Some((score, pos)) = fuzzy_words(q, &app.target) {
            let mut it = Item::new(&app.name, pretty_path(parent_dir(&app.path)), Kind::App, Icon::Shell(app.path.clone()), Action::Open(app.path.clone()))
                .with_key(format!("a:{}", app.path.to_lowercase()));
            it.score = score;
            it.hl = ranges16(&app.name, &pos);
            it.path = Some(app.path.clone());
            out.push(it);
        }
    }

    if q.chars().count() >= 2 {
        commands(q, &mut out);
    }

    if let Some(w) = ctx.cfg.web.first() {
        let mut it = Item::new(format!("Search {} for \u{201C}{q}\u{201D}", w.name), "", Kind::Web, sym("globe"), Action::Url(w.url.replace("{q}", &url_encode(q))));
        it.score = -100_000;
        out.push(it);
    }
    (out, q.chars().count() >= 2)
}

fn best_alias(q: &str, title: &str, aliases: &str) -> Option<i32> {
    std::iter::once(title).chain(aliases.split(' ')).filter_map(|a| fuzzy_words(q, &Target::new(a)).map(|(s, _)| s)).max()
}

fn commands(q: &str, out: &mut Vec<Item>) {
    let sys: &[(&str, &str, &str, SysCmd)] = &[
        ("Lock Screen", "lock computer screen", "lock", SysCmd::Lock),
        ("Sleep", "sleep suspend", "moon.zzz", SysCmd::Sleep),
        ("Restart", "restart reboot", "arrow.clockwise", SysCmd::Restart),
        ("Shut Down", "shutdown power off turn off", "power", SysCmd::Shutdown),
        ("Log Out", "log out logout sign out", "rectangle.portrait.and.arrow.right", SysCmd::SignOut),
        ("Empty Trash", "empty trash bin recycle", "trash", SysCmd::EmptyBin),
        ("System Settings", "settings preferences system", "gearshape", SysCmd::Settings),
    ];
    for &(title, aliases, glyph, cmd) in sys {
        if let Some(score) = best_alias(q, title, aliases).filter(|s| *s >= 450) {
            let mut it = Item::new(title, "System", Kind::System, sym(glyph), Action::System(cmd)).with_key(format!("s:{title}"));
            it.score = score - 40;
            it.confirm = matches!(cmd, SysCmd::Restart | SysCmd::Shutdown | SysCmd::SignOut);
            if let Some((_, pos)) = fuzzy_words(q, &Target::new(title)) {
                it.hl = ranges16(title, &pos);
            }
            out.push(it);
        }
    }

    settings_panes(q, out);

    let own: &[(&str, &str, Internal)] = &[
        ("Heimdall: Settings", "heimdall settings preferences hotkey shortcut theme", Internal::Settings),
        ("Heimdall: Open config file", "heimdall config toml edit", Internal::EditConfig),
        ("Heimdall: Rebuild file index", "heimdall rebuild reindex index", Internal::ReloadIndex),
        ("Heimdall: Quit", "heimdall quit exit close", Internal::Quit),
    ];
    for &(title, aliases, cmd) in own {
        if let Some(score) = best_alias(q, title, aliases).filter(|s| *s >= 450) {
            let mut it = Item::new(title, "", Kind::Command, heimdall_icon(), Action::Heimdall(cmd)).with_key(format!("h:{title}"));
            it.score = score - 60;
            out.push(it);
        }
    }
}

/// Heimdall's own icon, so its commands never look like System Settings.
fn heimdall_icon() -> Icon {
    match super::platform::bundle_path() {
        Some(p) => Icon::Shell(p),
        None => sym("gearshape"),
    }
}

/// System Settings panes: (title, pane id, keywords, symbol).
const PANES: &[(&str, &str, &str, &str)] = &[
    ("Wi-Fi", "com.apple.wifi-settings-extension", "wifi wireless network internet", "wifi"),
    ("Bluetooth", "com.apple.BluetoothSettings", "bluetooth devices pair headphones airpods", "antenna.radiowaves.left.and.right"),
    ("Network", "com.apple.Network-Settings.extension", "network internet ethernet ip dns vpn proxy", "network"),
    ("Displays", "com.apple.Displays-Settings.extension", "display screen resolution brightness night shift monitor scaling", "display"),
    ("Sound", "com.apple.Sound-Settings.extension", "audio volume speakers headphones microphone output input", "speaker.wave.2"),
    ("Notifications", "com.apple.Notifications-Settings.extension", "alerts banners badges", "bell"),
    ("Focus", "com.apple.Focus-Settings.extension", "do not disturb focus", "moon"),
    ("Battery", "com.apple.Battery-Settings.extension", "battery power energy low power mode", "battery.100"),
    ("Appearance", "com.apple.Appearance-Settings.extension", "dark mode light mode theme accent colour color", "circle.lefthalf.filled"),
    ("Wallpaper", "com.apple.Wallpaper-Settings.extension", "wallpaper desktop background", "photo"),
    ("Desktop & Dock", "com.apple.Desktop-Settings.extension", "dock desktop stage manager mission control hot corners", "dock.rectangle"),
    ("Control Center", "com.apple.ControlCenter-Settings.extension", "control center menu bar", "switch.2"),
    ("Lock Screen", "com.apple.Lock-Screen-Settings.extension", "lock screen saver password", "lock"),
    ("Keyboard", "com.apple.Keyboard-Settings.extension", "keyboard shortcuts key repeat input sources", "keyboard"),
    ("Trackpad", "com.apple.Trackpad-Settings.extension", "trackpad gestures tap click", "rectangle.and.hand.point.up.left"),
    ("Mouse", "com.apple.Mouse-Settings.extension", "mouse pointer scroll speed", "computermouse"),
    ("Privacy & Security", "com.apple.settings.PrivacySecurity.extension", "privacy security permissions full disk access filevault", "hand.raised"),
    ("Touch ID & Password", "com.apple.Touch-ID-Settings.extension", "touch id fingerprint password", "touchid"),
    ("Users & Groups", "com.apple.Users-Groups-Settings.extension", "users accounts groups", "person.2"),
    ("General", "com.apple.systempreferences.GeneralSettings", "general", "gearshape"),
    ("About", "com.apple.SystemProfiler.AboutExtension", "about mac specs version serial", "info.circle"),
    ("Software Update", "com.apple.Software-Update-Settings.extension", "update upgrade macos", "arrow.triangle.2.circlepath"),
    ("Storage", "com.apple.settings.Storage", "disk space storage free up", "internaldrive"),
    ("Login Items", "com.apple.LoginItems-Settings.extension", "login items startup open at login background", "arrow.up.forward.app"),
    ("Date & Time", "com.apple.Date-Time-Settings.extension", "date time clock timezone time zone", "clock"),
    ("Language & Region", "com.apple.Localization-Settings.extension", "language region locale", "globe"),
    ("Sharing", "com.apple.Sharing-Settings.extension", "sharing airdrop screen sharing file sharing remote", "square.and.arrow.up"),
    ("Printers & Scanners", "com.apple.Print-Scan-Settings.extension", "printer scanner print", "printer"),
    ("Accessibility", "com.apple.Accessibility-Settings.extension", "accessibility voiceover zoom", "accessibility"),
    ("Time Machine", "com.apple.Time-Machine-Settings.extension", "backup time machine restore", "clock.arrow.circlepath"),
];

/// Panes: a title match at substring strength, or every query word starting
/// a keyword, so `re` doesn't flood the list. Same rules as on Windows.
fn settings_panes(q: &str, out: &mut Vec<Item>) {
    let short = q.chars().count() < 3;
    let floor = if short { 600 } else { 450 };
    for &(title, id, keywords, glyph) in PANES {
        let t = fuzzy_words(q, &Target::new(title)).filter(|(s, _)| *s >= floor);
        let k = if short {
            None
        } else {
            let kw: Vec<Target> = keywords.split(' ').map(Target::new).collect();
            q.split_whitespace()
                .map(|w| kw.iter().filter_map(|t| fuzzy_words(w, t).map(|(s, _)| s)).max().filter(|s| *s >= 650))
                .collect::<Option<Vec<i32>>>()
                .filter(|v| !v.is_empty())
                .map(|v| v.iter().sum::<i32>() / v.len() as i32)
        };
        let score = match (&t, k) {
            (Some((a, _)), Some(b)) => (*a).max(b),
            (Some((a, _)), None) => *a,
            (None, Some(b)) => b - 30,
            (None, None) => continue,
        };
        let url = format!("x-apple.systempreferences:{id}");
        let mut it = Item::new(title, "System Settings", Kind::WinSetting, sym(glyph), Action::Open(url)).with_key(format!("w:{id}"));
        it.score = score - 50;
        if let Some((_, pos)) = t {
            it.hl = ranges16(title, &pos);
        }
        out.push(it);
    }
}

/// The `?` page.
fn tips() -> Vec<Item> {
    let rows: &[(&str, &str, &str, &str)] = &[
        ("ext:pdf", "Only these extensions, e.g. ext:pdf,docx", "line.3.horizontal.decrease", "ext:"),
        ("type:image", "Also video, audio, doc, code, archive, app, folder and file", "line.3.horizontal.decrease", "type:"),
        ("in:downloads", "Inside a folder with this name", "line.3.horizontal.decrease", "in:"),
        ("-in:node_modules", "Leave out a folder", "line.3.horizontal.decrease", "-in:"),
        ("size:>100mb", "Bigger than a size; size:<1mb for smaller", "line.3.horizontal.decrease", "size:>"),
        ("mod:week", "Changed recently: today, week, month, 3d, 12h", "line.3.horizontal.decrease", "mod:"),
        (">", "Switch between running apps", "lightbulb", ">"),
        ("~/", "Browse folders by path; Tab completes a folder", "lightbulb", "~/"),
        ("g rust lifetimes", "Search the web with a prefix: g, ddg, yt, gh, wiki", "globe", "g "),
        ("2 * (3 + 4)", "Calculator; Enter copies the answer", "equal.square", ""),
        ("b64 hello", "Base64 encode; b64d decodes. Enter copies it", "chevron.left.forwardslash.chevron.right", "b64 "),
        ("note buy milk", "Add a line to your notes file (notes in the config)", "note.text", "note "),
    ];
    let mut out: Vec<Item> = rows
        .iter()
        .map(|&(title, sub, glyph, insert)| Item::new(title, sub, Kind::Tip, sym(glyph), Action::Insert(insert.to_string())).with_key(format!("tip:{title}")))
        .collect();
    out.push(
        Item::new("Heimdall settings", "Change the hotkey, theme and more", Kind::Tip, heimdall_icon(), Action::Heimdall(Internal::Settings))
            .with_key("tip:settings"),
    );
    let n = out.len() as i32;
    for (i, it) in out.iter_mut().enumerate() {
        it.score = n - i as i32;
    }
    out
}

/// Heimdall's settings page, filtered by `q`.
pub fn settings(q: &str, cfg: &Config, recording: bool, notice: Option<&str>, has_recent: bool) -> Vec<Item> {
    let on = |b: bool| if b { "On" } else { "Off" }.to_string();
    let cap = |s: &str| {
        let mut c = s.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
    };
    let hotkey_sub = match (recording, notice) {
        (true, _) => "Press the new shortcut now. Esc cancels.".to_string(),
        (false, Some(n)) => n.to_string(),
        (false, None) => "The shortcut that opens Heimdall".to_string(),
    };
    let hotkey_value = if recording { "\u{2026}".to_string() } else { display_hotkey(&cfg.hotkey) };
    let rows: Vec<(&str, String, &str, Setting, String)> = vec![
        ("Hotkey", hotkey_sub, "keyboard", S::Hotkey, hotkey_value),
        ("Open at login", "Start in the background when you log in".into(), "arrow.up.forward.app", S::Autostart, on(cfg.start_with_windows)),
        ("Menu bar icon", "Show Heimdall in the menu bar".into(), "menubar.rectangle", S::Tray, on(cfg.tray)),
        ("Theme", "System, dark or light".into(), "circle.lefthalf.filled", S::Theme, cap(&cfg.theme)),
        ("Animations", "Turn off to make everything appear instantly".into(), "sparkles", S::Animations, on(cfg.animations)),
        ("Recent items", "Show recent files when the search box is empty".into(), "clock", S::Recent, on(cfg.show_recent)),
        ("Clear search on close", "Open with an empty search box every time".into(), "xmark.circle", S::ClearOnClose, on(cfg.clear_on_close)),
    ];
    let mut out = Vec::new();
    for (title, sub, glyph, setting, value) in rows {
        let hit = if q.trim().is_empty() { Some((0, Vec::new())) } else { fuzzy_words(q, &Target::new(title)) };
        let Some((_, pos)) = hit else { continue };
        let mut it = Item::new(title, sub, Kind::Setting, sym(glyph), Action::Heimdall(Internal::Set(setting))).with_key(format!("set:{title}"));
        it.hl = ranges16(title, &pos);
        it.value = value;
        out.push(it);
    }
    let clear = "Clear recent items";
    if q.trim().is_empty() || fuzzy_words(q, &Target::new(clear)).is_some() {
        let mut it = Item::new(clear, "Forget recently opened files and folders. Pinned items stay", Kind::Setting, sym("clock"), Action::Heimdall(Internal::ClearRecent))
            .with_key("set:clear");
        it.value = if has_recent { "Clear".into() } else { "Empty".into() };
        out.push(it);
    }
    let edit = "Open config file";
    if q.trim().is_empty() || fuzzy_words(q, &Target::new(edit)).is_some() {
        out.push(
            Item::new(edit, "Width, result count, web searches and more", Kind::Setting, sym("pencil"), Action::Heimdall(Internal::EditConfig))
                .with_key("set:config"),
        );
    }
    let n = out.len() as i32;
    for (i, it) in out.iter_mut().enumerate() {
        it.score = 1000 + n - i as i32;
    }
    out
}

/// "Alt+Space" -> "⌥ Space", the way Mac menus write shortcuts.
pub fn display_hotkey(s: &str) -> String {
    let mut mods = String::new();
    let mut key = "";
    for part in s.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.push('\u{2303}'),
            "alt" | "option" | "opt" => mods.push('\u{2325}'),
            "shift" => mods.push('\u{21E7}'),
            "cmd" | "command" | "win" | "super" => mods.push('\u{2318}'),
            _ => key = part,
        }
    }
    if mods.is_empty() {
        key.to_string()
    } else {
        format!("{mods} {key}")
    }
}

/// `>`: running apps with a window, `(name, bundle path, pid)`.
fn switcher(filter: &str, apps: Vec<(String, String, i32)>, out: &mut Vec<Item>) {
    for (i, (name, path, pid)) in apps.into_iter().enumerate() {
        let score = if filter.is_empty() { Some(1000 - i as i32) } else { fuzzy_words(filter, &Target::new(&name)).map(|(s, _)| s) };
        let Some(score) = score else { continue };
        let mut it = Item::new(&name, "Running", Kind::Window, Icon::Shell(path), Action::Window(pid as isize));
        it.score = score;
        if !filter.is_empty() {
            it.hl = highlight_tokens(&name, filter);
        }
        out.push(it);
    }
}

fn expand_home(t: &str) -> String {
    match t.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", heimdall_common::home().to_string_lossy()),
        None => t.to_string(),
    }
}

/// Browse: `~/Doc` lists entries of the home folder starting with `Doc`.
fn paths(q: &str, out: &mut Vec<Item>) {
    let full = expand_home(q);
    let (dir, partial) = match full.rfind('/') {
        Some(i) => (&full[..=i], &full[i + 1..]),
        None => (full.as_str(), ""),
    };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let pl = partial.to_lowercase();
    let mut rows: Vec<(i32, bool, String)> = rd
        .flatten()
        .take(5000)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // Dotfiles only when asked for.
            if name.starts_with('.') && !partial.starts_with('.') {
                return None;
            }
            let nl = name.to_lowercase();
            let score = if pl.is_empty() {
                0
            } else if nl.starts_with(&pl) {
                100
            } else if nl.contains(&pl) {
                40
            } else {
                return None;
            };
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            Some((score + if is_dir { 10 } else { 0 }, is_dir, name))
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.to_lowercase().cmp(&b.2.to_lowercase())));
    for (i, (score, is_dir, name)) in rows.into_iter().take(60).enumerate() {
        let path = format!("{dir}{name}");
        let kind = if is_dir && !name.ends_with(".app") { Kind::Folder } else if name.ends_with(".app") { Kind::App } else { Kind::File };
        let mut it = Item::new(&name, pretty_path(dir), kind, Icon::Shell(path.clone()), Action::Open(path.clone())).with_key(format!("f:{}", path.to_lowercase()));
        it.score = score * 10 - i as i32;
        it.hl = highlight_tokens(&name, partial);
        it.path = Some(path);
        out.push(it);
    }
}

pub fn file_items(r: &Results, query: &str) -> Vec<Item> {
    r.hits
        .iter()
        .map(|h| {
            let path = from_index_path(&h.path);
            let name = file_name(&path).to_string();
            let kind = if name.ends_with(".app") { Kind::App } else if h.dir { Kind::Folder } else { Kind::File };
            let mut it = Item::new(&name, pretty_path(parent_dir(&path)), kind, Icon::Shell(path.clone()), Action::Open(path.clone()))
                .with_key(format!("f:{}", path.to_lowercase()));
            it.score = 120 + h.score;
            it.hl = highlight_tokens(&name, query);
            it.path = Some(path);
            it
        })
        .collect()
}

/// Tab completion: folders become the new query.
pub fn completion(it: &Item) -> Option<String> {
    let p = it.path.as_ref()?;
    Some(if it.kind == Kind::Folder { format!("{}/", p.trim_end_matches('/')) } else { p.clone() })
}

pub fn file_name(p: &str) -> &str {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or(p)
}

pub fn parent_dir(p: &str) -> &str {
    let t = p.trim_end_matches('/');
    match t.rfind('/') {
        Some(0) => "/",
        Some(i) => &t[..i],
        None => "",
    }
}

/// `/Users/ethan/Documents` -> `~/Documents`
pub fn pretty_path(p: &str) -> String {
    let home = heimdall_common::home().to_string_lossy().into_owned();
    match p.strip_prefix(&home) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => p.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(q: &str) -> Vec<String> {
        let mut out = Vec::new();
        settings_panes(q, &mut out);
        out.into_iter().map(|i| i.title).collect()
    }

    #[test]
    fn panes() {
        assert!(titles("dark mode").contains(&"Appearance".to_string()));
        assert!(titles("bluetooth").contains(&"Bluetooth".to_string()));
        assert!(titles("wallpaper").contains(&"Wallpaper".to_string()));
        assert!(titles("re").len() < 6);
    }

    #[test]
    fn symbols_round_trip() {
        for name in SYMBOLS {
            let Icon::Glyph(c) = sym(name) else { panic!() };
            assert_eq!(symbol_name(c), *name);
        }
    }

    #[test]
    fn paths() {
        assert_eq!(parent_dir("/Users/ethan/a.txt"), "/Users/ethan");
        assert_eq!(parent_dir("/a"), "/");
        assert_eq!(file_name("/Applications/Safari.app"), "Safari.app");
        assert_eq!(display_hotkey("Alt+Space"), "\u{2325} Space");
    }
}
