//! AppKit glue: the panel and its glass, the view that draws and takes
//! input, the menu bar icon, and the run loop.

mod app;
mod apps;
mod draw;
mod files;
mod hotkey;
mod platform;
mod sources;

use app::{App, Key};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::*;
use std::cell::{Cell, RefCell};

const BUNDLE_ID: &str = "com.kryzern.heimdall";
const SHOW_NOTE: &str = "com.kryzern.heimdall.show";

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static TARGET: RefCell<Option<Retained<MenuTarget>>> = const { RefCell::new(None) };
    /// Tag of the right-click menu item picked, 0 for none.
    pub(crate) static CHOSEN: Cell<isize> = const { Cell::new(0) };
}

/// Run `f` against the app unless it is already borrowed (a callback that
/// arrived while the app was busy, e.g. during `orderOut`).
pub(crate) fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

define_class!(
    /// A borderless panel that can take the keyboard without activating
    /// Heimdall, so the app underneath stays frontmost like with Spotlight.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "HeimdallPanel"]
    pub struct Panel;

    impl Panel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            false
        }

        #[unsafe(method(resignKeyWindow))]
        fn resign_key(&self) {
            unsafe {
                let _: () = msg_send![super(self), resignKeyWindow];
            }
            with_app(|a| a.on_resign_key());
        }
    }
);

define_class!(
    /// Draws the frame the app built and passes input to it.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "HeimdallView"]
    pub struct HeimdallView;

    impl HeimdallView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw::paint();
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let key = Key {
                code: event.keyCode(),
                chars: event.characters().map(|s| s.to_string()).unwrap_or_default(),
                flags: event.modifierFlags().0 as usize,
            };
            with_app(|a| a.key_down(&key));
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let p = self.point(event);
            with_app(|a| a.mouse_move(p.x, p.y));
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.point(event);
            with_app(|a| a.mouse_down(p.x, p.y));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let p = self.point(event);
            with_app(|a| a.mouse_up(p.x, p.y));
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let p = self.point(event);
            with_app(|a| a.context_menu(p.x, p.y));
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let (dy, precise) = (event.scrollingDeltaY(), event.hasPreciseScrollingDeltas());
            with_app(|a| a.scroll(dy, precise));
        }
    }
);

impl HeimdallView {
    fn point(&self, event: &NSEvent) -> NSPoint {
        self.convertPoint_fromView(event.locationInWindow(), None)
    }
}

define_class!(
    /// Target for menu items and the "show yourself" notification.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "HeimdallMenuTarget"]
    pub struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(pick:))]
        fn pick(&self, sender: &NSMenuItem) {
            CHOSEN.set(sender.tag());
        }

        #[unsafe(method(openPanel:))]
        fn open_panel(&self, _sender: Option<&AnyObject>) {
            with_app(|a| {
                if !a.visible {
                    a.show();
                }
            });
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            with_app(|a| a.open_settings());
        }
    }
);

/// The one menu target, created on first use.
pub(crate) fn menu_target(mtm: MainThreadMarker) -> Retained<MenuTarget> {
    TARGET.with(|t| {
        t.borrow_mut()
            .get_or_insert_with(|| unsafe { msg_send![MenuTarget::alloc(mtm), init] })
            .clone()
    })
}

fn menu_item(mtm: MainThreadMarker, title: &str, action: objc2::runtime::Sel, target: Option<&AnyObject>) -> Retained<NSMenuItem> {
    let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(action), &NSString::new()) };
    unsafe { item.setTarget(target) };
    item
}

fn status_item(mtm: MainThreadMarker) -> Retained<NSStatusItem> {
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str("magnifyingglass"), Some(&NSString::from_str("Heimdall"))) {
            img.setTemplate(true);
            button.setImage(Some(&img));
        }
    }
    let target = menu_target(mtm);
    let menu = NSMenu::new(mtm);
    menu.addItem(&menu_item(mtm, "Open Heimdall", sel!(openPanel:), Some(&target)));
    menu.addItem(&menu_item(mtm, "Settings\u{2026}", sel!(openSettings:), Some(&target)));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    // No target: goes up the responder chain to NSApplication.
    menu.addItem(&menu_item(mtm, "Quit Heimdall", sel!(terminate:), None));
    item.setMenu(Some(&menu));
    item
}

/// A stretchable rounded-rectangle mask. Masking the frosted view this way
/// (rather than rounding its layer) also gives the window a rounded shadow.
fn corner_mask(radius: f64) -> Retained<NSImage> {
    let side = radius * 2.0 + 1.0;
    let handler = block2::RcBlock::new(move |rect: NSRect| -> objc2::runtime::Bool {
        NSColor::blackColor().setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius).fill();
        objc2::runtime::Bool::YES
    });
    let img = NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(side, side), false, &handler);
    img.setCapInsets(NSEdgeInsets { top: radius, left: radius, bottom: radius, right: radius });
    img.setResizingMode(NSImageResizingMode::Stretch);
    img
}

/// Another copy is already running: ask it to open, and say so.
fn already_running() -> bool {
    let others = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(BUNDLE_ID));
    if others.len() <= 1 {
        return false;
    }
    let center = NSDistributedNotificationCenter::defaultCenter();
    unsafe { center.postNotificationName_object_userInfo_deliverImmediately(&NSString::from_str(SHOW_NOTE), None, None, true) };
    true
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("Heimdall must start on the main thread");
    let nsapp = NSApplication::sharedApplication(mtm);
    // No Dock icon and no menu bar: a menu bar icon and a hotkey are enough.
    nsapp.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    if already_running() {
        return;
    }
    let args: Vec<String> = std::env::args().collect();
    let query = args.iter().position(|a| a == "--query").and_then(|i| args.get(i + 1).cloned());
    let background = args.iter().any(|a| a == "--background");

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(680.0, 60.0));
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let panel: Retained<Panel> = unsafe {
        msg_send![Panel::alloc(mtm), initWithContentRect: frame, styleMask: style, backing: NSBackingStoreType::Buffered, defer: false]
    };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(true);
    // Above normal windows and the Dock, like Spotlight.
    panel.setLevel(25);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary | NSWindowCollectionBehavior::IgnoresCycle,
    );
    panel.setHidesOnDeactivate(false);
    panel.setAcceptsMouseMovedEvents(true);
    panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(false);
    unsafe { panel.setReleasedWhenClosed(false) };

    let view: Retained<HeimdallView> = unsafe { msg_send![HeimdallView::alloc(mtm), initWithFrame: frame] };
    view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);

    // macOS 26 has Liquid Glass; earlier versions get the frosted material.
    let content: Retained<NSView> = if AnyClass::get(c"NSGlassEffectView").is_some() {
        let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
        glass.setCornerRadius(app::RADIUS);
        glass.setContentView(Some(&view));
        Retained::into_super(glass)
    } else {
        let fx = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
        fx.setMaterial(NSVisualEffectMaterial::Popover);
        fx.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        fx.setState(NSVisualEffectState::Active);
        fx.setMaskImage(Some(&corner_mask(app::RADIUS)));
        fx.addSubview(&view);
        Retained::into_super(fx)
    };
    panel.setContentView(Some(&content));

    let state = App::new(mtm, panel, view, query);
    APP.with(|a| *a.borrow_mut() = Some(state));
    let item = status_item(mtm);
    with_app(|a| a.set_status_item(item));

    // A second launch (from Finder or Spotlight) opens the running copy.
    let target = menu_target(mtm);
    unsafe {
        NSDistributedNotificationCenter::defaultCenter().addObserver_selector_name_object(
            &target,
            sel!(openPanel:),
            Some(&NSString::from_str(SHOW_NOTE)),
            None,
        );
    }

    if !background {
        with_app(|a| a.show());
    }
    nsapp.run();
}
