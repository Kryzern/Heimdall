//! Everything that produces result rows except the file index: apps, the
//! calculator, system commands, web searches, open windows and path browsing.

use crate::actions::url_encode;
use crate::apps::App;
use crate::config::Config;
use crate::items::*;
use crate::items::Setting as S;
use crate::platform::{self, pretty_path};
use heimdall_core::{b64, calc, Results};
use windows::Win32::Foundation::HWND;

pub const G_SEARCH: char = '\u{E721}';
const G_CALC: char = '\u{E8EF}';
const G_CODE: char = '\u{E943}';
const G_GLOBE: char = '\u{E774}';
const G_LOCK: char = '\u{E72E}';
const G_SLEEP: char = '\u{E708}';
const G_RESTART: char = '\u{E777}';
const G_POWER: char = '\u{E7E8}';
const G_SIGNOUT: char = '\u{E748}';
const G_BIN: char = '\u{E74D}';
const G_SETTINGS: char = '\u{E713}';
const G_EDIT: char = '\u{E70F}';
const G_KEYBOARD: char = '\u{E765}';
const G_STARTUP: char = '\u{E768}';
const G_TRAY: char = '\u{E7F4}';
const G_THEME: char = '\u{E706}';
const G_GLASS: char = '\u{E81E}';
const G_SHADOW: char = '\u{E890}';
const G_CORNERS: char = '\u{E739}';
const G_ANIM: char = '\u{E916}';
const G_RECENT: char = '\u{E823}';
const G_GAME: char = '\u{E7FC}';
const G_TIP: char = '\u{E82F}';
const G_FILTER: char = '\u{E71C}';

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Empty query: recent items.
    Recent,
    Normal,
    /// `>` prefix: switch between open windows.
    Windows,
    /// A path like `C:\Us` or `~\Doc`: browse the filesystem.
    Path,
    /// `?`: tips and filters.
    Help,
    /// Heimdall's own settings page. Not reachable by typing; the app sets it.
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
    } else if is_path(t) {
        Mode::Path
    } else {
        Mode::Normal
    }
}

fn is_path(t: &str) -> bool {
    let b = t.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/'))
        || t.starts_with("\\\\")
        || t.starts_with("~\\")
        || t.starts_with("~/")
}

pub struct Ctx<'a> {
    pub apps: &'a [App],
    pub cfg: &'a Config,
    pub service: bool,
}

/// Rows that can be computed instantly on the UI thread. Returns whether the
/// file index should also be queried.
pub fn instant(q: &str, ctx: &Ctx, own: HWND) -> (Vec<Item>, bool) {
    let mut out = Vec::new();
    match mode(q) {
        Mode::Recent | Mode::Settings => return (out, false),
        Mode::Help => return (tips(), false),
        Mode::Windows => {
            windows(q.trim_start()[1..].trim(), own, &mut out);
            return (out, false);
        }
        Mode::Path => {
            paths(q.trim(), &mut out);
            return (out, false);
        }
        Mode::Normal => {}
    }
    let q = q.trim();

    // Base64: `b64 text` encodes, `b64d text` decodes.
    if let Some((key, rest)) = q.split_once(' ') {
        let rest = rest.trim();
        let decode = key.eq_ignore_ascii_case("b64d");
        if (decode || key.eq_ignore_ascii_case("b64")) && !rest.is_empty() {
            let it = match decode.then(|| b64::decode_text(rest)) {
                None => {
                    let s = b64::encode(rest.as_bytes());
                    Item::new(&s, "Base64 encoded  ·  Enter to copy", Kind::Base64, Icon::Glyph(G_CODE), Action::Copy(s.clone()))
                }
                Some(Some(s)) => Item::new(&s, "Base64 decoded  ·  Enter to copy", Kind::Base64, Icon::Glyph(G_CODE), Action::Copy(s.clone())),
                Some(None) => Item::new("Not valid Base64 text", rest, Kind::Base64, Icon::Glyph(G_CODE), Action::Insert(q.to_string())),
            };
            out.push(it);
            return (out, false);
        }
    }

    // Web prefix: `g rust lifetimes`.
    if let Some((key, rest)) = q.split_once(' ') {
        if let Some(w) = ctx.cfg.web.iter().find(|w| w.key.eq_ignore_ascii_case(key)) {
            let rest = rest.trim();
            if !rest.is_empty() {
                let mut it = Item::new(
                    format!("Search {} for \u{201C}{rest}\u{201D}", w.name),
                    w.url.replace("{q}", &url_encode(rest)),
                    Kind::Web,
                    Icon::Glyph(G_GLOBE),
                    Action::Url(w.url.replace("{q}", &url_encode(rest))),
                );
                it.score = 9500;
                out.push(it);
                return (out, false);
            }
        }
    }

    if let Some(v) = calc::eval(q) {
        let s = calc::format(v);
        let mut it = Item::new(&s, format!("{q} =  ·  Enter to copy"), Kind::Calc, Icon::Glyph(G_CALC), Action::Copy(s.clone()));
        it.score = 10_000;
        out.push(it);
    }

    for app in ctx.apps {
        if let Some((score, pos)) = fuzzy_words(q, &app.target) {
            let sub = app.path.as_deref().map(pretty_path).unwrap_or_default();
            let mut it = Item::new(&app.name, sub, Kind::App, Icon::Shell(app.launch_uri()), Action::Open(app.launch_uri()))
                .with_key(format!("a:{}", app.id.to_lowercase()));
            it.score = score;
            it.hl = ranges16(&app.name, &pos);
            it.path = app.path.clone();
            out.push(it);
        }
    }

    if q.chars().count() >= 2 {
        commands(q, ctx, &mut out);
    }

    // Always offer a web search as the last resort.
    if let Some(w) = ctx.cfg.web.first() {
        let mut it = Item::new(
            format!("Search {} for \u{201C}{q}\u{201D}", w.name),
            "",
            Kind::Web,
            Icon::Glyph(G_GLOBE),
            Action::Url(w.url.replace("{q}", &url_encode(q))),
        );
        it.score = -100_000;
        out.push(it);
    }
    (out, q.chars().count() >= 2)
}

fn commands(q: &str, ctx: &Ctx, out: &mut Vec<Item>) {
    let sys: &[(&str, &str, char, SysCmd)] = &[
        ("Lock", "lock computer screen", G_LOCK, SysCmd::Lock),
        ("Sleep", "sleep suspend", G_SLEEP, SysCmd::Sleep),
        ("Restart", "restart reboot", G_RESTART, SysCmd::Restart),
        ("Shut down", "shutdown power off turn off", G_POWER, SysCmd::Shutdown),
        ("Sign out", "sign out log off logout", G_SIGNOUT, SysCmd::SignOut),
        ("Empty Recycle Bin", "empty recycle bin trash", G_BIN, SysCmd::EmptyBin),
        ("Windows Settings", "settings control panel", G_SETTINGS, SysCmd::Settings),
    ];
    for &(title, aliases, glyph, cmd) in sys {
        let best = std::iter::once(title)
            .chain(aliases.split(' '))
            .filter_map(|a| fuzzy_words(q, &Target::new(a)).map(|(s, _)| s))
            .max();
        if let Some(score) = best.filter(|s| *s >= 450) {
            let mut it = Item::new(title, "System", Kind::System, Icon::Glyph(glyph), Action::System(cmd))
                .with_key(format!("s:{title}"));
            it.score = score - 40;
            it.confirm = matches!(cmd, SysCmd::Restart | SysCmd::Shutdown | SysCmd::SignOut);
            if let Some((_, pos)) = fuzzy_words(q, &Target::new(title)) {
                it.hl = ranges16(title, &pos);
            }
            out.push(it);
        }
    }

    win_settings(q, out);

    let mut own: Vec<(&str, &str, Internal)> = vec![
        ("Heimdall: Settings", "heimdall settings preferences hotkey shortcut theme", Internal::Settings),
        ("Heimdall: Open config file", "heimdall config toml edit", Internal::EditConfig),
        ("Heimdall: Rebuild file index", "heimdall rebuild reindex index", Internal::ReloadIndex),
        ("Heimdall: Import Flow Launcher web searches", "heimdall import flow launcher", Internal::ImportFlow),
        ("Heimdall: Quit", "heimdall quit exit close", Internal::Quit),
    ];
    if !ctx.service {
        own.insert(
            0,
            ("Heimdall: Install the fast file indexer", "heimdall install indexer service faster mft", Internal::InstallIndexer),
        );
    }
    for (title, aliases, cmd) in own {
        let best = std::iter::once(title)
            .chain(aliases.split(' '))
            .filter_map(|a| fuzzy_words(q, &Target::new(a)).map(|(s, _)| s))
            .max();
        if let Some(score) = best.filter(|s| *s >= 450) {
            let mut it = Item::new(title, "", Kind::Command, heimdall_icon(), Action::Heimdall(cmd))
                .with_key(format!("h:{title}"));
            it.score = score - 60;
            out.push(it);
        }
    }
}

/// Heimdall's own logo, so its commands never look like Windows Settings.
fn heimdall_icon() -> Icon {
    static EXE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    match EXE.get_or_init(|| std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned())) {
        Some(p) => Icon::Shell(p.clone()),
        None => Icon::Glyph(G_SETTINGS),
    }
}

/// Pages in Windows Settings and a few classic tools: (title, target,
/// keywords, Segoe Fluent glyph). The glyphs are drawn in the same muted
/// colour as every other row icon.
const WIN_SETTINGS: &[(&str, &str, &str, char)] = &[
    ("Display", "ms-settings:display", "screen resolution brightness scale scaling monitor refresh rate hdr", '\u{E7F4}'),
    ("Night light", "ms-settings:nightlight", "blue light night warm", '\u{E708}'),
    ("Sound", "ms-settings:sound", "audio volume speakers headphones microphone output input", '\u{E767}'),
    ("Volume mixer", "ms-settings:apps-volume", "app volume mixer audio", '\u{E767}'),
    ("Notifications", "ms-settings:notifications", "alerts do not disturb focus", '\u{EA8F}'),
    ("Power & battery", "ms-settings:powersleep", "power sleep battery energy saver screen timeout", '\u{E945}'),
    ("Storage", "ms-settings:storagesense", "disk space storage sense cleanup free up", '\u{EDA2}'),
    ("Multitasking", "ms-settings:multitasking", "snap windows alt tab virtual desktops", '\u{E7C4}'),
    ("Clipboard", "ms-settings:clipboard", "clipboard history paste", '\u{E77F}'),
    ("About", "ms-settings:about", "system pc name specs version rename computer device info", '\u{E946}'),
    ("Bluetooth & devices", "ms-settings:bluetooth", "bluetooth devices pair headphones controller", '\u{E702}'),
    ("Printers & scanners", "ms-settings:printers", "printer scanner print", '\u{E749}'),
    ("Mouse", "ms-settings:mousetouchpad", "mouse pointer cursor speed scroll", '\u{E962}'),
    ("Touchpad", "ms-settings:devices-touchpad", "touchpad trackpad gestures", '\u{E962}'),
    ("Wi-Fi", "ms-settings:network-wifi", "wifi wireless network internet", '\u{E701}'),
    ("Network & internet", "ms-settings:network", "network internet ethernet ip dns adapter", '\u{E839}'),
    ("VPN", "ms-settings:network-vpn", "vpn", '\u{E705}'),
    ("Proxy", "ms-settings:network-proxy", "proxy", '\u{E774}'),
    ("Airplane mode", "ms-settings:network-airplanemode", "airplane flight mode", '\u{E709}'),
    ("Mobile hotspot", "ms-settings:network-mobilehotspot", "hotspot tethering share internet", '\u{E88A}'),
    ("Personalization", "ms-settings:personalization", "personalize appearance", '\u{E771}'),
    ("Background", "ms-settings:personalization-background", "wallpaper desktop background", '\u{E91B}'),
    ("Colors", "ms-settings:colors", "dark mode light mode theme accent colour color transparency", '\u{E790}'),
    ("Themes", "ms-settings:themes", "theme cursor icons", '\u{E771}'),
    ("Lock screen", "ms-settings:lockscreen", "lock screen", '\u{E72E}'),
    ("Start menu", "ms-settings:personalization-start", "start menu pins recommended", '\u{ECA5}'),
    ("Taskbar", "ms-settings:taskbar", "taskbar system tray icons", '\u{E7C4}'),
    ("Fonts", "ms-settings:fonts", "fonts typeface", '\u{E8D2}'),
    ("Installed apps", "ms-settings:appsfeatures", "uninstall apps programs remove features", '\u{E71D}'),
    ("Default apps", "ms-settings:defaultapps", "default browser apps file associations open with", '\u{E7AC}'),
    ("Startup apps", "ms-settings:startupapps", "startup boot autostart login", '\u{E768}'),
    ("Optional features", "ms-settings:optionalfeatures", "optional features add", '\u{E710}'),
    ("Your info", "ms-settings:yourinfo", "account profile picture microsoft account", '\u{E77B}'),
    ("Sign-in options", "ms-settings:signinoptions", "password pin windows hello fingerprint face login", '\u{E8D7}'),
    ("Email & accounts", "ms-settings:emailandaccounts", "email accounts work school", '\u{E715}'),
    ("Date & time", "ms-settings:dateandtime", "date time clock timezone time zone", '\u{E823}'),
    ("Language & region", "ms-settings:regionlanguage", "language region locale keyboard layout", '\u{E8C1}'),
    ("Typing", "ms-settings:typing", "keyboard typing autocorrect spellcheck", '\u{E765}'),
    ("Speech", "ms-settings:speech", "voice speech dictation", '\u{E720}'),
    ("Game Bar", "ms-settings:gaming-gamebar", "game bar xbox gaming record", '\u{E7FC}'),
    ("Game Mode", "ms-settings:gaming-gamemode", "game mode gaming performance", '\u{E7FC}'),
    ("Graphics", "ms-settings:display-advancedgraphics", "gpu graphics preference performance", '\u{E9D9}'),
    ("Accessibility", "ms-settings:easeofaccess", "accessibility ease of access", '\u{E776}'),
    ("Text size", "ms-settings:easeofaccess-display", "text size larger bigger", '\u{E8D2}'),
    ("Magnifier", "ms-settings:easeofaccess-magnifier", "magnifier zoom", '\u{E71E}'),
    ("Narrator", "ms-settings:easeofaccess-narrator", "narrator screen reader", '\u{E776}'),
    ("Privacy & security", "ms-settings:privacy", "privacy security permissions", '\u{EA18}'),
    ("Camera permissions", "ms-settings:privacy-webcam", "camera webcam privacy", '\u{E722}'),
    ("Microphone permissions", "ms-settings:privacy-microphone", "microphone mic privacy", '\u{E720}'),
    ("Location", "ms-settings:privacy-location", "location gps privacy", '\u{E707}'),
    ("Windows Security", "windowsdefender:", "antivirus defender virus firewall security", '\u{EA18}'),
    ("Windows Update", "ms-settings:windowsupdate", "update updates patch upgrade", '\u{E895}'),
    ("Recovery", "ms-settings:recovery", "reset recovery restore reinstall", '\u{E777}'),
    ("Activation", "ms-settings:activation", "activation product key license", '\u{E73E}'),
    ("For developers", "ms-settings:developers", "developer mode sudo", '\u{E943}'),
    ("Control Panel", "control.exe", "control panel classic", '\u{E713}'),
    ("Device Manager", "devmgmt.msc", "device manager drivers hardware", '\u{E772}'),
    ("Environment variables", "SystemPropertiesAdvanced.exe", "environment variables path advanced system settings", '\u{E943}'),
    ("Disk Management", "diskmgmt.msc", "disk management partition format drive", '\u{EDA2}'),
    ("Services", "services.msc", "services", '\u{E90F}'),
];

/// Windows settings pages: a title match at substring strength, or a keyword
/// match at word-prefix strength, so `re` doesn't flood the list.
fn win_settings(q: &str, out: &mut Vec<Item>) {
    // Two letters match half the titles somewhere and most keywords at a word
    // start, so short queries only look at word starts in titles.
    let short = q.chars().count() < 3;
    let floor = if short { 600 } else { 450 };
    for &(title, target, keywords, glyph) in WIN_SETTINGS {
        let t = fuzzy_words(q, &Target::new(title)).filter(|(s, _)| *s >= floor);
        // Every query word has to start one of the keywords: `dark mode`
        // finds Colors through "dark" and "mode" separately.
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
        let sub = if target.contains(':') { "Windows Settings" } else { "Windows tool" };
        let mut it = Item::new(title, sub, Kind::WinSetting, Icon::Glyph(glyph), Action::Open(target.to_string()))
            .with_key(format!("w:{target}"));
        it.score = score - 50;
        if let Some((_, pos)) = t {
            it.hl = ranges16(title, &pos);
        }
        out.push(it);
    }
}

/// The `?` page: what you can type.
fn tips() -> Vec<Item> {
    let rows: &[(&str, &str, char, &str)] = &[
        ("ext:pdf", "Only these extensions, e.g. ext:pdf,docx", G_FILTER, "ext:"),
        ("type:image", "Also video, audio, doc, code, archive, app, folder and file", G_FILTER, "type:"),
        ("in:downloads", "Inside a folder with this name", G_FILTER, "in:"),
        ("size:>100mb", "Bigger than a size; size:<1mb for smaller", G_FILTER, "size:>"),
        ("mod:week", "Changed recently: today, week, month, 3d, 12h", G_FILTER, "mod:"),
        (">", "Switch between open windows", G_TIP, ">"),
        ("C:\\", "Browse folders by path; Tab completes a folder", G_TIP, "C:\\"),
        ("g rust lifetimes", "Search the web with a prefix: g, ddg, yt, gh, wiki", G_GLOBE, "g "),
        ("2 * (3 + 4)", "Calculator; Enter copies the answer", G_CALC, ""),
        ("b64 hello", "Base64 encode; b64d decodes. Enter copies it", G_CODE, "b64 "),
    ];
    let mut out: Vec<Item> = rows
        .iter()
        .map(|&(title, sub, glyph, insert)| {
            Item::new(title, sub, Kind::Tip, Icon::Glyph(glyph), Action::Insert(insert.to_string())).with_key(format!("tip:{title}"))
        })
        .collect();
    out.push(
        Item::new("Heimdall settings", "Change the hotkey, theme, shadow and more", Kind::Tip, heimdall_icon(), Action::Heimdall(Internal::Settings))
            .with_key("tip:settings"),
    );
    let n = out.len() as i32;
    for (i, it) in out.iter_mut().enumerate() {
        it.score = n - i as i32;
    }
    out
}

/// Heimdall's settings page, filtered by `q`. `recording` and `notice` describe
/// the hotkey row while a new shortcut is being captured.
pub fn settings(q: &str, cfg: &Config, recording: bool, notice: Option<&str>) -> Vec<Item> {
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
    let hotkey_value = if recording { "\u{2026}".to_string() } else { cfg.hotkey.clone() };
    let rows: Vec<(&str, String, char, Setting, String)> = vec![
        ("Hotkey", hotkey_sub, G_KEYBOARD, S::Hotkey, hotkey_value),
        ("Start with Windows", "Open in the background when you sign in".into(), G_STARTUP, S::Autostart, on(cfg.start_with_windows)),
        ("Tray icon", "Show Heimdall in the notification area".into(), G_TRAY, S::Tray, on(cfg.tray)),
        ("Theme", "System, dark or light".into(), G_THEME, S::Theme, cap(&cfg.theme)),
        ("Glass", "Liquid, auto (follow Windows transparency) or solid".into(), G_GLASS, S::Glass, cap(&cfg.glass)),
        ("Shadow", "Normal, soft or off".into(), G_SHADOW, S::Shadow, cap(&cfg.shadow)),
        ("Corners", "Round, Windows (like Windows 11) or square".into(), G_CORNERS, S::Corners, cap(&cfg.corners)),
        ("Animations", "Turn off to make everything appear instantly".into(), G_ANIM, S::Animations, on(cfg.animations)),
        ("Recent items", "Show recent files when the search box is empty".into(), G_RECENT, S::Recent, on(cfg.show_recent)),
        ("Game mode", "Release the hotkey while a fullscreen game has focus".into(), G_GAME, S::GameMode, on(cfg.game_mode)),
    ];
    let mut out = Vec::new();
    for (title, sub, glyph, setting, value) in rows {
        let hit = if q.trim().is_empty() { Some((0, Vec::new())) } else { fuzzy_words(q, &Target::new(title)) };
        let Some((_, pos)) = hit else { continue };
        let mut it = Item::new(title, sub, Kind::Setting, Icon::Glyph(glyph), Action::Heimdall(Internal::Set(setting)))
            .with_key(format!("set:{title}"));
        it.hl = ranges16(title, &pos);
        it.value = value;
        out.push(it);
    }
    let edit = "Open config file";
    if q.trim().is_empty() || fuzzy_words(q, &Target::new(edit)).is_some() {
        out.push(
            Item::new(edit, "Width, result count, web searches and more", Kind::Setting, Icon::Glyph(G_EDIT), Action::Heimdall(Internal::EditConfig))
                .with_key("set:config"),
        );
    }
    let n = out.len() as i32;
    for (i, it) in out.iter_mut().enumerate() {
        it.score = 1000 + n - i as i32;
    }
    out
}

fn windows(filter: &str, own: HWND, out: &mut Vec<Item>) {
    for (i, w) in platform::windows(own).into_iter().enumerate() {
        let exe = w.exe.as_deref().map(platform::file_name).unwrap_or("").to_string();
        let target = Target::new(&format!("{} {}", w.title, exe.trim_end_matches(".exe")));
        let score = if filter.is_empty() {
            Some(1000 - i as i32)
        } else {
            fuzzy_words(filter, &target).map(|(s, _)| s)
        };
        let Some(score) = score else { continue };
        let icon = w.exe.clone().map(Icon::Shell).unwrap_or(Icon::None);
        let mut it = Item::new(&w.title, &exe, Kind::Window, icon, Action::Window(w.hwnd));
        it.score = score;
        if !filter.is_empty() {
            it.hl = highlight_tokens(&w.title, filter);
        }
        out.push(it);
    }
}

fn expand_home(t: &str) -> String {
    if let Some(rest) = t.strip_prefix("~\\").or_else(|| t.strip_prefix("~/")) {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return format!("{}\\{}", home.to_string_lossy(), rest);
        }
    }
    t.replace('/', "\\")
}

/// Browse: `C:\Users\et` lists entries of `C:\Users\` starting with `et`.
fn paths(q: &str, out: &mut Vec<Item>) {
    let full = expand_home(q);
    let (dir, partial) = match full.rfind('\\') {
        Some(i) => (&full[..=i], &full[i + 1..]),
        None => (full.as_str(), ""),
    };
    let dir = if dir.len() == 2 { format!("{dir}\\") } else { dir.to_string() };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let pl = partial.to_lowercase();
    let mut rows: Vec<(i32, bool, String)> = rd
        .flatten()
        .take(5000)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
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
        let mut it = Item::new(
            &name,
            pretty_path(&dir),
            if is_dir { Kind::Folder } else { Kind::File },
            Icon::Shell(path.clone()),
            Action::Open(path.clone()),
        )
        .with_key(format!("f:{}", path.to_lowercase()));
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
            let name = platform::file_name(&h.path).to_string();
            let mut it = Item::new(
                &name,
                pretty_path(platform::parent_dir(&h.path)),
                if h.dir { Kind::Folder } else { Kind::File },
                Icon::Shell(h.path.clone()),
                Action::Open(h.path.clone()),
            )
            .with_key(format!("f:{}", h.path.to_lowercase()));
            // Index scores sit roughly in -150..250; place them below strong app
            // matches but above weak ones.
            it.score = 120 + h.score;
            it.hl = highlight_tokens(&name, query);
            it.path = Some(h.path.clone());
            it
        })
        .collect()
}

/// Rows for recent documents (Windows' own Recent list).
pub fn recent_items(paths: &[String]) -> Vec<Item> {
    paths
        .iter()
        .map(|p| {
            let dir = std::path::Path::new(p).is_dir();
            let mut it = Item::new(
                platform::file_name(p),
                pretty_path(platform::parent_dir(p)),
                if dir { Kind::Folder } else { Kind::File },
                Icon::Shell(p.clone()),
                Action::Open(p.clone()),
            )
            .with_key(format!("f:{}", p.to_lowercase()));
            it.path = Some(p.clone());
            it
        })
        .collect()
}

/// Tab completion target for a row: folders become the new query.
pub fn completion(it: &Item) -> Option<String> {
    let p = it.path.as_ref()?;
    Some(if it.kind == Kind::Folder { format!("{}\\", p.trim_end_matches('\\')) } else { p.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(q: &str) -> Vec<String> {
        let mut out = Vec::new();
        win_settings(q, &mut out);
        out.into_iter().map(|i| i.title).collect()
    }

    #[test]
    fn windows_settings_search() {
        assert!(titles("dark mode").contains(&"Colors".to_string()));
        assert!(titles("bluetooth").contains(&"Bluetooth & devices".to_string()));
        assert!(titles("uninstall").contains(&"Installed apps".to_string()));
        assert!(titles("display").contains(&"Display".to_string()));
        assert!(titles("wallpaper").contains(&"Background".to_string()));
        let re = titles("re");
        assert!(re.len() < 6, "{re:?}");
    }
}
