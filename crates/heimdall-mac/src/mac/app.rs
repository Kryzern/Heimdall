//! The launcher on macOS: state, input, layout and animation. The flow and
//! the layout follow the Windows app closely so the two look and behave the
//! same; only the platform calls differ.

use super::draw::{self, Cmd, Rect, Rgba, Text, REGULAR, SEMIBOLD};
use super::files::FileSearch;
use super::sources::{self, Ctx, Mode};
use super::{apps, hotkey, platform, HeimdallView, Panel, CHOSEN};
use heimdall_common::config::{self, Config};
use heimdall_common::edit::Edit;
use heimdall_common::history::History;
use heimdall_common::items::*;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::*;
use std::collections::HashSet;
use std::sync::mpsc::Receiver;
use std::time::Instant;

// Layout, in points. The same numbers as the Windows app (DIPs).
const BAR: f64 = 60.0;
const ROW: f64 = 48.0;
const PAD: f64 = 6.0;
const HEADER: f64 = 26.0;
pub const RADIUS: f64 = 22.0;
const ICON: f64 = 30.0;

struct Spring {
    x: f64,
    v: f64,
    target: f64,
    k: f64,
    zeta: f64,
}

impl Spring {
    fn new(x: f64, k: f64, zeta: f64) -> Spring {
        Spring { x, v: 0.0, target: x, k, zeta }
    }

    fn snap(&mut self, x: f64) {
        self.x = x;
        self.target = x;
        self.v = 0.0;
    }

    /// Advance; returns true while still moving.
    fn step(&mut self, dt: f64) -> bool {
        let c = 2.0 * self.zeta * self.k.sqrt();
        let mut t = dt;
        while t > 0.0 {
            let h = t.min(1.0 / 240.0);
            let f = -self.k * (self.x - self.target) - c * self.v;
            self.v += f * h;
            self.x += self.v * h;
            t -= h;
        }
        let scale = self.target.abs().max(1.0);
        if (self.x - self.target).abs() < 0.002 * scale && self.v.abs() < 0.02 * scale {
            self.snap(self.target);
            return false;
        }
        true
    }
}

struct Theme {
    dark: bool,
    accent: Rgba,
    text: Rgba,
    sub: Rgba,
    faint: Rgba,
    line: Rgba,
    hl: Rgba,
}

impl Theme {
    fn new(dark: bool) -> Theme {
        let (r, g, b) = platform::accent();
        let accent = (r, g, b, 1.0);
        // A grey accent would make highlights vanish; fall back to pure contrast.
        let colourful = r.max(g).max(b) - r.min(g).min(b) > 0.12;
        let mix = |t: f64, to: f64| (r + (to - r) * t, g + (to - g) * t, b + (to - b) * t, 1.0);
        if dark {
            Theme {
                dark,
                accent,
                text: (1.0, 1.0, 1.0, 0.94),
                sub: (1.0, 1.0, 1.0, 0.56),
                faint: (1.0, 1.0, 1.0, 0.36),
                line: (1.0, 1.0, 1.0, 0.09),
                hl: if colourful { mix(0.45, 1.0) } else { (1.0, 1.0, 1.0, 1.0) },
            }
        } else {
            Theme {
                dark,
                accent,
                text: (0.05, 0.05, 0.07, 0.92),
                sub: (0.0, 0.0, 0.02, 0.54),
                faint: (0.0, 0.0, 0.02, 0.38),
                line: (0.0, 0.0, 0.0, 0.08),
                hl: if colourful { mix(0.2, 0.0) } else { (0.0, 0.0, 0.0, 1.0) },
            }
        }
    }
}

fn alpha(c: Rgba, a: f64) -> Rgba {
    (c.0, c.1, c.2, c.3 * a)
}

/// What a key press arrived with.
pub struct Key {
    pub code: u16,
    pub chars: String,
    /// NSEvent modifier flags.
    pub flags: usize,
}

const SHIFT: usize = 1 << 17;
const CONTROL: usize = 1 << 18;
const OPTION: usize = 1 << 19;
const COMMAND: usize = 1 << 20;

/// Things the right-click menu can do to a result.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Open,
    Reveal,
    Pin,
    CopyPath,
}

pub struct App {
    mtm: MainThreadMarker,
    panel: Retained<Panel>,
    view: Retained<HeimdallView>,
    /// Liquid Glass, on macOS 26 and later.
    liquid: Option<Retained<NSGlassEffectView>>,
    status: Option<Retained<NSStatusItem>>,
    cfg: Config,
    loader: config::Loader,
    theme: Theme,
    hotkey_ok: bool,
    pub visible: bool,
    /// Top edge and left edge of the panel on screen (Cocoa coordinates).
    top: f64,
    left: f64,

    edit: Edit,
    mode: Mode,
    instant_rows: Vec<Item>,
    file_rows: Vec<Item>,
    items: Vec<Item>,
    sel: usize,
    scroll: usize,
    user_moved: bool,
    confirm_key: Option<String>,
    qid: u64,
    files_ready: bool,

    apps: Vec<apps::App>,
    apps_rx: Option<Receiver<Vec<apps::App>>>,
    apps_at: Instant,
    history: History,
    files: FileSearch,

    height: Spring,
    sel_y: Spring,
    opacity: Spring,
    anim_last: Option<Instant>,
    /// Restarts the caret blink on input.
    caret_at: Instant,
    caret_was: bool,
    dirty: bool,
    timer: Option<Retained<NSTimer>>,
    /// Recompute the window shadow once the first frame is on screen, so it
    /// follows the glass's rounded corners rather than the square frame.
    shadow_pending: bool,

    settings_page: bool,
    recording: bool,
    notice: Option<String>,
    press: Option<usize>,
    /// Ignore the first mouse move after opening, so the row under a resting
    /// cursor doesn't steal the selection.
    mouse_armed: bool,
    scroll_acc: f64,
    /// Query to type on the first open (`--query`, for screenshots).
    first_query: Option<String>,
}

impl App {
    pub fn new(
        mtm: MainThreadMarker,
        panel: Retained<Panel>,
        view: Retained<HeimdallView>,
        liquid: Option<Retained<NSGlassEffectView>>,
        first_query: Option<String>,
    ) -> App {
        let mut loader = config::Loader::new();
        let cfg = loader.poll().unwrap_or_default();
        platform::set_autostart(cfg.start_with_windows);
        platform::set_theme(&panel, &cfg.theme);
        let theme = Theme::new(platform::is_dark(&panel));
        let mut app = App {
            mtm,
            panel,
            view,
            liquid,
            status: None,
            theme,
            hotkey_ok: false,
            visible: false,
            top: 0.0,
            left: 0.0,
            edit: Edit::default(),
            mode: Mode::Recent,
            instant_rows: Vec::new(),
            file_rows: Vec::new(),
            items: Vec::new(),
            sel: 0,
            scroll: 0,
            user_moved: false,
            confirm_key: None,
            qid: 0,
            files_ready: false,
            apps: Vec::new(),
            apps_rx: None,
            apps_at: Instant::now(),
            history: History::load(),
            files: FileSearch::start(),
            height: Spring::new(BAR, 420.0, 0.88),
            sel_y: Spring::new(0.0, 560.0, 0.80),
            opacity: Spring::new(0.0, 320.0, 1.0),
            anim_last: None,
            caret_at: Instant::now(),
            caret_was: true,
            dirty: true,
            timer: None,
            shadow_pending: false,
            settings_page: false,
            recording: false,
            notice: None,
            press: None,
            mouse_armed: false,
            scroll_acc: 0.0,
            first_query,
            cfg,
            loader,
        };
        app.load_apps();
        app.set_hotkey(true);
        app
    }

    pub fn set_status_item(&mut self, item: Retained<NSStatusItem>) {
        item.setVisible(self.cfg.tray);
        self.status = Some(item);
    }

    fn load_apps(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tx.send(apps::enumerate()).ok();
        });
        self.apps_rx = Some(rx);
        self.apps_at = Instant::now();
    }

    fn set_hotkey(&mut self, on: bool) {
        if on {
            self.hotkey_ok = hotkey::register(&self.cfg.hotkey);
            if !self.hotkey_ok {
                heimdall_common::log(format!("could not register hotkey {}", self.cfg.hotkey));
            }
        } else {
            hotkey::unregister();
            self.hotkey_ok = false;
        }
    }

    fn sync_tray(&self) {
        if let Some(s) = &self.status {
            s.setVisible(self.cfg.tray);
        }
    }

    // ---- show / hide -------------------------------------------------------

    pub fn toggle(&mut self) {
        if self.visible {
            self.hide();
        } else {
            self.show();
        }
    }

    pub fn show(&mut self) {
        if let Some(cfg) = self.loader.poll() {
            let hotkey_changed = cfg.hotkey != self.cfg.hotkey;
            if cfg.start_with_windows != self.cfg.start_with_windows {
                platform::set_autostart(cfg.start_with_windows);
            }
            self.cfg = cfg;
            if hotkey_changed {
                self.set_hotkey(true);
            }
            self.sync_tray();
        }
        platform::set_theme(&self.panel, &self.cfg.theme);
        self.apply_look();
        if self.apps_at.elapsed().as_secs() > 600 {
            self.load_apps();
        }

        let screen = platform::active_screen(self.mtm);
        let w = self.panel_w();
        self.left = screen.origin.x + ((screen.size.width - w) / 2.0).round();
        self.top = screen.origin.y + screen.size.height - (screen.size.height * 0.17).round();

        self.poll();
        if let Some(q) = self.first_query.take() {
            self.edit.set(&q);
        } else if self.cfg.clear_on_close {
            self.edit.clear();
        } else {
            self.edit.select_all();
        }
        self.confirm_key = None;
        self.mouse_armed = false;
        self.refresh();
        self.height.snap(self.target_height());
        self.sel_y.snap(self.sel_target());
        if self.cfg.animations {
            self.opacity.snap(0.0);
            self.opacity.target = 1.0;
        } else {
            self.opacity.snap(1.0);
        }
        self.anim_last = None;
        self.place();
        self.panel.setAlphaValue(self.opacity.x);
        self.render();

        self.visible = true;
        self.panel.makeKeyAndOrderFront(None);
        self.panel.makeFirstResponder(Some(&self.view));
        self.caret_at = Instant::now();
        self.shadow_pending = true;
        self.start_timer();
    }

    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.panel.orderOut(None);
        if let Some(t) = self.timer.take() {
            t.invalidate();
        }
        self.history.save();
        if self.recording {
            self.recording = false;
            self.set_hotkey(true);
        }
        self.settings_page = false;
        self.notice = None;
    }

    /// The panel lost keyboard focus: clicked elsewhere or switched apps.
    pub fn on_resign_key(&mut self) {
        self.hide();
    }

    fn start_timer(&mut self) {
        if self.timer.is_some() {
            return;
        }
        let block = block2::RcBlock::new(|_t: std::ptr::NonNull<NSTimer>| {
            super::with_app(|a| a.tick());
        });
        let t = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / 120.0, true, &block) };
        // Keep ticking while a menu or resize is tracking the mouse.
        unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&t, NSRunLoopCommonModes) };
        self.timer = Some(t);
    }

    /// Window frame from the animated height, keeping the top edge fixed.
    fn place(&self) {
        let h = self.height.x.max(BAR).round();
        let frame = NSRect::new(NSPoint::new(self.left, self.top - h), NSSize::new(self.panel_w(), h));
        self.panel.setFrame_display(frame, false);
        self.panel.invalidateShadow();
    }

    // ---- results -----------------------------------------------------------

    fn max_rows(&self) -> usize {
        self.cfg.max_results.clamp(3, 14)
    }

    fn panel_w(&self) -> f64 {
        self.cfg.width.clamp(480.0, 1200.0) as f64
    }

    fn refresh(&mut self) {
        self.qid += 1;
        self.confirm_key = None;
        self.user_moved = false;
        self.dirty = true;
        let q = self.edit.text.clone();
        if self.settings_page {
            self.mode = Mode::Settings;
            let has_recent = self.history.len() > 0;
            self.instant_rows = sources::settings(&q, &self.cfg, self.recording, self.notice.as_deref(), has_recent);
            self.file_rows.clear();
            self.merge(true);
            return;
        }
        self.mode = sources::mode(&q);
        let ctx = Ctx { apps: &self.apps, cfg: &self.cfg };
        let (items, want_files) = sources::instant(&q, &ctx, platform::running_apps);
        self.instant_rows = items;
        if self.mode == Mode::Recent {
            let mut rows = self.history.pins();
            if self.cfg.show_recent {
                for r in self.history.top(8) {
                    if !rows.iter().any(|x| x.key == r.key) {
                        rows.push(r);
                    }
                }
            }
            rows.truncate(self.max_rows());
            for (i, r) in rows.iter_mut().enumerate() {
                r.score = 1000 - i as i32;
            }
            self.instant_rows = rows;
        }
        if want_files {
            let mut fq = q.clone();
            for x in &self.cfg.exclude {
                let x = match x.strip_prefix("~/") {
                    Some(rest) => format!("{}/{rest}", heimdall_common::home().to_string_lossy()),
                    None => x.clone(),
                };
                fq.push_str(&format!(" -in:\"{}\"", x.replace('"', "")));
            }
            self.files.query(self.qid, &fq);
            // Keep rows that still match so the list doesn't flicker while typing.
            let ql = q.to_lowercase();
            let toks: Vec<&str> = ql.split_whitespace().filter(|t| !t.contains(':')).collect();
            self.file_rows.retain(|it| {
                let p = it.path.as_deref().unwrap_or("").to_lowercase();
                toks.iter().all(|t| p.contains(t))
            });
        } else {
            self.file_rows.clear();
        }
        self.merge(true);
    }

    fn merge(&mut self, reset: bool) {
        let keep = if !reset && self.user_moved { self.items.get(self.sel).map(|i| i.key.clone()) } else { None };
        let q = self.edit.text.clone();
        let mut all: Vec<Item> = self.instant_rows.iter().chain(self.file_rows.iter()).cloned().collect();
        if self.mode == Mode::Normal {
            for it in &mut all {
                it.score += self.history.boost(&q, &it.key);
            }
        }
        all.sort_by(|a, b| b.score.cmp(&a.score));
        let mut seen = HashSet::new();
        all.retain(|it| it.key.is_empty() || seen.insert(it.key.clone()));
        all.truncate(60);
        self.items = all;
        self.sel = keep
            .and_then(|k| self.items.iter().position(|i| i.key == k))
            .unwrap_or(0)
            .min(self.items.len().saturating_sub(1));
        if reset || self.sel < self.scroll {
            self.scroll = 0;
        }
        self.ensure_visible();
        self.height.target = self.target_height();
        self.sel_y.target = self.sel_target();
        if reset {
            self.sel_y.snap(self.sel_target());
        }
        self.dirty = true;
    }

    /// Pick up results from background threads.
    fn poll(&mut self) {
        if let Some(rx) = &self.apps_rx {
            if let Ok(apps) = rx.try_recv() {
                self.apps = apps;
                self.apps_rx = None;
                if self.visible && self.mode == Mode::Normal {
                    self.refresh();
                }
            }
        }
        let mut latest = None;
        while let Ok((id, r)) = self.files.rx.try_recv() {
            if id == self.qid {
                latest = Some(r);
            }
        }
        if let Some(r) = latest {
            self.file_rows = sources::file_items(&r, &self.edit.text);
            self.merge(false);
        }
        // The first index finished while a search was showing: run it again.
        let ready = self.files.ready();
        if ready && !self.files_ready && self.visible && self.mode == Mode::Normal {
            self.refresh();
        }
        if ready != self.files_ready {
            self.dirty = true;
        }
        self.files_ready = ready;
    }

    // ---- geometry ----------------------------------------------------------

    fn visible_rows(&self) -> usize {
        self.items.len().min(self.max_rows())
    }

    fn header_h(&self) -> f64 {
        if matches!(self.mode, Mode::Recent | Mode::Settings | Mode::Help) && !self.items.is_empty() {
            HEADER
        } else {
            0.0
        }
    }

    fn list_top(&self) -> f64 {
        BAR + PAD + self.header_h()
    }

    fn target_height(&self) -> f64 {
        let n = self.visible_rows();
        if n == 0 {
            BAR
        } else {
            BAR + PAD * 2.0 + self.header_h() + n as f64 * ROW
        }
    }

    fn sel_target(&self) -> f64 {
        self.list_top() + (self.sel.saturating_sub(self.scroll)) as f64 * ROW
    }

    fn ensure_visible(&mut self) {
        let n = self.max_rows();
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + n {
            self.scroll = self.sel + 1 - n;
        }
    }

    fn set_sel(&mut self, i: usize) {
        if self.items.is_empty() {
            return;
        }
        let i = i.min(self.items.len() - 1);
        if i == self.sel {
            return;
        }
        self.sel = i;
        self.user_moved = true;
        self.confirm_key = None;
        let old = self.scroll;
        self.ensure_visible();
        if old != self.scroll {
            self.sel_y.snap(self.sel_target());
        }
        self.sel_y.target = self.sel_target();
        self.dirty = true;
    }

    fn row_at(&self, x: f64, y: f64) -> Option<usize> {
        if x < 0.0 || x > self.panel_w() {
            return None;
        }
        let top = self.list_top();
        if y < top {
            return None;
        }
        let r = ((y - top) / ROW) as usize;
        (r < self.visible_rows()).then_some(self.scroll + r)
    }

    // ---- actions -----------------------------------------------------------

    fn activate(&mut self, cmd: bool) {
        let Some(it) = self.items.get(self.sel).cloned() else { return };
        if it.confirm && self.confirm_key.as_deref() != Some(it.key.as_str()) {
            self.confirm_key = Some(it.key.clone());
            self.dirty = true;
            return;
        }
        let q = self.edit.text.clone();
        let mut hide = true;
        match &it.action {
            Action::Open(target) => {
                if cmd {
                    platform::reveal(it.path.as_deref().unwrap_or(target));
                } else if !platform::open(target) {
                    return;
                }
            }
            Action::Url(u) => {
                platform::open_url(u);
            }
            Action::Copy(s) => platform::copy_text(s),
            Action::System(c) => {
                self.hide();
                platform::system(*c);
                return;
            }
            Action::Window(pid) => platform::activate(*pid as i32),
            Action::Heimdall(c) => hide = self.internal(*c),
            Action::Insert(s) => {
                self.edit.set(s);
                self.refresh();
                return;
            }
            Action::Note(text) => {
                if let Err(e) = heimdall_common::notes::append(&self.cfg, &platform::local_timestamp(), text) {
                    heimdall_common::log(format!("note: {e}"));
                    self.edit.set(&format!("Couldn't save the note: {e}"));
                    self.edit.select_all();
                    self.refresh();
                    return;
                }
            }
        }
        if !matches!(it.action, Action::Window(_) | Action::Heimdall(_) | Action::Note(_)) {
            self.history.record(&q, &it);
        }
        if hide {
            self.hide();
        }
    }

    /// Returns whether the panel should hide afterwards.
    fn internal(&mut self, cmd: Internal) -> bool {
        match cmd {
            Internal::Quit => {
                self.history.save();
                NSApplication::sharedApplication(self.mtm).terminate(None);
            }
            Internal::EditConfig => {
                let p = config::path();
                if !p.exists() {
                    config::save(&self.cfg);
                }
                // TextEdit opens .toml happily; `open -t` uses the default text editor.
                std::process::Command::new("/usr/bin/open").arg("-t").arg(&p).spawn().ok();
            }
            Internal::Settings => {
                self.open_settings();
                return false;
            }
            Internal::Set(s) => {
                self.change(s);
                return false;
            }
            Internal::ReloadIndex => self.files.rebuild(),
            Internal::ClearRecent => {
                let key = self.items.get(self.sel).map(|i| i.key.clone());
                self.history.clear();
                self.history.save();
                self.refresh();
                self.select_key(key);
                return false;
            }
            Internal::InstallIndexer | Internal::ImportFlow => {}
        }
        true
    }

    // ---- settings ----------------------------------------------------------

    pub fn open_settings(&mut self) {
        if !self.visible {
            self.show();
        }
        self.settings_page = true;
        self.notice = None;
        self.edit.clear();
        self.refresh();
    }

    fn save_config(&mut self) {
        config::save(&self.cfg);
        // Take the new timestamp now so the next open doesn't reload it.
        self.loader.poll();
    }

    fn select_key(&mut self, key: Option<String>) {
        if let Some(i) = key.and_then(|k| self.items.iter().position(|it| it.key == k)) {
            self.sel = i;
            self.ensure_visible();
            self.sel_y.snap(self.sel_target());
            self.dirty = true;
        }
    }

    fn change(&mut self, s: Setting) {
        fn cycle(v: &mut String, opts: &[&str]) {
            let i = opts.iter().position(|o| o.eq_ignore_ascii_case(v)).map_or(0, |i| (i + 1) % opts.len());
            *v = opts[i].to_string();
        }
        let key = self.items.get(self.sel).map(|i| i.key.clone());
        let c = &mut self.cfg;
        match s {
            Setting::Hotkey => {
                self.recording = true;
                self.notice = None;
                // Unregister so pressing the current hotkey arrives here as keys.
                self.set_hotkey(false);
            }
            Setting::Autostart => {
                c.start_with_windows = !c.start_with_windows;
                platform::set_autostart(c.start_with_windows);
            }
            Setting::Tray => c.tray = !c.tray,
            Setting::Theme => cycle(&mut c.theme, &["system", "dark", "light"]),
            Setting::Animations => c.animations = !c.animations,
            Setting::Recent => c.show_recent = !c.show_recent,
            Setting::ClearOnClose => c.clear_on_close = !c.clear_on_close,
            Setting::Glass => cycle(&mut c.glass, &["liquid", "auto", "solid"]),
            Setting::Shadow | Setting::Corners | Setting::GameMode => {}
        }
        if s != Setting::Hotkey {
            self.save_config();
            self.sync_tray();
        }
        self.apply_look();
        self.refresh();
        self.select_key(key);
    }

    /// Theme and glass from the config. "liquid" is Apple's clear glass under
    /// the same tint as the Windows panel, "auto" the standard glass, and
    /// "solid" is opaque.
    fn apply_look(&mut self) {
        platform::set_theme(&self.panel, &self.cfg.theme);
        self.theme = Theme::new(platform::is_dark(&self.panel));
        if let Some(g) = &self.liquid {
            g.setStyle(if self.cfg.glass == "auto" { NSGlassEffectViewStyle::Regular } else { NSGlassEffectViewStyle::Clear });
        }
    }

    /// A key arrived while recording a new hotkey.
    fn record_key(&mut self, k: &Key) {
        if hotkey::is_modifier(k.code) {
            return;
        }
        let key = self.items.get(self.sel).map(|i| i.key.clone());
        let mods = k.flags & (SHIFT | CONTROL | OPTION | COMMAND);
        if k.code == 53 && mods == 0 {
            self.recording = false;
            self.notice = None;
            self.set_hotkey(true);
        } else if let Some(name) = hotkey::name(mods, k.code) {
            self.recording = false;
            let old = std::mem::replace(&mut self.cfg.hotkey, name.clone());
            self.set_hotkey(true);
            if self.hotkey_ok {
                self.notice = None;
                self.save_config();
            } else {
                self.cfg.hotkey = old;
                self.set_hotkey(true);
                self.notice = Some(format!("{} is already used by another app. Press Enter to try another.", sources::display_hotkey(&name)));
            }
        } else {
            self.notice = Some("Add \u{2303}, \u{2325}, \u{21E7} or \u{2318} to that key.".into());
        }
        self.refresh();
        self.select_key(key);
    }

    // ---- right-click menu --------------------------------------------------

    pub fn context_menu(&mut self, x: f64, y: f64) {
        let Some(r) = self.row_at(x, y) else { return };
        self.set_sel(r);
        self.render();
        let Some(it) = self.items.get(self.sel).cloned() else { return };
        let Action::Open(_) = &it.action else { return };
        let mut acts: Vec<(Act, String)> = vec![(Act::Open, "Open".into())];
        if it.path.is_some() {
            acts.push((Act::Reveal, "Show in Finder".into()));
        }
        if !it.key.is_empty() {
            acts.push((Act::Pin, if self.history.pinned(&it.key) { "Unpin".into() } else { "Pin to Top".into() }));
        }
        if it.path.is_some() {
            acts.push((Act::CopyPath, "Copy Path".into()));
        }
        let menu = NSMenu::new(self.mtm);
        for (i, (_, title)) in acts.iter().enumerate() {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(self.mtm),
                    &NSString::from_str(title),
                    Some(objc2::sel!(pick:)),
                    &NSString::new(),
                )
            };
            item.setTag(i as isize + 1);
            unsafe { item.setTarget(Some(&super::menu_target(self.mtm))) };
            menu.addItem(&item);
        }
        CHOSEN.set(0);
        menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(x, y), Some(&self.view));
        let chosen = CHOSEN.replace(0);
        if let Some((act, _)) = acts.get((chosen as usize).wrapping_sub(1)) {
            match act {
                Act::Open => self.activate(false),
                Act::Reveal => {
                    if let Some(p) = &it.path {
                        platform::reveal(p);
                        self.hide();
                    }
                }
                Act::Pin => {
                    self.history.toggle_pin(&it);
                    self.history.save();
                    if self.mode == Mode::Recent {
                        self.refresh();
                    }
                }
                Act::CopyPath => {
                    if let Some(p) = &it.path {
                        platform::copy_text(p);
                    }
                }
            }
        }
        // The menu took key focus away for a moment; take it back.
        if self.visible {
            self.panel.makeKeyAndOrderFront(None);
        }
    }

    // ---- input -------------------------------------------------------------

    /// Returns false for keys Heimdall doesn't use.
    pub fn key_down(&mut self, k: &Key) -> bool {
        if self.recording {
            self.record_key(k);
            return true;
        }
        let cmd = k.flags & COMMAND != 0;
        let opt = k.flags & OPTION != 0;
        let shift = k.flags & SHIFT != 0;
        let before = self.edit.text.clone();
        match k.code {
            // Esc
            53 => {
                if !self.edit.text.is_empty() && !self.edit.all {
                    self.edit.clear();
                } else if self.settings_page {
                    self.settings_page = false;
                    self.notice = None;
                    self.edit.clear();
                    self.refresh();
                    return true;
                } else {
                    self.hide();
                    return true;
                }
            }
            126 => self.set_sel(self.sel.saturating_sub(1)),
            125 => self.set_sel(self.sel + 1),
            116 => self.set_sel(self.sel.saturating_sub(self.max_rows())),
            121 => self.set_sel(self.sel + self.max_rows()),
            36 | 76 => {
                self.activate(cmd);
                return true;
            }
            48 => {
                if let Some(c) = self.items.get(self.sel).and_then(sources::completion) {
                    self.edit.set(&c);
                }
            }
            // Delete
            51 if cmd && shift => {
                if let Some(it) = self.items.get(self.sel) {
                    let key = it.key.clone();
                    self.history.forget(&key);
                    self.refresh();
                }
                return true;
            }
            51 if cmd => self.edit.clear(),
            51 => self.edit.backspace(opt),
            117 => self.edit.delete(opt),
            123 if cmd => self.edit.home(),
            124 if cmd => self.edit.end(),
            123 => self.edit.left(opt),
            124 => self.edit.right(opt),
            115 => self.set_sel(0),
            119 => self.set_sel(self.items.len().saturating_sub(1)),
            _ if cmd => match k.chars.to_ascii_lowercase().as_str() {
                "a" => self.edit.select_all(),
                "v" => {
                    if let Some(t) = platform::clipboard_text() {
                        self.edit.insert(t.lines().next().unwrap_or(""));
                    }
                }
                "c" => {
                    if let Some(it) = self.items.get(self.sel) {
                        let s = match &it.action {
                            Action::Copy(s) => s.clone(),
                            _ => it.path.clone().unwrap_or_else(|| it.title.clone()),
                        };
                        platform::copy_text(&s);
                    }
                    return true;
                }
                "," => {
                    self.open_settings();
                    return true;
                }
                "w" => {
                    self.hide();
                    return true;
                }
                _ => return false,
            },
            _ if k.flags & CONTROL != 0 => return false,
            _ => {
                let text: String = k.chars.chars().filter(|c| !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(c)).collect();
                if text.is_empty() {
                    return false;
                }
                self.edit.insert(&text);
            }
        }
        self.caret_at = Instant::now();
        if self.edit.text != before {
            self.refresh();
        } else {
            self.dirty = true;
        }
        true
    }

    pub fn mouse_move(&mut self, x: f64, y: f64) {
        if !self.mouse_armed {
            self.mouse_armed = true;
            return;
        }
        if let Some(r) = self.row_at(x, y) {
            self.set_sel(r);
        }
    }

    pub fn mouse_down(&mut self, x: f64, y: f64) {
        if let Some(r) = self.row_at(x, y) {
            self.set_sel(r);
            self.press = Some(r);
        }
    }

    pub fn mouse_up(&mut self, x: f64, y: f64) {
        if let Some(row) = self.press.take() {
            if self.row_at(x, y) == Some(row) {
                self.activate(false);
            }
        }
    }

    pub fn scroll(&mut self, dy: f64, precise: bool) {
        // Trackpads report points; one row per ~row height of travel.
        self.scroll_acc -= if precise { dy / 24.0 } else { dy };
        while self.scroll_acc >= 1.0 {
            self.scroll_acc -= 1.0;
            self.set_sel(self.sel + 1);
        }
        while self.scroll_acc <= -1.0 {
            self.scroll_acc += 1.0;
            self.set_sel(self.sel.saturating_sub(1));
        }
    }

    // ---- frame loop --------------------------------------------------------

    pub fn tick(&mut self) {
        if !self.visible {
            return;
        }
        self.poll();
        let now = Instant::now();
        let dt = self.anim_last.map(|t| (now - t).as_secs_f64()).unwrap_or(1.0 / 120.0).min(1.0 / 30.0);
        self.anim_last = Some(now);
        let animate = self.cfg.animations;
        let mut moving = false;
        for s in [&mut self.height, &mut self.sel_y, &mut self.opacity] {
            if !animate {
                s.snap(s.target);
            }
            moving |= s.step(dt);
        }
        let caret = (self.caret_at.elapsed().as_millis() / 530) % 2 == 0;
        if caret != self.caret_was {
            self.caret_was = caret;
            self.dirty = true;
        }
        if moving {
            self.place();
            self.panel.setAlphaValue(self.opacity.x.clamp(0.0, 1.0));
            self.dirty = true;
        }
        if self.dirty {
            self.render();
        }
        if self.shadow_pending {
            self.shadow_pending = false;
            self.panel.displayIfNeeded();
            self.panel.invalidateShadow();
        }
    }

    fn render(&mut self) {
        self.dirty = false;
        let th = &self.theme;
        let w = self.panel_w();
        let h = self.height.x.max(BAR);
        let mut out = Vec::with_capacity(64);
        let text = |out: &mut Vec<Cmd>, s: &str, r: Rect, size: f64, weight: f64, color: Rgba| {
            out.push(Cmd::Text(Text { text: s.to_string(), r, size, weight, color, hl: Vec::new(), hl_color: color, right: false }));
        };

        // A light veil over the system glass, and a rim like the Windows panel's.
        // The Windows shader's tints, over the glass or as the solid panel.
        let veil = match (th.dark, self.cfg.glass != "solid") {
            (true, true) => (0.06, 0.065, 0.08, 0.30),
            (true, false) => (0.115, 0.12, 0.135, 1.0),
            (false, true) => (0.985, 0.985, 0.99, 0.12),
            (false, false) => (0.955, 0.957, 0.965, 1.0),
        };
        out.push(Cmd::Fill { r: Rect::new(0.0, 0.0, w, h), radius: RADIUS, color: veil });
        let rim = if th.dark { (1.0, 1.0, 1.0, 0.14) } else { (1.0, 1.0, 1.0, 0.55) };
        out.push(Cmd::Stroke { r: Rect::new(0.0, 0.0, w, h), radius: RADIUS, color: rim, width: 1.0 });

        // Search bar.
        out.push(Cmd::Symbol { name: "magnifyingglass", r: Rect::new(14.0, 0.0, 30.0, BAR), size: 17.0, color: th.faint });
        let tx = 52.0;
        let tw = w - 52.0 - 110.0;
        let caret_on = self.caret_was;
        let caret = |out: &mut Vec<Cmd>, x: f64| {
            out.push(Cmd::Fill { r: Rect::new(x, BAR / 2.0 - 12.0, 1.5, 24.0), radius: 0.0, color: th.accent });
        };
        if self.edit.text.is_empty() {
            let placeholder = if self.settings_page { "Filter settings" } else { "Search apps, files and settings" };
            text(&mut out, placeholder, Rect::new(tx, 0.0, tw, BAR), 21.0, REGULAR, th.faint);
            if caret_on {
                caret(&mut out, tx);
            }
        } else {
            if self.edit.all {
                let lw = platform::text_width(&self.edit.text, 21.0, REGULAR).min(tw);
                out.push(Cmd::Fill { r: Rect::new(tx - 2.0, BAR / 2.0 - 14.0, lw + 4.0, 28.0), radius: 5.0, color: alpha(th.accent, 0.32) });
            }
            text(&mut out, &self.edit.text, Rect::new(tx, 0.0, tw, BAR), 21.0, REGULAR, th.text);
            if caret_on && !self.edit.all {
                let before: String = self.edit.text.chars().take(self.edit.caret).collect();
                let cx = platform::text_width(&before, 21.0, REGULAR).min(tw);
                caret(&mut out, tx + cx);
            }
        }
        let status = match self.mode {
            Mode::Windows => "Apps",
            Mode::Path => "Browse",
            Mode::Settings => "Settings",
            Mode::Help => "Tips",
            _ if !self.files.ready() => "Indexing\u{2026}",
            Mode::Recent => "? for tips",
            _ => "",
        };
        out.push(Cmd::Text(Text {
            text: status.into(),
            r: Rect::new(w - 130.0, 0.0, 112.0, BAR),
            size: 11.5,
            weight: REGULAR,
            color: th.faint,
            hl: Vec::new(),
            hl_color: th.faint,
            right: true,
        }));

        let n = self.visible_rows();
        if n > 0 {
            out.push(Cmd::Fill { r: Rect::new(16.0, BAR, w - 32.0, 1.0), radius: 0.0, color: th.line });
            if self.header_h() > 0.0 {
                let pinned = self.items.iter().filter(|i| self.history.pinned(&i.key)).count();
                let header = match self.mode {
                    Mode::Settings => "SETTINGS",
                    Mode::Help => "TIPS",
                    _ if pinned == self.items.len() => "PINNED",
                    _ if pinned > 0 => "PINNED AND RECENT",
                    _ => "RECENT",
                };
                text(&mut out, header, Rect::new(22.0, BAR + PAD, 200.0, HEADER), 11.0, SEMIBOLD, th.faint);
            }
            // The selection: a soft pill that glides between rows.
            if !self.items.is_empty() {
                let sel = Rect::new(8.0, self.sel_y.x, w - 16.0, ROW);
                let (fill, edge) = if th.dark {
                    ((1.0, 1.0, 1.0, 0.085), (1.0, 1.0, 1.0, 0.12))
                } else {
                    ((0.0, 0.0, 0.0, 0.055), (1.0, 1.0, 1.0, 0.7))
                };
                out.push(Cmd::Fill { r: sel, radius: 12.0, color: fill });
                out.push(Cmd::Stroke { r: sel, radius: 12.0, color: edge, width: 1.0 });
            }
            let top = self.list_top();
            for vi in 0..n {
                let i = self.scroll + vi;
                let Some(it) = self.items.get(i) else { break };
                self.draw_row(&mut out, it, i == self.sel, top + vi as f64 * ROW, w);
            }
        }
        draw::submit(out);
        self.view.setNeedsDisplay(true);
    }

    fn draw_row(&self, out: &mut Vec<Cmd>, it: &Item, selected: bool, y: f64, w: f64) {
        let th = &self.theme;
        let ix = 20.0;
        match &it.icon {
            Icon::Glyph(c) => out.push(Cmd::Symbol {
                name: sources::symbol_name(*c),
                r: Rect::new(ix, y, ICON, ROW),
                size: 18.0,
                color: if selected { th.text } else { th.sub },
            }),
            Icon::Shell(path) => out.push(Cmd::Icon { path: path.clone(), r: Rect::new(ix, y + (ROW - ICON) / 2.0, ICON, ICON) }),
            Icon::None => {}
        }
        let x = 64.0;
        let hint_w = 120.0;
        let title_w = w - 64.0 - 18.0 - hint_w;
        let confirming = self.confirm_key.as_deref() == Some(it.key.as_str()) && it.confirm;
        let subtitle = if confirming { format!("Press Enter again to {}", it.title.to_lowercase()) } else { it.subtitle.clone() };
        let has_sub = !subtitle.is_empty();
        let (title_y, title_h) = if has_sub { (y + 5.0, 22.0) } else { (y, ROW) };
        // Matched letters go bold and full strength; the rest steps back a little.
        let base = if it.hl.is_empty() { th.text } else { alpha(th.text, 0.80) };
        out.push(Cmd::Text(Text {
            text: it.title.clone(),
            r: Rect::new(x, title_y, title_w, title_h),
            size: 14.0,
            weight: REGULAR,
            color: base,
            hl: it.hl.clone(),
            hl_color: th.hl,
            right: false,
        }));
        if has_sub {
            let c = if confirming { th.accent } else { th.sub };
            out.push(Cmd::Text(Text { text: subtitle, r: Rect::new(x, y + 25.0, title_w, 17.0), size: 11.5, weight: REGULAR, color: c, hl: Vec::new(), hl_color: c, right: false }));
        }
        let (hint, colour) = match (it.value.is_empty(), selected) {
            (false, true) => (format!("{}  \u{21A9}", it.value), th.text),
            (false, false) => (it.value.clone(), th.sub),
            (true, true) => (format!("{}  \u{21A9}", it.verb()), th.sub),
            (true, false) => (label(it.kind).to_string(), th.faint),
        };
        out.push(Cmd::Text(Text { text: hint, r: Rect::new(w - 18.0 - hint_w, y, hint_w, ROW), size: 11.5, weight: REGULAR, color: colour, hl: Vec::new(), hl_color: colour, right: true }));
    }
}

/// Kind labels, with Mac words where Windows ones would read oddly.
fn label(k: Kind) -> &'static str {
    match k {
        Kind::Window => "Running",
        Kind::WinSetting => "Settings",
        k => k.label(),
    }
}

