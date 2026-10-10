//! Small floating panel that shows the Paste Stack queue.

use objc2::rc::Retained;
use objc2::{MainThreadOnly, msg_send};
use objc2_app_kit::{NSBackingStoreType, NSColor, NSPanel, NSScreen, NSWindowCollectionBehavior, NSWindowStyleMask};
use objc2_foundation::MainThreadMarker;

use super::{label, rect};
use crate::model::Item;

const W: f64 = 300.0;
const ROW: f64 = 22.0;
const SHOWN: usize = 6;

pub struct StackPanel {
    panel: Retained<NSPanel>,
    mtm: MainThreadMarker,
}

impl StackPanel {
    pub fn new(mtm: MainThreadMarker) -> StackPanel {
        let panel: Retained<NSPanel> = unsafe {
            msg_send![NSPanel::alloc(mtm), initWithContentRect: rect(0.0, 0.0, W, 100.0),
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered, defer: false]
        };
        // SAFETY: owned by `StackPanel`.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setLevel(25);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(true);
        panel.setIgnoresMouseEvents(true);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        StackPanel { panel, mtm }
    }

    pub fn hide(&self) {
        self.panel.orderOut(None);
    }

    /// Redraws the queue; `items` are in paste order.
    pub fn show(&self, items: &[Item], order: crate::stack::StackOrder) {
        let mtm = self.mtm;
        let rows = items.len().clamp(1, SHOWN) + usize::from(items.len() > SHOWN);
        let h = 44.0 + rows as f64 * ROW + 10.0;
        let Some(screen) = NSScreen::mainScreen(mtm) else { return };
        let vf = screen.visibleFrame();
        self.panel.setFrame_display(
            rect(vf.origin.x + vf.size.width - W - 12.0, vf.origin.y + vf.size.height - h - 12.0, W, h),
            true,
        );
        // Opaque so text contrast does not depend on what is behind the panel.
        let effect = objc2_app_kit::NSView::initWithFrame(objc2_app_kit::NSView::alloc(mtm), rect(0.0, 0.0, W, h));
        effect.setWantsLayer(true);
        if let Some(layer) = effect.layer() {
            layer.setBackgroundColor(Some(&NSColor::textBackgroundColor().CGColor()));
            layer.setCornerRadius(12.0);
            layer.setMasksToBounds(true);
            layer.setBorderWidth(1.0);
            layer.setBorderColor(Some(&NSColor::separatorColor().CGColor()));
        }
        let title = label(
            &format!(
                "Paste Stack \u{00b7} {} item{} \u{00b7} {}",
                items.len(),
                if items.len() == 1 { "" } else { "s" },
                order.label()
            ),
            13.0,
            true,
            mtm,
        );
        title.setFrame(rect(14.0, h - 30.0, W - 28.0, 18.0));
        effect.addSubview(&title);
        let mut y = h - 30.0 - ROW - 4.0;
        if items.is_empty() {
            let hint = label("Copy items to add them, then paste with \u{2318}V", 12.0, false, mtm);
            hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
            hint.setFrame(rect(14.0, y, W - 28.0, 18.0));
            effect.addSubview(&hint);
        }
        for (i, item) in items.iter().take(SHOWN).enumerate() {
            let row = label(&format!("{}. {}", i + 1, item.summary(60)), 12.0, i == 0, mtm);
            row.setFrame(rect(14.0, y, W - 28.0, 18.0));
            effect.addSubview(&row);
            y -= ROW;
        }
        if items.len() > SHOWN {
            let more = label(&format!("and {} more", items.len() - SHOWN), 11.0, false, mtm);
            more.setTextColor(Some(&NSColor::tertiaryLabelColor()));
            more.setFrame(rect(14.0, y, W - 28.0, 16.0));
            effect.addSubview(&more);
        }
        self.panel.setContentView(Some(&effect));
        self.panel.setAnimationBehavior(super::a11y::animation_behavior());
        self.panel.orderFrontRegardless();
        let spoken = match items.first() {
            None => format!("Paste Stack on, {}. Copy items to add them.", order.label()),
            Some(next) => format!("Paste Stack, {} items. Next: {}", items.len(), next.summary(80)),
        };
        super::a11y::announce(&self.panel, &spoken);
    }
}
