//! Large preview of one item, shown above the shelf without taking focus.

use objc2::rc::Retained;
use objc2::{MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFont, NSFontWeightRegular, NSImage, NSImageScaling, NSImageView, NSPanel,
    NSScrollView, NSTextView, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSRect};

use super::{ns, rect};
use crate::model::{Item, ItemKind};

define_class!(
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "PastedPreviewPanel"]
    pub struct PreviewPanel;

    impl PreviewPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            false
        }
    }

    unsafe impl NSObjectProtocol for PreviewPanel {}
);

pub struct Preview {
    panel: Retained<PreviewPanel>,
    pub item_id: Option<i64>,
}

impl Preview {
    pub fn new(mtm: MainThreadMarker) -> Preview {
        let panel: Retained<PreviewPanel> = unsafe {
            msg_send![PreviewPanel::alloc(mtm), initWithContentRect: rect(0.0, 0.0, 640.0, 440.0),
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered, defer: false]
        };
        // SAFETY: owned by `Preview`.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setLevel(26);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(true);
        Preview { panel, item_id: None }
    }

    pub fn is_visible(&self) -> bool {
        self.panel.isVisible()
    }

    pub fn hide(&mut self) {
        self.item_id = None;
        self.panel.orderOut(None);
    }

    /// Shows `item` centered above `shelf_frame`.
    pub fn show(&mut self, item: &Item, image: Option<Retained<NSImage>>, shelf_frame: NSRect, mtm: MainThreadMarker) {
        let (w, h) = (680.0f64.min(shelf_frame.size.width - 40.0), 460.0);
        let x = shelf_frame.origin.x + (shelf_frame.size.width - w) / 2.0;
        let y = shelf_frame.origin.y + shelf_frame.size.height + 12.0;
        self.panel.setFrame_display(rect(x, y, w, h), true);

        let effect = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), rect(0.0, 0.0, w, h));
        effect.setMaterial(NSVisualEffectMaterial::Popover);
        effect.setState(NSVisualEffectState::Active);
        effect.setWantsLayer(true);
        if let Some(layer) = effect.layer() {
            layer.setCornerRadius(14.0);
            layer.setMasksToBounds(true);
        }
        let inner = rect(16.0, 16.0, w - 32.0, h - 32.0);
        match (item.kind, image) {
            (ItemKind::Image, Some(image)) => {
                let iv = NSImageView::imageViewWithImage(&image, mtm);
                iv.setImageScaling(NSImageScaling::ScaleProportionallyDown);
                iv.setFrame(inner);
                effect.addSubview(&iv);
            }
            _ => {
                let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), inner);
                scroll.setHasVerticalScroller(true);
                scroll.setDrawsBackground(false);
                let tv = NSTextView::initWithFrame(
                    NSTextView::alloc(mtm),
                    rect(0.0, 0.0, inner.size.width, inner.size.height),
                );
                tv.setEditable(false);
                tv.setSelectable(true);
                tv.setDrawsBackground(false);
                let font = if item.kind == ItemKind::Code {
                    // SAFETY: reading an AppKit constant.
                    NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular })
                } else {
                    NSFont::systemFontOfSize(14.0)
                };
                tv.setFont(Some(&font));
                tv.setString(&ns(&item.text));
                scroll.setDocumentView(Some(&tv));
                effect.addSubview(&scroll);
            }
        }
        self.panel.setContentView(Some(&effect));
        self.item_id = Some(item.id);
        self.panel.orderFrontRegardless();
    }
}
