//! Drawing. The app builds a list of commands each frame; the view's
//! `drawRect:` paints them with AppKit (Core Text for text, SF Symbols and
//! Finder icons for row icons). The glass underneath is the system's own.
//!
//! Coordinates are in points, top-left origin (the view is flipped).

use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_app_kit::*;
use objc2_foundation::*;
use std::cell::RefCell;
use std::collections::HashMap;

pub type Rgba = (f64, f64, f64, f64);

#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn ns(self) -> NSRect {
        NSRect::new(NSPoint::new(self.x, self.y), NSSize::new(self.w, self.h))
    }
}

pub enum Cmd {
    Fill { r: Rect, radius: f64, color: Rgba },
    Stroke { r: Rect, radius: f64, color: Rgba, width: f64 },
    Text(Text),
    /// A Finder icon for a file, folder or app.
    Icon { path: String, r: Rect },
    /// An SF Symbol, tinted.
    Symbol { name: &'static str, r: Rect, size: f64, color: Rgba },
}

pub struct Text {
    pub text: String,
    pub r: Rect,
    pub size: f64,
    pub weight: f64,
    pub color: Rgba,
    /// UTF-16 ranges drawn bold in `hl_color`.
    pub hl: Vec<(u32, u32)>,
    pub hl_color: Rgba,
    pub right: bool,
}

pub const REGULAR: f64 = 0.0;
pub const SEMIBOLD: f64 = 0.3;
pub const BOLD: f64 = 0.4;

thread_local! {
    static FRAME: RefCell<Vec<Cmd>> = const { RefCell::new(Vec::new()) };
    static ICONS: RefCell<HashMap<String, Retained<NSImage>>> = RefCell::new(HashMap::new());
}

/// Hand over the next frame's commands.
pub fn submit(cmds: Vec<Cmd>) {
    FRAME.with(|f| *f.borrow_mut() = cmds);
}

fn color(c: Rgba) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.0, c.1, c.2, c.3)
}

/// Text with the system font, truncated with an ellipsis, `hl` ranges bold.
pub fn attributed(text: &str, size: f64, weight: f64, fg: Rgba, hl: &[(u32, u32)], hl_color: Rgba, right: bool) -> Retained<NSMutableAttributedString> {
    let font = NSFont::systemFontOfSize_weight(size, weight);
    let para = NSMutableParagraphStyle::new();
    para.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    if right {
        para.setAlignment(NSTextAlignment::Right);
    }
    let s = NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &NSString::from_str(text));
    let len = s.length();
    let all = NSRange::new(0, len);
    unsafe {
        s.addAttribute_value_range(NSFontAttributeName, &font, all);
        s.addAttribute_value_range(NSForegroundColorAttributeName, &color(fg), all);
        s.addAttribute_value_range(NSParagraphStyleAttributeName, &para, all);
        if !hl.is_empty() {
            let bold = NSFont::systemFontOfSize_weight(size, BOLD);
            let hc = color(hl_color);
            for &(start, n) in hl {
                let (start, n) = (start as usize, n as usize);
                if start + n <= len {
                    let r = NSRange::new(start, n);
                    s.addAttribute_value_range(NSFontAttributeName, &bold, r);
                    s.addAttribute_value_range(NSForegroundColorAttributeName, &hc, r);
                }
            }
        }
    }
    s
}

fn rounded(r: Rect, radius: f64) -> Retained<NSBezierPath> {
    if radius <= 0.0 {
        NSBezierPath::bezierPathWithRect(r.ns())
    } else {
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r.ns(), radius, radius)
    }
}

fn icon(path: &str) -> Retained<NSImage> {
    // Files share their type's icon; apps and folders have their own.
    let lower = path.to_lowercase();
    let ext = std::path::Path::new(&lower).extension().and_then(|e| e.to_str()).map(str::to_string);
    let key = match ext {
        Some(e) if !matches!(e.as_str(), "app" | "prefpane") && !std::path::Path::new(path).is_dir() => format!(".{e}"),
        _ => lower,
    };
    ICONS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 2000 {
            c.clear();
        }
        c.entry(key).or_insert_with(|| NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path))).clone()
    })
}

fn symbol(name: &str, size: f64, tint: Rgba) -> Option<Retained<NSImage>> {
    let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?;
    let sizing = NSImageSymbolConfiguration::configurationWithPointSize_weight(size, REGULAR);
    let colour = NSImageSymbolConfiguration::configurationWithHierarchicalColor(&color(tint));
    img.imageWithSymbolConfiguration(&sizing.configurationByApplyingConfiguration(&colour))
}

/// Paint the current frame. Called from the view's `drawRect:`.
pub fn paint() {
    FRAME.with(|f| {
        for cmd in f.borrow().iter() {
            match cmd {
                Cmd::Fill { r, radius, color: c } => {
                    color(*c).setFill();
                    rounded(*r, *radius).fill();
                }
                Cmd::Stroke { r, radius, color: c, width } => {
                    color(*c).setStroke();
                    let inset = Rect::new(r.x + width / 2.0, r.y + width / 2.0, r.w - width, r.h - width);
                    let p = rounded(inset, (radius - width / 2.0).max(0.0));
                    p.setLineWidth(*width);
                    p.stroke();
                }
                Cmd::Text(t) => {
                    let s = attributed(&t.text, t.size, t.weight, t.color, &t.hl, t.hl_color, t.right);
                    // Centre one line vertically in the rect.
                    let plain: &NSAttributedString = &s;
                    let h = plain.size().height;
                    let r = Rect::new(t.r.x, t.r.y + (t.r.h - h) / 2.0, t.r.w, h);
                    s.drawWithRect_options(r.ns(), NSStringDrawingOptions::UsesLineFragmentOrigin | NSStringDrawingOptions::TruncatesLastVisibleLine);
                }
                Cmd::Icon { path, r } => {
                    icon(path).drawInRect(r.ns());
                }
                Cmd::Symbol { name, r, size, color: c } => {
                    if let Some(img) = symbol(name, *size, *c) {
                        // Keep the symbol's own proportions, centred in the rect.
                        let s = img.size();
                        let k = (r.w / s.width).min(r.h / s.height).min(1.0);
                        let (w, h) = (s.width * k, s.height * k);
                        img.drawInRect(Rect::new(r.x + (r.w - w) / 2.0, r.y + (r.h - h) / 2.0, w, h).ns());
                    }
                }
            }
        }
    });
}

