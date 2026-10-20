//! Everything that produces result rows except the file index: apps, the
//! calculator, system commands, web searches, open windows and path browsing.

use crate::actions::url_encode;
use crate::apps::App;
use crate::config::Config;
use crate::items::*;
use crate::platform::{self, pretty_path};
use heimdall_core::{calc, Results};
use windows::Win32::Foundation::HWND;

pub const G_SEARCH: char = '\u{E721}';
const G_CALC: char = '\u{E8EF}';
const G_GLOBE: char = '\u{E774}';
const G_LOCK: char = '\u{E72E}';
const G_SLEEP: char = '\u{E708}';
const G_RESTART: char = '\u{E777}';
const G_POWER: char = '\u{E7E8}';
const G_SIGNOUT: char = '\u{E748}';
const G_BIN: char = '\u{E74D}';
const G_SETTINGS: char = '\u{E713}';
const G_QUIT: char = '\u{E711}';
const G_EDIT: char = '\u{E70F}';
const G_REFRESH: char = '\u{E72C}';
const G_DOWNLOAD: char = '\u{E896}';
const G_IMPORT: char = '\u{E8B5}';

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Empty query: recent items.
    Recent,
    Normal,
    /// `>` prefix: switch between open windows.
    Windows,
    /// A path like `C:\Us` or `~\Doc`: browse the filesystem.
    Path,
}

pub fn mode(q: &str) -> Mode {
    let t = q.trim_start();
    if t.is_empty() {
        Mode::Recent
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
        Mode::Recent => return (out, false),
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

    let mut own: Vec<(&str, &str, char, Internal)> = vec![
        ("Heimdall: Settings", "heimdall settings config preferences", G_EDIT, Internal::EditConfig),
        ("Heimdall: Rebuild file index", "heimdall rebuild reindex index", G_REFRESH, Internal::ReloadIndex),
        ("Heimdall: Import Flow Launcher web searches", "heimdall import flow launcher", G_IMPORT, Internal::ImportFlow),
        ("Heimdall: Quit", "heimdall quit exit close", G_QUIT, Internal::Quit),
    ];
    if !ctx.service {
        own.insert(
            0,
            ("Heimdall: Install the fast file indexer", "heimdall install indexer service faster mft", G_DOWNLOAD, Internal::InstallIndexer),
        );
    }
    for (title, aliases, glyph, cmd) in own {
        let best = std::iter::once(title)
            .chain(aliases.split(' '))
            .filter_map(|a| fuzzy_words(q, &Target::new(a)).map(|(s, _)| s))
            .max();
        if let Some(score) = best.filter(|s| *s >= 450) {
            let mut it = Item::new(title, "", Kind::Command, Icon::Glyph(glyph), Action::Heimdall(cmd))
                .with_key(format!("h:{title}"));
            it.score = score - 60;
            out.push(it);
        }
    }
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
