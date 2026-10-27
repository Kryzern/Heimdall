//! The launcher window: state, input, layout, animation and drawing.

use crate::actions;
use crate::apps::App as AppEntry;
use crate::config::{self, Config};
use crate::edit::Edit;
use crate::files::FileSearch;
use crate::gfx::{caret_x, layout_width, Color, GlassParams, Gfx, Rect};
use crate::history::History;
use crate::items::*;
use crate::menu::{self, Menu, Tray};
use crate::platform;
use crate::shell::{Done, Job, Preview, Worker};
use crate::sources::{self, Ctx, Mode};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_SHOW: u32 = WM_APP + 2;
const TIMER_CARET: usize = 2;
const HOTKEY_ID: i32 = 1;

// Layout, in DIPs.
const M: f32 = 40.0; // margin around the panel: room for the shadow and refraction
const BAR: f32 = 60.0;
const ROW: f32 = 48.0;
const PAD: f32 = 6.0;
const HEADER: f32 = 26.0;
const PREVIEW: f32 = 340.0;
const RADIUS: f32 = 22.0;
const ICON: f32 = 30.0;
const MENU_W: f32 = 268.0;
const MENU_ROW: f32 = 34.0;
const MENU_SEP: f32 = 9.0;
const MENU_PAD: f32 = 6.0;

const G_OPEN: char = '\u{E8A7}';
const G_ADMIN: char = '\u{E7EF}';
const G_FOLDER: char = '\u{E838}';
const G_OPEN_WITH: char = '\u{E7AC}';
const G_COPY: char = '\u{E8C8}';
const G_INFO: char = '\u{E946}';
const G_CLOSE: char = '\u{E8BB}';

/// Things the action menu can do to a result.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Open,
    Admin,
    Location,
    OpenWith,
    CopyPath,
    Properties,
    Close,
}

/// (action, glyph, label, shortcut hint)
type MenuRow = (Act, char, &'static str, &'static str);

/// The right-click menu. `None` rows are separators.
struct ActionMenu {
    rows: Vec<Option<MenuRow>>,
    sel: usize,
    x: f32,
    y: f32,
    item: Item,
}

fn menu_height(rows: &[Option<MenuRow>]) -> f32 {
    2.0 * MENU_PAD + rows.iter().map(|r| if r.is_some() { MENU_ROW } else { MENU_SEP }).sum::<f32>()
}

struct Spring {
    x: f32,
    v: f32,
    target: f32,
    k: f32,
    zeta: f32,
}

impl Spring {
    fn new(x: f32, k: f32, zeta: f32) -> Spring {
        Spring { x, v: 0.0, target: x, k, zeta }
    }

    fn snap(&mut self, x: f32) {
        self.x = x;
        self.target = x;
        self.v = 0.0;
    }

    /// Advance; returns true while still moving.
    fn step(&mut self, dt: f32) -> bool {
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
    accent: Color,
    glass: bool,
    text: Color,
    sub: Color,
    faint: Color,
    line: Color,
    hl: Color,
}

impl Theme {
    fn new(cfg: &Config) -> Theme {
        let dark = match cfg.theme.as_str() {
            "dark" => true,
            "light" => false,
            _ => platform::system_dark(),
        };
        let accent = platform::parse_hex(&cfg.accent).unwrap_or_else(platform::system_accent);
        // "liquid" always renders glass; "auto" follows Windows' transparency setting.
        let glass = match cfg.glass.as_str() {
            "solid" => false,
            "auto" => platform::transparency_enabled(),
            _ => true,
        } && !platform::battery_saver()
            && !platform::remote_session();
        let mix = |a: Color, b: Color, t: f32| {
            Color::rgba(a.r + (b.r - a.r) * t, a.g + (b.g - a.g) * t, a.b + (b.b - a.b) * t, 1.0)
        };
        if dark {
            Theme {
                dark,
                accent,
                glass,
                text: Color::rgba(1.0, 1.0, 1.0, 0.94),
                sub: Color::rgba(1.0, 1.0, 1.0, 0.56),
                faint: Color::rgba(1.0, 1.0, 1.0, 0.36),
                line: Color::rgba(1.0, 1.0, 1.0, 0.09),
                hl: if colourful(accent) { mix(accent, Color::rgba(1.0, 1.0, 1.0, 1.0), 0.45) } else { Color::rgba(1.0, 1.0, 1.0, 1.0) },
            }
        } else {
            Theme {
                dark,
                accent,
                glass,
                text: Color::rgba(0.05, 0.05, 0.07, 0.92),
                sub: Color::rgba(0.0, 0.0, 0.02, 0.54),
                faint: Color::rgba(0.0, 0.0, 0.02, 0.38),
                line: Color::rgba(0.0, 0.0, 0.0, 0.08),
                hl: if colourful(accent) { mix(accent, Color::rgba(0.0, 0.0, 0.0, 1.0), 0.2) } else { Color::rgba(0.0, 0.0, 0.0, 1.0) },
            }
        }
    }
}

/// A grey accent would make highlights vanish; fall back to pure contrast.
fn colourful(c: Color) -> bool {
    let hi = c.r.max(c.g).max(c.b);
    let lo = c.r.min(c.g).min(c.b);
    hi - lo > 0.12
}

struct PreviewView {
    data: Box<Preview>,
    bmp: Option<(ID2D1Bitmap1, u32, u32)>,
}

pub struct App {
    hwnd: HWND,
    gfx: Option<Gfx>,
    cfg: Config,
    loader: config::Loader,
    theme: Theme,
    hotkey_registered: bool,
    hotkey_wanted: bool,
    pub visible: bool,
    scale: f32,
    win: (i32, i32, u32, u32),

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
    files_pending: bool,

    apps: Vec<AppEntry>,
    apps_at: Option<Instant>,
    recent: Vec<String>,
    history: History,
    files: FileSearch,
    fast: Worker,
    slow: Worker,
    done: Receiver<Done>,
    icons: HashMap<String, Option<ID2D1Bitmap1>>,
    icon_pending: HashSet<String>,

    preview_on: bool,
    preview: Option<PreviewView>,
    preview_req: Option<String>,

    height: Spring,
    sel_y: Spring,
    opacity: Spring,
    zoom: Spring,
    pw: Spring,
    anim_last: Option<Instant>,
    frame_pending: bool,
    caret_on: bool,
    eat_char: bool,

    mouse_last: (i32, i32),
    press: Option<(i32, i32, usize)>,
    dragging: bool,

    /// Showing Heimdall's settings page instead of search results.
    settings_page: bool,
    /// Waiting for the user to press a new hotkey.
    recording: bool,
    /// Shown under the hotkey row (e.g. "Ctrl+K is already used").
    notice: Option<String>,
    tray: Tray,
    menu: Option<ActionMenu>,
    menu_fade: Spring,
}

fn icon_key(p: &str) -> String {
    if p.starts_with("shell:") {
        return p.to_string();
    }
    let lower = p.to_lowercase();
    match std::path::Path::new(&lower).extension().and_then(|e| e.to_str()) {
        Some(ext) if !matches!(ext, "exe" | "lnk" | "ico" | "url" | "appref-ms" | "msc" | "cpl" | "scr") => format!(".{ext}"),
        _ => lower,
    }
}

impl App {
    pub fn new(hwnd: HWND) -> App {
        let mut loader = config::Loader::new();
        let cfg = loader.poll().unwrap_or_default();
        let theme = Theme::new(&cfg);
        let (dtx, done) = channel();
        let fast = Worker::spawn("icons", hwnd, dtx.clone());
        let slow = Worker::spawn("shell", hwnd, dtx);
        slow.send(Job::Apps);
        slow.send(Job::Recent);
        platform::set_autostart(cfg.start_with_windows);
        let gfx = match Gfx::new(hwnd, 64, 64) {
            Ok(g) => Some(g),
            Err(e) => {
                crate::log(format!("graphics init failed: {e}"));
                None
            }
        };
        let mut app = App {
            hwnd,
            gfx,
            theme,
            hotkey_registered: false,
            hotkey_wanted: true,
            visible: false,
            scale: 1.0,
            win: (0, 0, 0, 0),
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
            files_pending: false,
            apps: Vec::new(),
            apps_at: Some(Instant::now()),
            recent: Vec::new(),
            history: History::load(),
            files: FileSearch::start(hwnd),
            fast,
            slow,
            done,
            icons: HashMap::new(),
            icon_pending: HashSet::new(),
            preview_on: false,
            preview: None,
            preview_req: None,
            height: Spring::new(BAR, 420.0, 0.88),
            sel_y: Spring::new(0.0, 560.0, 0.80),
            opacity: Spring::new(0.0, 320.0, 1.0),
            zoom: Spring::new(1.0, 340.0, 0.70),
            pw: Spring::new(0.0, 420.0, 0.9),
            anim_last: None,
            frame_pending: false,
            caret_on: true,
            eat_char: false,
            mouse_last: (i32::MIN, i32::MIN),
            press: None,
            dragging: false,
            settings_page: false,
            recording: false,
            notice: None,
            tray: Tray::new(hwnd),
            menu: None,
            menu_fade: Spring::new(0.0, 900.0, 1.0),
            cfg,
            loader,
        };
        app.set_hotkey(true);
        app.sync_tray();
        app
    }

    // ---- hotkey ------------------------------------------------------------

    fn set_hotkey(&mut self, on: bool) {
        self.hotkey_wanted = on;
        if on == self.hotkey_registered {
            return;
        }
        unsafe {
            if on {
                match config::parse_hotkey(&self.cfg.hotkey) {
                    Some((mods, vk)) => {
                        let ok = RegisterHotKey(Some(self.hwnd), HOTKEY_ID, HOT_KEY_MODIFIERS(mods | MOD_NOREPEAT.0), vk);
                        if ok.is_err() {
                            crate::log(format!("could not register hotkey {} (in use by another app?)", self.cfg.hotkey));
                        }
                        self.hotkey_registered = ok.is_ok();
                    }
                    None => crate::log(format!("unrecognised hotkey {:?}", self.cfg.hotkey)),
                }
            } else {
                UnregisterHotKey(Some(self.hwnd), HOTKEY_ID).ok();
                self.hotkey_registered = false;
            }
        }
    }

    fn sync_tray(&mut self) {
        let tip = format!("Heimdall ({})", self.cfg.hotkey);
        self.tray.set(self.cfg.tray, &tip);
    }

    /// Remove the tray icon before exiting so it doesn't linger until hovered.
    pub fn shutdown(&mut self) {
        self.hide();
        self.tray.set(false, "");
    }

    fn rebind_hotkey(&mut self) {
        let wanted = self.hotkey_wanted;
        self.set_hotkey(false);
        self.set_hotkey(wanted);
    }

    /// Foreground window changed: release the hotkey in games and passthrough apps.
    pub fn on_foreground(&mut self, fg: HWND) {
        if fg == self.hwnd || self.visible {
            return;
        }
        let exe = platform::process_path(fg)
            .map(|p| platform::file_name(&p).to_ascii_lowercase())
            .unwrap_or_default();
        let pass = self.cfg.passthrough.iter().any(|p| p.eq_ignore_ascii_case(&exe));
        let game = self.cfg.game_mode && !exe.eq_ignore_ascii_case("explorer.exe") && platform::foreground_fullscreen(fg);
        self.set_hotkey(!(pass || game));
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
                self.rebind_hotkey();
            }
            self.sync_tray();
        }
        self.theme = Theme::new(&self.cfg);
        if self.gfx.is_none() {
            self.gfx = Gfx::new(self.hwnd, 64, 64).ok();
        }

        let mon = platform::active_monitor();
        self.scale = mon.dpi as f32 / 96.0;
        let s = self.scale;
        let w = self.cfg.width.clamp(480.0, 1200.0);
        let rows = self.cfg.max_results.clamp(3, 14) as f32;
        let ww = ((w + PREVIEW + 2.0 * M) * s).ceil() as i32;
        let wh = ((BAR + 2.0 * PAD + HEADER + ROW * rows + 2.0 * M) * s).ceil() as i32;
        let work = mon.work;
        let x = work.left + ((work.right - work.left) - ((w + 2.0 * M) * s) as i32) / 2;
        let y = work.top + ((work.bottom - work.top) as f32 * 0.17) as i32 - (M * s) as i32;
        self.win = (x, y, ww as u32, wh as u32);

        // Capture what is behind us while we are still hidden.
        let backdrop = if self.theme.glass { platform::capture(x, y, ww, wh) } else { None };
        if let Some(g) = self.gfx.as_mut() {
            g.set_backdrop(backdrop.as_deref().map(|px| (px, ww as u32, wh as u32)));
            g.resize(ww as u32, wh as u32).ok();
        }
        unsafe {
            SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, ww, wh, SWP_NOACTIVATE).ok();
        }

        // Results that arrived while we were hidden or busy.
        self.frame_pending = false;
        self.on_shell();
        self.on_files();
        self.edit.select_all();
        self.preview_on = false;
        self.pw.snap(0.0);
        self.confirm_key = None;
        self.mouse_last = (i32::MIN, i32::MIN);
        self.refresh();
        self.height.snap(self.target_height());
        self.sel_y.snap(self.sel_target());
        self.opacity.snap(0.0);
        self.opacity.target = 1.0;
        self.zoom.snap(0.965);
        self.zoom.target = 1.0;
        self.anim_last = None;
        self.render();

        self.visible = true;
        unsafe {
            ShowWindow(self.hwnd, SW_SHOW).ok().ok();
            SetForegroundWindow(self.hwnd).ok().ok();
            windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.hwnd)).ok();
            SetTimer(Some(self.hwnd), TIMER_CARET, 530, None);
        }
        self.caret_on = true;
        if self.apps_at.is_none_or(|t| t.elapsed().as_secs() > 600) {
            self.apps_at = Some(Instant::now());
            self.slow.send(Job::Apps);
        }
        self.slow.send(Job::Recent);
        self.kick();
    }

    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE).ok().ok();
            KillTimer(Some(self.hwnd), TIMER_CARET).ok();
        }
        if let Some(g) = self.gfx.as_mut() {
            g.set_backdrop(None);
        }
        self.preview = None;
        self.preview_req = None;
        self.history.save();
        self.settings_page = false;
        self.recording = false;
        self.notice = None;
        self.menu = None;
        if !self.hotkey_wanted {
            self.set_hotkey(true);
        }
    }

    // ---- results -----------------------------------------------------------

    fn max_rows(&self) -> usize {
        self.cfg.max_results.clamp(3, 14)
    }

    fn refresh(&mut self) {
        self.qid += 1;
        self.menu = None;
        self.confirm_key = None;
        self.user_moved = false;
        let q = self.edit.text.clone();
        if self.settings_page {
            self.mode = Mode::Settings;
            self.instant_rows = sources::settings(&q, &self.cfg, self.recording, self.notice.as_deref());
            self.file_rows.clear();
            self.files_pending = false;
            self.merge(true);
            return;
        }
        self.mode = sources::mode(&q);
        let ctx = Ctx { apps: &self.apps, cfg: &self.cfg, service: self.files.status.service.load(Ordering::Relaxed) };
        let (items, want_files) = sources::instant(&q, &ctx, self.hwnd);
        self.instant_rows = items;
        if self.mode == Mode::Recent && self.cfg.show_recent {
            let mut rows = self.history.top(5);
            for r in sources::recent_items(&self.recent) {
                if !rows.iter().any(|x| x.key == r.key) {
                    rows.push(r);
                }
            }
            rows.truncate(self.max_rows());
            for (i, r) in rows.iter_mut().enumerate() {
                r.score = 1000 - i as i32;
            }
            self.instant_rows = rows;
        }
        if want_files {
            self.files.query(self.qid, &q);
            self.files_pending = true;
            // Keep rows that still match so the list doesn't flicker while typing.
            let ql = q.to_lowercase();
            let toks: Vec<&str> = ql.split_whitespace().filter(|t| !t.contains(':')).collect();
            self.file_rows.retain(|it| {
                let p = it.path.as_deref().unwrap_or("").to_lowercase();
                toks.iter().all(|t| p.contains(t))
            });
        } else {
            self.file_rows.clear();
            self.files_pending = false;
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
        self.load_preview();
        self.kick();
    }

    fn on_files(&mut self) {
        let mut latest = None;
        while let Ok((id, r)) = self.files.rx.try_recv() {
            if id == self.qid {
                latest = Some(r);
            }
        }
        if let Some(r) = latest {
            self.files_pending = false;
            self.file_rows = sources::file_items(&r, &self.edit.text);
            self.merge(false);
        }
    }

    fn on_shell(&mut self) {
        while let Ok(d) = self.done.try_recv() {
            match d {
                Done::Icon { key, img } => {
                    self.icon_pending.remove(&key);
                    let bmp = img.and_then(|i| self.gfx.as_ref()?.create_bitmap(&i.px, i.w, i.h));
                    self.icons.insert(key, bmp);
                    self.kick();
                }
                Done::Preview(p) => {
                    if self.preview_req.as_deref() != Some(p.path.as_str()) {
                        continue;
                    }
                    let bmp = p
                        .image
                        .as_ref()
                        .and_then(|i| Some((self.gfx.as_ref()?.create_bitmap(&i.px, i.w, i.h)?, i.w, i.h)));
                    self.preview = Some(PreviewView { data: p, bmp });
                    self.kick();
                }
                Done::Apps(apps) => {
                    if apps.is_empty() {
                        crate::log("apps: none found");
                    }
                    self.apps = apps;
                    if self.visible && self.mode == Mode::Normal {
                        let q = self.edit.text.clone();
                        let ctx = Ctx { apps: &self.apps, cfg: &self.cfg, service: self.files.status.service.load(Ordering::Relaxed) };
                        self.instant_rows = sources::instant(&q, &ctx, self.hwnd).0;
                        self.merge(false);
                    }
                }
                Done::Recent(r) => {
                    let changed = r != self.recent;
                    self.recent = r;
                    if changed && self.visible && self.mode == Mode::Recent {
                        self.refresh();
                    }
                }
            }
        }
    }

    // ---- geometry ----------------------------------------------------------

    fn panel_w(&self) -> f32 {
        self.cfg.width.clamp(480.0, 1200.0)
    }

    fn visible_rows(&self) -> usize {
        self.items.len().min(self.max_rows())
    }

    fn header_h(&self) -> f32 {
        if matches!(self.mode, Mode::Recent | Mode::Settings | Mode::Help) && !self.items.is_empty() {
            HEADER
        } else {
            0.0
        }
    }

    fn list_top(&self) -> f32 {
        M + BAR + PAD + self.header_h()
    }

    fn target_height(&self) -> f32 {
        let n = self.visible_rows();
        if n == 0 {
            BAR
        } else {
            BAR + PAD * 2.0 + self.header_h() + n as f32 * ROW
        }
    }

    fn sel_target(&self) -> f32 {
        self.list_top() + (self.sel.saturating_sub(self.scroll)) as f32 * ROW
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
        let old_scroll = self.scroll;
        self.ensure_visible();
        if old_scroll != self.scroll {
            self.sel_y.snap(self.sel_target());
        }
        self.sel_y.target = self.sel_target();
        self.load_preview();
        self.kick();
    }

    fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        if x < M || x > M + self.panel_w() {
            return None;
        }
        let top = self.list_top();
        if y < top {
            return None;
        }
        let r = ((y - top) / ROW) as usize;
        (r < self.visible_rows()).then_some(self.scroll + r)
    }

    fn shape_rect(&self) -> Rect {
        Rect::new(M, M, self.panel_w() + self.pw.x.max(0.0), self.height.x)
    }

    // ---- preview -----------------------------------------------------------

    fn load_preview(&mut self) {
        if !self.preview_on {
            return;
        }
        let path = self.items.get(self.sel).and_then(|i| i.path.clone());
        if path == self.preview_req {
            return;
        }
        self.preview = None;
        self.preview_req = path.clone();
        if let Some(p) = path {
            let px = ((PREVIEW - 40.0) * self.scale) as u32;
            self.fast.send(Job::Preview { path: p, px });
        }
    }

    fn toggle_preview(&mut self) {
        self.preview_on = !self.preview_on;
        self.pw.target = if self.preview_on { PREVIEW } else { 0.0 };
        if self.preview_on {
            self.preview_req = None;
            self.load_preview();
        }
        self.kick();
    }

    // ---- actions -----------------------------------------------------------

    fn activate(&mut self, ctrl: bool, shift: bool) {
        let Some(it) = self.items.get(self.sel).cloned() else { return };
        if it.confirm && self.confirm_key.as_deref() != Some(it.key.as_str()) {
            self.confirm_key = Some(it.key.clone());
            self.kick();
            return;
        }
        let q = self.edit.text.clone();
        let mut hide = true;
        match &it.action {
            Action::Open(target) => {
                if ctrl && shift {
                    let t = it.path.as_deref().unwrap_or(target);
                    actions::open(t, true);
                } else if ctrl {
                    if let Some(p) = it.path.as_deref() {
                        actions::reveal(p);
                    }
                } else if !actions::open(target, false) {
                    return;
                }
            }
            Action::Url(u) => {
                actions::open(u, false);
            }
            Action::Copy(s) => actions::copy_text(self.hwnd, s),
            Action::System(c) => {
                self.hide();
                actions::system(*c, self.hwnd);
                return;
            }
            Action::Window(h) => actions::switch_to(*h),
            Action::Heimdall(cmd) => hide = self.internal(*cmd),
            Action::Insert(s) => {
                self.edit.set(s);
                self.refresh();
                return;
            }
        }
        if !matches!(it.action, Action::Window(_) | Action::Heimdall(_)) {
            self.history.record(&q, &it);
        }
        if hide {
            self.hide();
        }
    }

    /// Returns whether the window should hide afterwards.
    fn internal(&mut self, cmd: Internal) -> bool {
        match cmd {
            Internal::Quit => {
                self.history.save();
                unsafe { PostQuitMessage(0) };
            }
            Internal::EditConfig => {
                let p = config::path();
                if !p.exists() {
                    config::save(&self.cfg);
                }
                let p = p.to_string_lossy().into_owned();
                if !actions::open(&p, false) {
                    unsafe {
                        windows::Win32::UI::Shell::ShellExecuteW(
                            None,
                            windows::core::w!("open"),
                            windows::core::w!("notepad.exe"),
                            &platform::hstring(&p),
                            windows::core::PCWSTR::null(),
                            SW_SHOWNORMAL,
                        );
                    }
                }
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
            Internal::InstallIndexer => {
                if let Ok(exe) = std::env::current_exe() {
                    let indexer = exe.with_file_name("heimdall-indexer.exe");
                    unsafe {
                        windows::Win32::UI::Shell::ShellExecuteW(
                            None,
                            windows::core::w!("runas"),
                            &platform::hstring(&indexer.to_string_lossy()),
                            windows::core::w!("install"),
                            windows::core::PCWSTR::null(),
                            SW_HIDE,
                        );
                    }
                }
            }
            Internal::ImportFlow => {
                let added = crate::flow::import(&mut self.cfg);
                config::save(&self.cfg);
                self.edit.set(&format!("Imported {added} web searches from Flow Launcher"));
                self.edit.select_all();
                self.refresh();
                return false;
            }
        }
        true
    }

    // ---- settings ----------------------------------------------------------

    fn open_settings(&mut self) {
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

    /// Keep the selection on the same row after the list is rebuilt.
    fn select_key(&mut self, key: Option<String>) {
        if let Some(i) = key.and_then(|k| self.items.iter().position(|it| it.key == k)) {
            self.sel = i;
            self.ensure_visible();
            self.sel_y.snap(self.sel_target());
            self.kick();
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
            Setting::Glass => cycle(&mut c.glass, &["liquid", "auto", "solid"]),
            Setting::Shadow => cycle(&mut c.shadow, &["normal", "soft", "off"]),
            Setting::Recent => c.show_recent = !c.show_recent,
            Setting::GameMode => c.game_mode = !c.game_mode,
        }
        if s != Setting::Hotkey {
            self.save_config();
            self.sync_tray();
        }
        self.theme = Theme::new(&self.cfg);
        self.refresh();
        self.select_key(key);
    }

    /// A key arrived while recording a new hotkey.
    fn record_key(&mut self, vk: u16) {
        let k = VIRTUAL_KEY(vk);
        if matches!(k, VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN | VK_LSHIFT | VK_RSHIFT | VK_LCONTROL | VK_RCONTROL | VK_LMENU | VK_RMENU) {
            return;
        }
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let mods = down(VK_MENU) as u32
            | (down(VK_CONTROL) as u32) << 1
            | (down(VK_SHIFT) as u32) << 2
            | ((down(VK_LWIN) || down(VK_RWIN)) as u32) << 3;
        // Keys that also produce a WM_CHAR: don't let it land in the search box.
        self.eat_char = mods & 3 == 0 && matches!(vk, 0x09 | 0x0D | 0x1B | 0x20 | 0x30..=0x5A | 0xBA..=0xC0 | 0xDB..=0xDF);
        let key = self.items.get(self.sel).map(|i| i.key.clone());
        if k == VK_ESCAPE && mods == 0 {
            self.recording = false;
            self.notice = None;
            self.set_hotkey(true);
        } else if let Some(name) = config::hotkey_name(mods, vk as u32) {
            self.recording = false;
            let old = std::mem::replace(&mut self.cfg.hotkey, name.clone());
            self.set_hotkey(true);
            if self.hotkey_registered {
                self.notice = None;
                self.save_config();
                self.sync_tray();
            } else {
                self.cfg.hotkey = old;
                self.set_hotkey(true);
                self.notice = Some(format!("{name} is already used by another app. Press Enter to try another."));
            }
        } else {
            self.notice = Some("Add Ctrl, Alt, Shift or Win to that key.".into());
        }
        self.refresh();
        self.select_key(key);
    }

    // ---- action menu -------------------------------------------------------

    /// Right-click on a row, Shift+F10 or the menu key. `at` is a screen point,
    /// or None to open under the selected row. The menu is drawn inside the
    /// window in Heimdall's own style, not as a native popup.
    fn context_menu(&mut self, at: Option<(i32, i32)>) {
        let anchor = match at {
            Some((sx, sy)) => {
                let (x, y) = ((sx - self.win.0) as f32 / self.scale, (sy - self.win.1) as f32 / self.scale);
                match self.row_at(x, y) {
                    Some(r) => self.set_sel(r),
                    None => return,
                }
                (x, y)
            }
            None => {
                let y = self.list_top() + (self.sel.saturating_sub(self.scroll) + 1) as f32 * ROW;
                (M + self.panel_w() - MENU_W - 12.0, y - 6.0)
            }
        };
        let Some(it) = self.items.get(self.sel).cloned() else { return };
        let mut rows: Vec<Option<MenuRow>> = Vec::new();
        match &it.action {
            Action::Window(_) => {
                rows.push(Some((Act::Open, G_OPEN, "Switch to", "\u{21B5}")));
                rows.push(Some((Act::Close, G_CLOSE, "Close window", "")));
            }
            Action::Open(target) => {
                rows.push(Some((Act::Open, G_OPEN, if it.kind == Kind::App { "Launch" } else { "Open" }, "\u{21B5}")));
                let ext = it.path.as_deref().unwrap_or(target).rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
                if it.kind == Kind::App || matches!(ext.as_deref(), Some("exe" | "lnk" | "bat" | "cmd" | "msi" | "msc")) {
                    rows.push(Some((Act::Admin, G_ADMIN, "Run as administrator", "Ctrl+Shift+\u{21B5}")));
                }
                if it.path.is_some() {
                    rows.push(Some((Act::Location, G_FOLDER, "Open file location", "Ctrl+\u{21B5}")));
                }
                if it.kind == Kind::File {
                    rows.push(Some((Act::OpenWith, G_OPEN_WITH, "Open with\u{2026}", "")));
                }
                if it.path.is_some() {
                    rows.push(None);
                    rows.push(Some((Act::CopyPath, G_COPY, "Copy path", "Ctrl+C")));
                    rows.push(Some((Act::Properties, G_INFO, "Properties", "Alt+\u{21B5}")));
                }
            }
            _ => return,
        }
        let h = menu_height(&rows);
        let win_h = self.win.3 as f32 / self.scale;
        let x = anchor.0.min(M + self.panel_w() - MENU_W - 8.0).max(M + 8.0);
        let y = if anchor.1 + h > win_h - M + 20.0 { (anchor.1 - h).max(M) } else { anchor.1 };
        self.menu = Some(ActionMenu { rows, sel: 0, x, y, item: it });
        self.menu_fade.snap(0.0);
        self.menu_fade.target = 1.0;
        self.kick();
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.kick();
        }
    }

    fn menu_rect(&self) -> Option<Rect> {
        self.menu.as_ref().map(|m| Rect::new(m.x, m.y, MENU_W, menu_height(&m.rows)))
    }

    /// Index of the action row under a point.
    fn menu_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let m = self.menu.as_ref()?;
        if !self.menu_rect()?.contains(x, y) {
            return None;
        }
        let mut top = m.y + MENU_PAD;
        for (i, r) in m.rows.iter().enumerate() {
            let h = if r.is_some() { MENU_ROW } else { MENU_SEP };
            if y >= top && y < top + h {
                return r.is_some().then_some(i);
            }
            top += h;
        }
        None
    }

    fn menu_move(&mut self, down: bool) {
        let Some(m) = self.menu.as_mut() else { return };
        let n = m.rows.len();
        let mut i = m.sel;
        for _ in 0..n {
            i = if down { (i + 1) % n } else { (i + n - 1) % n };
            if m.rows[i].is_some() {
                m.sel = i;
                break;
            }
        }
        self.kick();
    }

    fn menu_run(&mut self, i: usize) {
        let Some(m) = self.menu.take() else { return };
        if let Some(Some((act, ..))) = m.rows.get(i) {
            self.run_act(*act, &m.item);
        }
        self.kick();
    }

    fn run_act(&mut self, act: Act, it: &Item) {
        let q = self.edit.text.clone();
        let path = it.path.clone();
        let target = path.clone().or_else(|| if let Action::Open(t) = &it.action { Some(t.clone()) } else { None });
        match act {
            Act::Open => {
                if let Some(i) = self.items.iter().position(|x| x.key == it.key && x.title == it.title) {
                    self.sel = i;
                }
                self.activate(false, false);
            }
            Act::Admin => {
                if let Some(t) = &target {
                    actions::open(t, true);
                    self.history.record(&q, it);
                    self.hide();
                }
            }
            Act::Location => {
                if let Some(p) = &path {
                    actions::reveal(p);
                    self.hide();
                }
            }
            Act::OpenWith => {
                if let Some(p) = &path {
                    self.hide();
                    menu::open_with(p);
                }
            }
            Act::CopyPath => {
                if let Some(p) = &path {
                    actions::copy_text(self.hwnd, p);
                }
            }
            Act::Properties => {
                if let Some(p) = &path {
                    self.hide();
                    menu::properties(p);
                }
            }
            Act::Close => {
                if let Action::Window(h) = it.action {
                    unsafe { PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)).ok() };
                    self.instant_rows.retain(|r| !matches!(r.action, Action::Window(x) if x == h));
                    self.merge(false);
                }
            }
        }
    }

    fn draw_menu(&self, g: &Gfx) {
        let (Some(m), Some(r)) = (&self.menu, self.menu_rect()) else { return };
        let th = &self.theme;
        let t = self.menu_fade.x.clamp(0.0, 1.0);
        g.push_opacity(t);
        let r = Rect::new(r.x, r.y - (1.0 - t) * 6.0, r.w, r.h);
        let black = Color::rgba(0.0, 0.0, 0.0, 1.0);
        let white = Color::rgba(1.0, 1.0, 1.0, 1.0);
        // A soft shadow from a few widening layers.
        for (grow, a) in [(14.0, 0.025), (9.0, 0.04), (5.0, 0.06), (2.0, 0.08)] {
            let a = if th.dark { a * 2.2 } else { a };
            g.fill_round(Rect::new(r.x - grow, r.y - grow + 6.0, r.w + 2.0 * grow, r.h + 2.0 * grow), 14.0 + grow, black.alpha(a));
        }
        let (bg, rim, hover) = if th.dark {
            (Color::rgba(0.115, 0.12, 0.137, 0.97), white.alpha(0.11), white.alpha(0.085))
        } else {
            (Color::rgba(0.992, 0.992, 0.996, 0.98), black.alpha(0.09), black.alpha(0.055))
        };
        g.fill_round(r, 13.0, bg);
        g.stroke_round(r, 13.0, rim);
        let mut y = r.y + MENU_PAD;
        for (i, row) in m.rows.iter().enumerate() {
            let Some((_, glyph, label, hint)) = row else {
                g.hline(r.x + 14.0, r.x + r.w - 14.0, y + MENU_SEP / 2.0, th.line);
                y += MENU_SEP;
                continue;
            };
            let selected = i == m.sel;
            if selected {
                g.fill_round(Rect::new(r.x + 6.0, y, r.w - 12.0, MENU_ROW), 8.0, hover);
            }
            g.text(&glyph.to_string(), &g.fonts.menu_glyph, Rect::new(r.x + 12.0, y, 26.0, MENU_ROW), if selected { th.text } else { th.sub });
            g.text(label, &g.fonts.menu, Rect::new(r.x + 44.0, y, r.w - 44.0 - 96.0, MENU_ROW), th.text);
            g.text(hint, &g.fonts.hint, Rect::new(r.x + r.w - 14.0 - 96.0, y, 96.0, MENU_ROW), th.faint);
            y += MENU_ROW;
        }
        g.pop_layer();
    }

    fn tray_menu(&mut self) {
        let Some(m) = Menu::new() else { return };
        m.item(1, &format!("Open Heimdall\t{}", self.cfg.hotkey));
        m.item(2, "Settings");
        m.sep();
        m.check(3, "Start with Windows", self.cfg.start_with_windows);
        m.sep();
        m.item(4, "Quit Heimdall");
        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt).ok() };
        match m.show(self.hwnd, pt.x, pt.y, self.theme.dark) {
            1 => self.show(),
            2 => self.open_settings(),
            3 => {
                self.cfg.start_with_windows = !self.cfg.start_with_windows;
                platform::set_autostart(self.cfg.start_with_windows);
                self.save_config();
            }
            4 => {
                self.internal(Internal::Quit);
            }
            _ => {}
        }
    }

    // ---- input -------------------------------------------------------------

    fn key_down(&mut self, vk: u16) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        if let Some(sel) = self.menu.as_ref().map(|m| m.sel) {
            match VIRTUAL_KEY(vk) {
                VK_UP => self.menu_move(false),
                VK_DOWN | VK_TAB => self.menu_move(true),
                VK_RETURN => {
                    self.eat_char = true;
                    self.menu_run(sel);
                }
                VK_ESCAPE | VK_APPS => {
                    self.eat_char = vk == VK_ESCAPE.0;
                    self.close_menu();
                }
                _ => {
                    // Anything else goes back to the search box.
                    self.close_menu();
                    return self.key_down(vk);
                }
            }
            return true;
        }
        let before = self.edit.text.clone();
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE => {
                if self.preview_on {
                    self.toggle_preview();
                } else if !self.edit.text.is_empty() && !self.edit.all {
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
            VK_UP => self.set_sel(self.sel.saturating_sub(1)),
            VK_DOWN => self.set_sel(self.sel + 1),
            VK_PRIOR => self.set_sel(self.sel.saturating_sub(self.max_rows())),
            VK_NEXT => self.set_sel(self.sel + self.max_rows()),
            VK_RETURN => {
                self.activate(ctrl, shift);
                return true;
            }
            VK_TAB => {
                if let Some(c) = self.items.get(self.sel).and_then(sources::completion) {
                    self.edit.set(&c);
                }
            }
            VK_SPACE if shift => {
                self.eat_char = true;
                self.toggle_preview();
                return true;
            }
            VK_BACK => self.edit.backspace(ctrl),
            VK_DELETE if ctrl && shift => {
                if let Some(it) = self.items.get(self.sel) {
                    let key = it.key.clone();
                    self.history.forget(&key);
                    self.refresh();
                }
                return true;
            }
            VK_DELETE => self.edit.delete(ctrl),
            VK_LEFT => self.edit.left(ctrl),
            VK_RIGHT => self.edit.right(ctrl),
            VK_HOME if !ctrl => self.edit.home(),
            VK_END if !ctrl => self.edit.end(),
            VK_HOME => self.set_sel(0),
            VK_END => self.set_sel(self.items.len().saturating_sub(1)),
            _ if ctrl => match vk as u8 {
                b'A' => self.edit.select_all(),
                b'V' => {
                    if let Some(t) = actions::clipboard_text(self.hwnd) {
                        self.edit.insert(t.lines().next().unwrap_or(""));
                    }
                }
                b'C' => {
                    if let Some(it) = self.items.get(self.sel) {
                        let s = match &it.action {
                            Action::Copy(s) => s.clone(),
                            _ => it.path.clone().unwrap_or_else(|| it.title.clone()),
                        };
                        actions::copy_text(self.hwnd, &s);
                    }
                    return true;
                }
                b'P' => self.toggle_preview(),
                _ => return false,
            },
            _ => return false,
        }
        self.caret_on = true;
        if self.edit.text != before {
            self.refresh();
        } else {
            self.kick();
        }
        true
    }

    fn char_in(&mut self, c: char) {
        if std::mem::take(&mut self.eat_char) {
            return;
        }
        self.menu = None;
        if c.is_control() {
            return;
        }
        let mut buf = [0u8; 4];
        self.edit.insert(c.encode_utf8(&mut buf));
        self.caret_on = true;
        self.refresh();
    }

    fn dip(&self, lp: LPARAM) -> (f32, f32, i32, i32) {
        let x = (lp.0 & 0xFFFF) as i16 as i32;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
        (x as f32 / self.scale, y as f32 / self.scale, x, y)
    }

    fn mouse_move(&mut self, lp: LPARAM, buttons: usize) {
        let (x, y, px, py) = self.dip(lp);
        if self.mouse_last == (i32::MIN, i32::MIN) {
            self.mouse_last = (px, py);
            return;
        }
        if (px, py) == self.mouse_last {
            return;
        }
        self.mouse_last = (px, py);
        if self.menu.is_some() {
            if let Some(i) = self.menu_row_at(x, y) {
                if let Some(m) = self.menu.as_mut() {
                    if m.sel != i {
                        m.sel = i;
                        self.kick();
                    }
                }
            }
            return;
        }
        if let Some((sx, sy, row)) = self.press {
            if buttons & 1 != 0 && ((px - sx).abs() > 6 || (py - sy).abs() > 6) {
                self.press = None;
                if let Some(path) = self.items.get(row).and_then(|i| i.path.clone()) {
                    self.dragging = true;
                    actions::drag(self.hwnd, &path);
                    self.dragging = false;
                    self.hide();
                }
                return;
            }
        }
        if let Some(r) = self.row_at(x, y) {
            self.set_sel(r);
        }
    }

    fn mouse_down(&mut self, lp: LPARAM) {
        let (x, y, px, py) = self.dip(lp);
        if let Some(r) = self.menu_rect() {
            // Clicks inside run on release; a click anywhere else just closes it.
            if !r.contains(x, y) {
                self.close_menu();
            }
            return;
        }
        if !self.shape_rect().contains(x, y) {
            self.hide();
            return;
        }
        if let Some(r) = self.row_at(x, y) {
            self.set_sel(r);
            self.press = Some((px, py, r));
            unsafe { SetCapture(self.hwnd) };
        }
    }

    fn mouse_up(&mut self, lp: LPARAM) {
        unsafe { ReleaseCapture().ok() };
        let (x, y, _, _) = self.dip(lp);
        if self.menu.is_some() {
            if let Some(i) = self.menu_row_at(x, y) {
                self.menu_run(i);
            }
            return;
        }
        if let Some((_, _, row)) = self.press.take() {
            if self.row_at(x, y) == Some(row) {
                self.activate(false, false);
            }
        }
    }

    fn wheel(&mut self, wp: WPARAM) {
        self.close_menu();
        let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
        let steps = -delta / 120;
        if steps > 0 {
            self.set_sel(self.sel + steps as usize);
        } else if steps < 0 {
            self.set_sel(self.sel.saturating_sub((-steps) as usize));
        }
    }

    // ---- frame loop --------------------------------------------------------

    /// Ask for a frame. The message loop draws it once pending input is handled.
    fn kick(&mut self) {
        self.frame_pending = true;
    }

    /// Does the message loop owe us a frame? Returns the frame-ready handle to wait on.
    pub fn wants_frame(&self) -> Option<HANDLE> {
        (self.visible && self.frame_pending).then(|| self.gfx.as_ref().map(|g| g.waitable()).unwrap_or_default())
    }

    pub fn tick(&mut self) {
        self.frame_pending = false;
        if !self.visible {
            return;
        }
        let now = Instant::now();
        let dt = self.anim_last.map(|t| (now - t).as_secs_f32()).unwrap_or(1.0 / 120.0).min(1.0 / 30.0);
        let mut moving = false;
        for s in [&mut self.height, &mut self.sel_y, &mut self.opacity, &mut self.zoom, &mut self.pw, &mut self.menu_fade] {
            moving |= s.step(dt);
        }
        self.render();
        if crate::trace() {
            let ms = now.elapsed().as_secs_f64() * 1e3;
            if ms > 2.0 {
                crate::log(format!("frame took {ms:.2} ms"));
            }
        }
        if moving {
            self.anim_last = Some(now);
            self.frame_pending = true;
        } else {
            self.anim_last = None;
        }
    }

    fn render(&mut self) {
        let Some(mut gfx) = self.gfx.take() else { return };
        let s = self.scale;
        let shape = self.shape_rect();
        let zoom = self.zoom.x;
        // Zoom around the top centre of the panel.
        let cx = shape.x + shape.w / 2.0;
        let cy = shape.y;
        let zr = |r: Rect| {
            Rect::new(cx + (r.x - cx) * zoom, cy + (r.y - cy) * zoom, r.w * zoom, r.h * zoom)
        };
        let zs = zr(shape);
        let sel_rect = Rect::new(M + 8.0, self.sel_y.x, self.panel_w() - 16.0, ROW);
        let zsel = zr(sel_rect);
        let n = self.visible_rows().max(1) as f32;
        let frac = ((self.sel_y.x - self.list_top()) / ROW / n).clamp(0.0, 1.0);
        let (lx, ly) = (-0.55 + 0.9 * frac, -1.0f32);
        let ll = (lx * lx + ly * ly).sqrt();
        let th = &self.theme;
        let tint = match (th.dark, th.glass) {
            (true, true) => [0.06, 0.065, 0.08, 0.30],
            (true, false) => [0.115, 0.12, 0.135, 1.0],
            (false, true) => [0.985, 0.985, 0.99, 0.12],
            (false, false) => [0.955, 0.957, 0.965, 1.0],
        };
        let has_backdrop = gfx.has_backdrop();
        let params = GlassParams {
            scale: s,
            dark: if th.dark { 1.0 } else { 0.0 },
            shape: [zs.x * s, zs.y * s, zs.w * s, zs.h * s],
            sel: [zsel.x * s, zsel.y * s, zsel.w * s, zsel.h * s],
            tint,
            accent: [th.accent.r, th.accent.g, th.accent.b, 1.0],
            radius: RADIUS * s * zoom,
            sel_radius: 12.0 * s * zoom,
            sel_alpha: if self.items.is_empty() { 0.0 } else { 1.0 },
            opacity: self.opacity.x.clamp(0.0, 1.0),
            light: [lx / ll, ly / ll],
            backdrop: if has_backdrop { 1.0 } else { 0.0 },
            lens: 26.0,
            back_size: [self.win.2 as f32, self.win.3 as f32],
            blur_lod: 4.6 + s.log2(),
            shadow: match self.cfg.shadow.as_str() {
                "off" => 0.0,
                "soft" => 0.45,
                _ => 1.0,
            },
            ..Default::default()
        };
        let mut missing: Vec<(String, String)> = Vec::new();
        let r = gfx.frame(params, 96.0 * s, |g| self.draw(g, shape, &mut missing));
        if let Err(e) = r {
            crate::log(format!("render failed: {e}; recreating device"));
            self.gfx = Gfx::new(self.hwnd, self.win.2, self.win.3).ok();
            self.icons.clear();
            return;
        }
        self.gfx = Some(gfx);
        let px = (ICON * self.scale).round() as u32;
        for (key, path) in missing {
            if self.icon_pending.insert(key.clone()) {
                self.fast.send(Job::Icon { key, path, px });
            }
        }
    }

    fn draw(&self, g: &Gfx, shape: Rect, missing: &mut Vec<(String, String)>) {
        let th = &self.theme;
        let w = self.panel_w();
        let opacity = self.opacity.x.clamp(0.0, 1.0);
        let fade = opacity < 0.999;
        if fade {
            g.push_opacity(opacity);
        }
        if (self.zoom.x - 1.0).abs() > 1e-3 {
            g.set_transform(self.zoom.x, shape.x + shape.w / 2.0, shape.y);
        }
        g.push_clip(shape);

        // Search bar.
        g.text(&sources::G_SEARCH.to_string(), &g.fonts.glyph, Rect::new(M + 12.0, M, 34.0, BAR), th.faint);
        let tx = M + 52.0;
        let tw = w - 52.0 - 110.0;
        if self.edit.text.is_empty() {
            let placeholder = if self.settings_page { "Filter settings" } else { "Search apps, files and settings" };
            g.text(placeholder, &g.fonts.query, Rect::new(tx, M, tw, BAR), th.faint);
            if self.caret_on {
                g.fill(Rect::new(tx, M + BAR / 2.0 - 12.0, 1.5, 24.0), th.accent);
            }
        } else if let Some(layout) = g.layout(&self.edit.text, &g.fonts.query, tw, BAR) {
            if self.edit.all {
                let lw = layout_width(&layout).min(tw);
                g.fill_round(Rect::new(tx - 2.0, M + BAR / 2.0 - 14.0, lw + 4.0, 28.0), 5.0, th.accent.alpha(0.32));
            }
            g.draw_layout(&layout, (tx, M), th.text);
            if self.caret_on && !self.edit.all {
                let cx = caret_x(&layout, self.edit.caret16());
                g.fill(Rect::new(tx + cx, M + BAR / 2.0 - 12.0, 1.5, 24.0), th.accent);
            }
        }
        let status = match self.mode {
            Mode::Windows => "Windows",
            Mode::Path => "Browse",
            Mode::Settings => "Settings",
            Mode::Help => "Tips",
            _ if !self.files.ready() => "Indexing\u{2026}",
            Mode::Recent => "? for tips",
            _ => "",
        };
        g.text(status, &g.fonts.hint, Rect::new(M + w - 130.0, M, 112.0, BAR), th.faint);

        let n = self.visible_rows();
        if n > 0 {
            g.hline(M + 16.0, M + w - 16.0, M + BAR, th.line);
            if self.header_h() > 0.0 {
                let header = match self.mode {
                    Mode::Settings => "SETTINGS",
                    Mode::Help => "TIPS",
                    _ => "RECENT",
                };
                g.text(header, &g.fonts.header, Rect::new(M + 22.0, M + BAR + PAD, 200.0, HEADER), th.faint);
            }
            let top = self.list_top();
            for vi in 0..n {
                let i = self.scroll + vi;
                let Some(it) = self.items.get(i) else { break };
                self.draw_row(g, it, i == self.sel, top + vi as f32 * ROW, w, missing);
            }
        }

        if self.pw.x > 1.0 {
            self.draw_preview(g, M + w, shape.h);
        }

        g.pop_clip();
        self.draw_menu(g);
        g.reset_transform();
        if fade {
            g.pop_layer();
        }
    }

    fn draw_row(&self, g: &Gfx, it: &Item, selected: bool, y: f32, w: f32, missing: &mut Vec<(String, String)>) {
        let th = &self.theme;
        let ix = M + 20.0;
        let iy = y + (ROW - ICON) / 2.0;
        match &it.icon {
            Icon::Glyph(c) => {
                g.text(&c.to_string(), &g.fonts.glyph_row, Rect::new(ix, y, ICON, ROW), if selected { th.text } else { th.sub });
            }
            Icon::Shell(path) => {
                let key = icon_key(path);
                match self.icons.get(&key) {
                    Some(Some(b)) => g.bitmap(b, Rect::new(ix, iy, ICON, ICON), 1.0),
                    Some(None) => {}
                    None => missing.push((key, path.clone())),
                }
            }
            Icon::None => {}
        }

        let x = M + 64.0;
        let hint_w = 120.0;
        let title_w = w - 64.0 - 18.0 - hint_w;
        let confirming = self.confirm_key.as_deref() == Some(it.key.as_str()) && it.confirm;
        let subtitle = if confirming {
            format!("Press Enter again to {}", it.title.to_lowercase())
        } else {
            it.subtitle.clone()
        };
        let has_sub = !subtitle.is_empty();
        let title_y = if has_sub { y + 5.0 } else { y };
        let title_h = if has_sub { 22.0 } else { ROW };
        if let Some(layout) = g.layout(&it.title, &g.fonts.title, title_w, title_h) {
            // Matched letters go bold and full strength; the rest steps back a little.
            let base = if it.hl.is_empty() { th.text } else { th.text.alpha(0.80) };
            g.text_highlighted(&layout, (x, title_y), base, &it.hl, th.hl);
        }
        if has_sub {
            let c = if confirming { th.accent } else { th.sub };
            g.text(&subtitle, &g.fonts.subtitle, Rect::new(x, y + 25.0, title_w, 17.0), c);
        }
        let (hint, colour) = match (it.value.is_empty(), selected) {
            (false, true) => (format!("{}  \u{21B5}", it.value), th.text),
            (false, false) => (it.value.clone(), th.sub),
            (true, true) => (format!("{}  \u{21B5}", it.verb()), th.sub),
            (true, false) => (it.kind.label().to_string(), th.faint),
        };
        g.text(&hint, &g.fonts.hint, Rect::new(M + w - 18.0 - hint_w, y, hint_w, ROW), colour);
    }

    fn draw_preview(&self, g: &Gfx, x0: f32, h: f32) {
        let th = &self.theme;
        let pw = self.pw.x;
        g.push_clip(Rect::new(x0, M, pw, h));
        g.vline(x0, M + 14.0, M + h - 14.0, th.line);
        let x = x0 + 20.0;
        let cw = PREVIEW - 40.0;
        let Some(pv) = &self.preview else {
            let msg = if self.preview_req.is_some() { "" } else { "No preview" };
            g.text(msg, &g.fonts.hint, Rect::new(x, M, cw, h), th.faint);
            g.pop_clip();
            return;
        };
        let p = &pv.data;
        let info_h = 112.0;
        let area = Rect::new(x, M + 18.0, cw, (h - info_h - 30.0).max(40.0));

        if !p.lines.is_empty() {
            g.fill_round(area, 10.0, if th.dark { Color::rgba(0.0, 0.0, 0.0, 0.22) } else { Color::rgba(1.0, 1.0, 1.0, 0.45) });
            g.push_clip(Rect::new(area.x + 10.0, area.y + 8.0, area.w - 20.0, area.h - 16.0));
            for (i, line) in p.lines.iter().enumerate() {
                let ly = area.y + 8.0 + i as f32 * 15.0;
                if ly > area.y + area.h {
                    break;
                }
                g.text(line, &g.fonts.mono, Rect::new(area.x + 10.0, ly, area.w - 20.0, 15.0), th.sub);
            }
            g.pop_clip();
        } else if p.dir && !p.children.is_empty() {
            for (i, c) in p.children.iter().enumerate() {
                let ly = area.y + i as f32 * 19.0;
                if ly + 19.0 > area.y + area.h {
                    break;
                }
                g.text(c, &g.fonts.body, Rect::new(area.x, ly, area.w, 19.0), th.sub);
            }
        } else if let Some((bmp, bw, bh)) = &pv.bmp {
            let (bw, bh) = (*bw as f32 / self.scale, *bh as f32 / self.scale);
            let k = (area.w / bw).min(area.h / bh).min(if p.thumbnail { 4.0 } else { 1.0 });
            let (dw, dh) = (bw * k, bh * k);
            let r = Rect::new(area.x + (area.w - dw) / 2.0, area.y + (area.h - dh) / 2.0, dw, dh);
            g.bitmap(bmp, r, 1.0);
        }

        let iy = M + h - info_h;
        let name = platform::file_name(&p.path);
        g.text(name, &g.fonts.preview_title, Rect::new(x, iy, cw, 24.0), th.text);
        let mut meta = Vec::new();
        if p.dir {
            meta.push(format!("Folder \u{00B7} {} items", p.child_count));
        } else if let Some(sz) = p.size {
            let ext = std::path::Path::new(&p.path)
                .extension()
                .map(|e| e.to_string_lossy().to_uppercase())
                .unwrap_or_else(|| "File".into());
            meta.push(format!("{ext} \u{00B7} {}", human_size(sz)));
        }
        if let Some(m) = &p.modified {
            meta.push(format!("Modified {m}"));
        }
        meta.push(platform::pretty_path(platform::parent_dir(&p.path)));
        for (i, line) in meta.iter().enumerate() {
            g.text(line, &g.fonts.body, Rect::new(x, iy + 28.0 + i as f32 * 18.0, cw, 18.0), if i == 2 { th.faint } else { th.sub });
        }
        g.pop_clip();
    }

    // ---- message dispatch --------------------------------------------------

    pub fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        if crate::trace() {
            let t0 = Instant::now();
            let r = self.dispatch(msg, wp, lp);
            let ms = t0.elapsed().as_secs_f64() * 1e3;
            if ms > 2.0 {
                crate::log(format!("msg {msg:#06x} wp={} took {ms:.2} ms", wp.0));
            }
            return r;
        }
        self.dispatch(msg, wp, lp)
    }

    fn dispatch(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_HOTKEY => self.toggle(),
            WM_KEYDOWN | WM_SYSKEYDOWN if self.recording => self.record_key(wp.0 as u16),
            // No window menu and no beep for Alt+letter.
            WM_SYSCHAR => {}
            WM_CONTEXTMENU => {
                let at = (lp.0 != -1).then(|| ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32));
                self.context_menu(at);
            }
            menu::WM_APP_TRAY => match (lp.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP => self.show(),
                WM_RBUTTONUP => self.tray_menu(),
                _ => {}
            },
            m if m == self.tray.taskbar_created => {
                let tip = format!("Heimdall ({})", self.cfg.hotkey);
                self.tray.readd(self.cfg.tray, &tip);
            }
            WM_APP_SHOW => {
                if !self.visible {
                    self.show();
                }
            }
            m if m == crate::files::WM_FILES_DONE => self.on_files(),
            m if m == crate::shell::WM_SHELL_DONE => self.on_shell(),
            WM_KEYDOWN => {
                if !self.key_down(wp.0 as u16) {
                    return None;
                }
            }
            WM_SYSKEYDOWN if wp.0 as u16 == VK_F4.0 => self.hide(),
            WM_SYSKEYDOWN if wp.0 as u16 == VK_RETURN.0 => {
                if let Some(it) = self.items.get(self.sel).cloned() {
                    self.close_menu();
                    self.run_act(Act::Properties, &it);
                }
            }
            WM_CHAR => {
                if let Some(c) = char::from_u32(wp.0 as u32) {
                    self.char_in(c);
                }
            }
            WM_ACTIVATE => {
                if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE && self.visible && !self.dragging {
                    self.hide();
                }
            }
            WM_TIMER if wp.0 == TIMER_CARET => {
                self.caret_on = !self.caret_on;
                self.kick();
            }
            WM_MOUSEMOVE => self.mouse_move(lp, wp.0),
            WM_LBUTTONDOWN => self.mouse_down(lp),
            WM_LBUTTONUP => self.mouse_up(lp),
            WM_MOUSEWHEEL => self.wheel(wp),
            WM_DISPLAYCHANGE | WM_DPICHANGED => self.hide(),
            _ => return None,
        }
        Some(LRESULT(0))
    }
}

fn human_size(b: u64) -> String {
    const U: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} bytes")
    } else {
        format!("{v:.1} {}", U[i])
    }
}
