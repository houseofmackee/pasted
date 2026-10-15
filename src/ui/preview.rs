//! Large preview of one item, shown above the shelf without taking focus.

use objc2::rc::Retained;
use objc2::{MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFont, NSFontWeightRegular, NSImage, NSImageScaling, NSImageView, NSPanel,
    NSScrollView, NSTextView, NSView, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSRect};

use super::card::{cover_view, image_from};
use super::{label, wrapping_label};
use super::{ns, rect};
use crate::link_preview::LinkPreview;
use crate::model::{Item, ItemKind};

/// Image, title, description and URL of a link, top to bottom.
fn link_layout(parent: &NSView, item: &Item, link: Option<&LinkPreview>, inner: NSRect, mtm: MainThreadMarker) {
    let (x, w) = (inner.origin.x, inner.size.width);
    let mut top = inner.origin.y + inner.size.height;
    if let Some(image) = link.and_then(|l| l.image.as_deref()).and_then(image_from) {
        let h = 230.0;
        top -= h;
        parent.addSubview(&cover_view(&image, rect(x, top, w, h), mtm));
        top -= 14.0;
    }
    let title =
        item.title.clone().or_else(|| link.and_then(|l| l.title.clone())).or_else(|| item.meta.url_name.clone());
    if let Some(title) = title {
        let l = wrapping_label(&title, &NSFont::boldSystemFontOfSize(19.0), 2, mtm);
        l.setPreferredMaxLayoutWidth(w);
        let h = l.fittingSize().height.clamp(24.0, 50.0);
        top -= h;
        l.setFrame(rect(x, top, w, h));
        parent.addSubview(&l);
        top -= 6.0;
    }
    if let Some(desc) = link.and_then(|l| l.description.clone()) {
        let l = wrapping_label(&desc, &NSFont::systemFontOfSize(14.0), 4, mtm);
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
        l.setPreferredMaxLayoutWidth(w);
        let h = l.fittingSize().height.min(76.0).min((top - inner.origin.y - 30.0).max(0.0));
        top -= h;
        l.setFrame(rect(x, top, w, h));
        parent.addSubview(&l);
        top -= 8.0;
    }
    let url = item.meta.url.clone().unwrap_or_else(|| item.text.trim().to_string());
    let l = label(&url, 13.0, false, mtm);
    l.setTextColor(Some(&NSColor::linkColor()));
    l.setFrame(rect(x, (top - 20.0).max(inner.origin.y), w, 18.0));
    parent.addSubview(&l);
}

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
    pub fn show(
        &mut self,
        item: &Item,
        image: Option<Retained<NSImage>>,
        link: Option<LinkPreview>,
        rich: Option<Retained<objc2_foundation::NSAttributedString>>,
        shelf_frame: NSRect,
        mtm: MainThreadMarker,
    ) {
        let (w, h) = (680.0f64.min(shelf_frame.size.width - 40.0), 460.0);
        let x = shelf_frame.origin.x + (shelf_frame.size.width - w) / 2.0;
        let y = shelf_frame.origin.y + shelf_frame.size.height + 12.0;
        self.panel.setFrame_display(rect(x, y, w, h), true);

        // Opaque, like the cards: text contrast must not depend on what is behind the panel.
        let effect = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, w, h));
        effect.setWantsLayer(true);
        if let Some(layer) = effect.layer() {
            layer.setBackgroundColor(Some(&NSColor::textBackgroundColor().CGColor()));
            layer.setCornerRadius(14.0);
            layer.setMasksToBounds(true);
            layer.setBorderWidth(1.0);
            layer.setBorderColor(Some(&NSColor::separatorColor().CGColor()));
        }
        let inner = rect(16.0, 16.0, w - 32.0, h - 32.0);
        match (item.kind, image) {
            (ItemKind::Link, _) => link_layout(&effect, item, link.as_ref(), inner, mtm),
            (ItemKind::Image | ItemKind::File, Some(image)) => {
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
                // SAFETY: the text view owns its text storage for its whole lifetime.
                if let (Some(rich), Some(storage)) = (&rich, unsafe { tv.textStorage() }) {
                    storage.setAttributedString(rich);
                }
                scroll.setDocumentView(Some(&tv));
                effect.addSubview(&scroll);
            }
        }
        self.panel.setContentView(Some(&effect));
        self.item_id = Some(item.id);
        self.panel.setAnimationBehavior(super::a11y::animation_behavior());
        self.panel.orderFrontRegardless();
        let spoken = if item.sensitive {
            "Preview of sensitive item. The text is shown on screen.".to_string()
        } else {
            format!("Preview. {}", crate::model::truncate_chars(&item.text, 400))
        };
        super::a11y::announce(&self.panel, &spoken);
    }
}
