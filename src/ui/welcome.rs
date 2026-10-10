//! The welcome window: the shortcut, Accessibility permission (with a live
//! status) and, when Paste is installed, importing its history.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::{NSBackingStoreType, NSButton, NSColor, NSFont, NSTextField, NSView, NSWindow, NSWindowStyleMask};
use objc2_foundation::MainThreadMarker;

use super::shelf::tag;
use super::{activate_app, label, ns, rect, wrapping_label};

pub const WELCOME_DONE: u8 = 80;

const W: f64 = 540.0;
const PAD: f64 = 28.0;

pub struct Welcome {
    pub window: Retained<NSWindow>,
    status: Retained<NSTextField>,
    allow: Retained<NSButton>,
    trusted: std::cell::Cell<Option<bool>>,
}

type Placed = (Retained<NSView>, f64, f64, f64, f64);

/// Places `view` at the running `y` (distance from the top) and moves it down.
fn add(views: &mut Vec<Placed>, view: Retained<NSView>, x: f64, w: f64, h: f64, y: &mut f64, gap: f64) {
    views.push((view, x, *y, w, h));
    *y += h + gap;
}

fn running_from_app_bundle() -> bool {
    objc2_foundation::NSBundle::mainBundle().bundlePath().to_string().ends_with(".app")
}

impl Welcome {
    pub fn new(target: &AnyObject, shortcut: &str, paste_found: bool, mtm: MainThreadMarker) -> Welcome {
        // Laid out top to bottom; `y` is the distance from the top.
        let mut views: Vec<Placed> = Vec::new();
        let mut y = PAD;
        let text = |s: &str, size: f64, bold: bool| -> Retained<NSView> {
            Retained::into_super(Retained::into_super(label(s, size, bold, mtm)))
        };
        let body = |s: &str| -> Retained<NSView> {
            let l = wrapping_label(s, &NSFont::systemFontOfSize(13.0), 6, mtm);
            l.setTextColor(Some(&NSColor::secondaryLabelColor()));
            Retained::into_super(Retained::into_super(l))
        };
        let inner = W - 2.0 * PAD;

        add(&mut views, text("Welcome to Pasted", 22.0, true), PAD, inner, 28.0, &mut y, 4.0);
        add(
            &mut views,
            body("To see this again, right-click the menu bar icon and choose Welcome\u{2026}"),
            PAD,
            inner,
            18.0,
            &mut y,
            22.0,
        );

        add(&mut views, text("1. Open your clipboard", 15.0, true), PAD, inner, 20.0, &mut y, 4.0);
        add(
            &mut views,
            body(&format!(
                "Press {shortcut} anywhere to see everything you copied. You can choose a different shortcut in Settings > Shortcuts."
            )),
            PAD,
            inner,
            36.0,
            &mut y,
            20.0,
        );

        add(&mut views, text("2. Let Pasted paste for you", 15.0, true), PAD, inner, 20.0, &mut y, 4.0);
        let mut explain = "To paste straight into other apps, macOS needs you to allow Pasted in System Settings > \
            Privacy & Security > Accessibility. Without it, choosing an item still copies it and you press \u{2318}V yourself."
            .to_string();
        if !running_from_app_bundle() {
            explain.push_str(
                " Pasted is running from a terminal, so macOS will list your terminal app instead of Pasted.",
            );
        }
        add(&mut views, body(&explain), PAD, inner, 72.0, &mut y, 8.0);
        let status = label("", 13.0, true, mtm);
        // SAFETY: the target implements `buttonAction:`.
        let allow = unsafe {
            NSButton::buttonWithTitle_target_action(
                &ns("Open Accessibility Settings\u{2026}"),
                Some(target),
                Some(sel!(buttonAction:)),
                mtm,
            )
        };
        allow.setTag(tag(super::settings_window::OPEN_ACCESSIBILITY, 0));
        allow.sizeToFit();
        let row_y = y;
        views.push((
            Retained::into_super(Retained::into_super(allow.clone())),
            PAD - 6.0,
            row_y,
            allow.frame().size.width,
            30.0,
        ));
        views.push((
            Retained::into_super(Retained::into_super(status.clone())),
            PAD + allow.frame().size.width + 6.0,
            row_y + 6.0,
            inner - allow.frame().size.width,
            18.0,
        ));
        y += 30.0 + 20.0;

        if paste_found {
            add(&mut views, text("3. Bring over your Paste history", 15.0, true), PAD, inner, 20.0, &mut y, 4.0);
            add(
                &mut views,
                body(
                    "Copies your Paste history, pinboards and shortcuts. Paste itself is not changed, and you can choose what to leave out in Settings > Data first.",
                ),
                PAD,
                inner,
                36.0,
                &mut y,
                8.0,
            );
            // SAFETY: the target implements `buttonAction:`.
            let import = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &ns("Import from Paste\u{2026}"),
                    Some(target),
                    Some(sel!(buttonAction:)),
                    mtm,
                )
            };
            import.setTag(tag(crate::app::IMPORT_PASTE_TAG, 0));
            import.sizeToFit();
            let w = import.frame().size.width;
            add(&mut views, Retained::into_super(Retained::into_super(import)), PAD - 6.0, w, 30.0, &mut y, 20.0);
        }

        // SAFETY: the target implements `buttonAction:`.
        let done = unsafe {
            NSButton::buttonWithTitle_target_action(&ns("Done"), Some(target), Some(sel!(buttonAction:)), mtm)
        };
        done.setTag(tag(WELCOME_DONE, 0));
        done.setKeyEquivalent(&ns("\r"));
        done.sizeToFit();
        let done_w = done.frame().size.width.max(90.0);
        views.push((Retained::into_super(Retained::into_super(done)), W - PAD - done_w, y, done_w, 30.0));
        let h = y + 30.0 + PAD;

        // SAFETY: standard window creation; the window is kept alive by `Welcome`.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, W, h),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: owned by `Welcome`.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&ns("Welcome to Pasted"));
        if let Some(content) = window.contentView() {
            for (view, x, top, w, vh) in views {
                view.setFrame(rect(x, h - top - vh, w, vh));
                content.addSubview(&view);
            }
        }
        window.center();
        let welcome = Welcome { window, status, allow, trusted: std::cell::Cell::new(None) };
        welcome.refresh();
        welcome
    }

    pub fn show(&self, mtm: MainThreadMarker) {
        self.window.setAnimationBehavior(super::a11y::animation_behavior());
        activate_app(mtm);
        self.window.makeKeyAndOrderFront(None);
        self.window.orderFrontRegardless();
    }

    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    pub fn close(&self) {
        self.window.orderOut(None);
    }

    /// Updates the permission status. Returns true when it just became allowed.
    pub fn refresh(&self) -> bool {
        let trusted = crate::paste::is_trusted();
        let before = self.trusted.replace(Some(trusted));
        if before == Some(trusted) {
            return false;
        }
        let (text, color) = if trusted {
            ("\u{2713} Allowed", NSColor::systemGreenColor())
        } else {
            ("Not allowed yet", NSColor::secondaryLabelColor())
        };
        self.status.setStringValue(&ns(text));
        self.status.setTextColor(Some(&color));
        // Hidden rather than disabled: a dimmed button is hard to read.
        self.allow.setHidden(trusted);
        let mut frame = self.status.frame();
        frame.origin.x = if trusted { PAD } else { PAD + self.allow.frame().size.width + 6.0 };
        self.status.setFrame(frame);
        if trusted && before == Some(false) {
            super::a11y::announce(&self.window, "Accessibility allowed. Pasted can now paste for you.");
            return true;
        }
        false
    }
}
