//! Result rows and the small-list fuzzy matcher used for apps, commands and windows.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    App,
    File,
    Folder,
    Calc,
    Web,
    System,
    Window,
    Command,
    /// A page in Windows Settings or Control Panel.
    WinSetting,
    /// A row on Heimdall's own settings page.
    Setting,
    /// A query tip on the `?` page.
    Tip,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::App => "Application",
            Kind::File => "File",
            Kind::Folder => "Folder",
            Kind::Calc => "Calculator",
            Kind::Web => "Web",
            Kind::System => "System",
            Kind::Window => "Window",
            Kind::Command => "Heimdall",
            Kind::WinSetting => "Settings",
            Kind::Setting => "",
            Kind::Tip => "Tip",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    None,
    /// A Segoe Fluent Icons glyph.
    Glyph(char),
    /// Shell icon for a filesystem path or `shell:AppsFolder\...` item.
    Shell(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SysCmd {
    Lock,
    Sleep,
    Restart,
    Shutdown,
    SignOut,
    EmptyBin,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Internal {
    Quit,
    Settings,
    EditConfig,
    ReloadIndex,
    InstallIndexer,
    ImportFlow,
    Set(Setting),
}

/// Options on the settings page. Enter changes the selected one in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Hotkey,
    Autostart,
    Tray,
    Theme,
    Glass,
    Shadow,
    Recent,
    GameMode,
}

#[derive(Clone, Debug)]
pub enum Action {
    /// Launch through the shell (apps, files, folders, `shell:` URIs).
    Open(String),
    Url(String),
    Copy(String),
    System(SysCmd),
    Window(isize),
    Heimdall(Internal),
    /// Replace the query (tips: `ext:` and friends).
    Insert(String),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub title: String,
    pub subtitle: String,
    pub kind: Kind,
    pub icon: Icon,
    pub action: Action,
    pub score: i32,
    /// Stable identity used for usage history.
    pub key: String,
    /// UTF-16 ranges of `title` to highlight.
    pub hl: Vec<(u32, u32)>,
    /// Filesystem path, when the row is a file or folder (preview, drag, reveal).
    pub path: Option<String>,
    /// Needs a second Enter (restart, shut down, sign out).
    pub confirm: bool,
    /// Shown on the right instead of the kind label (settings: "On", "Dark").
    pub value: String,
}

impl Item {
    pub fn new(title: impl Into<String>, subtitle: impl Into<String>, kind: Kind, icon: Icon, action: Action) -> Item {
        let title = title.into();
        Item {
            key: String::new(),
            title,
            subtitle: subtitle.into(),
            kind,
            icon,
            action,
            score: 0,
            hl: Vec::new(),
            path: None,
            confirm: false,
            value: String::new(),
        }
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Item {
        self.key = key.into();
        self
    }

    pub fn verb(&self) -> &'static str {
        match self.action {
            Action::Copy(_) => "Copy",
            Action::Window(_) => "Switch",
            Action::Url(_) => "Search",
            Action::Heimdall(Internal::Set(_)) => "Change",
            Action::System(_) | Action::Heimdall(_) => "Run",
            Action::Insert(_) => "Use",
            Action::Open(_) => match self.kind {
                Kind::App => "Launch",
                _ => "Open",
            },
        }
    }
}

/// Precomputed matching data for a candidate string.
#[derive(Clone, Debug)]
pub struct Target {
    pub lower: Vec<char>,
    /// Indices (in chars) where words start.
    pub starts: Vec<usize>,
}

impl Target {
    pub fn new(s: &str) -> Target {
        let chars: Vec<char> = s.chars().collect();
        let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
        let mut starts = Vec::new();
        for i in 0..chars.len() {
            let c = chars[i];
            if !c.is_alphanumeric() {
                continue;
            }
            let prev = if i == 0 { None } else { Some(chars[i - 1]) };
            let boundary = match prev {
                None => true,
                Some(p) => !p.is_alphanumeric() || (p.is_lowercase() && c.is_uppercase()) || (p.is_alphabetic() && c.is_numeric()),
            };
            if boundary {
                starts.push(i);
            }
        }
        Target { lower, starts }
    }
}

/// Score `query` (lowercase chars) against a target. Higher is better; None = no match.
/// Returns matched char positions for highlighting.
pub fn fuzzy(query: &[char], t: &Target) -> Option<(i32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let n = t.lower.len();
    let q = query.len();
    if q > n {
        return None;
    }
    if t.lower == query {
        return Some((1000, (0..n).collect()));
    }
    if t.lower.starts_with(query) {
        return Some((850 - (n - q).min(100) as i32, (0..q).collect()));
    }
    // Word prefix: "code" in "Visual Studio Code".
    for &s in &t.starts {
        if t.lower[s..].starts_with(query) {
            return Some((700 - s.min(50) as i32 - (n - q).min(60) as i32 / 2, (s..s + q).collect()));
        }
    }
    // Acronym: "vsc" -> Visual Studio Code.
    if q >= 2 && q <= t.starts.len() {
        let ok = t.starts.iter().zip(query).all(|(&s, c)| t.lower[s] == *c);
        if ok {
            return Some((650 - (t.starts.len() - q) as i32 * 10, t.starts[..q].to_vec()));
        }
    }
    // Substring.
    if let Some(p) = t.lower.windows(q).position(|w| w == query) {
        return Some((450 - p.min(60) as i32 - (n - q).min(60) as i32 / 2, (p..p + q).collect()));
    }
    // Subsequence, preferring word starts.
    let mut pos = Vec::with_capacity(q);
    let mut i = 0;
    let mut score = 260i32;
    let mut last: Option<usize> = None;
    for &c in query {
        // Prefer the next word start holding `c`, else the next occurrence.
        let ws = t.starts.iter().copied().find(|&s| s >= i && t.lower[s] == c);
        let any = (i..n).find(|&k| t.lower[k] == c)?;
        let k = match ws {
            Some(s) if s - any <= 3 => s,
            _ => any,
        };
        if let Some(l) = last {
            score -= ((k - l - 1).min(8) * 4) as i32;
        }
        if t.starts.contains(&k) {
            score += 6;
        }
        pos.push(k);
        last = Some(k);
        i = k + 1;
    }
    (score > 120).then_some((score, pos))
}

/// Multi-word matching: every word must match; scores are averaged.
pub fn fuzzy_words(query: &str, t: &Target) -> Option<(i32, Vec<usize>)> {
    let words: Vec<Vec<char>> = query.split_whitespace().map(|w| w.to_lowercase().chars().collect()).collect();
    if words.len() <= 1 {
        let q: Vec<char> = query.trim().to_lowercase().chars().collect();
        return fuzzy(&q, t);
    }
    let mut total = 0;
    let mut all = Vec::new();
    for w in &words {
        let (s, p) = fuzzy(w, t)?;
        total += s;
        all.extend(p);
    }
    all.sort_unstable();
    all.dedup();
    Some((total / words.len() as i32, all))
}

/// Convert char positions to merged UTF-16 ranges for DirectWrite.
pub fn ranges16(s: &str, positions: &[usize]) -> Vec<(u32, u32)> {
    let mut offs = Vec::with_capacity(s.len() + 1);
    let mut acc = 0u32;
    for c in s.chars() {
        offs.push((acc, c.len_utf16() as u32));
        acc += c.len_utf16() as u32;
    }
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &p in positions {
        let Some(&(start, len)) = offs.get(p) else { continue };
        match out.last_mut() {
            Some(last) if last.0 + last.1 == start => last.1 += len,
            _ => out.push((start, len)),
        }
    }
    out
}

/// Highlight ranges for a file name matched by the indexer (substring per token).
pub fn highlight_tokens(title: &str, query: &str) -> Vec<(u32, u32)> {
    let t = Target::new(title);
    let mut pos = Vec::new();
    for w in query.split_whitespace().filter(|w| !w.contains(':')) {
        let q: Vec<char> = w.to_lowercase().chars().collect();
        if q.is_empty() || q.len() > t.lower.len() {
            continue;
        }
        if let Some(p) = t.lower.windows(q.len()).position(|x| x == q.as_slice()) {
            pos.extend(p..p + q.len());
        }
    }
    pos.sort_unstable();
    pos.dedup();
    ranges16(title, &pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(q: &str, t: &str) -> Option<i32> {
        fuzzy_words(q, &Target::new(t)).map(|x| x.0)
    }

    #[test]
    fn ordering() {
        assert!(s("code", "Code") > s("code", "Visual Studio Code"));
        assert!(s("vsc", "Visual Studio Code").is_some());
        assert!(s("chr", "Google Chrome") > s("chr", "Character Map"));
        assert!(s("term", "Windows Terminal").unwrap() > 600);
        assert!(s("zzz", "Notepad").is_none());
        assert!(s("ps", "PowerShell").unwrap() >= 650);
    }

    #[test]
    fn utf16_ranges() {
        assert_eq!(ranges16("a😀bc", &[0, 1, 2]), vec![(0, 4)]);
        assert_eq!(ranges16("abc", &[0, 2]), vec![(0, 1), (2, 1)]);
    }
}
