//! One clip rendered as a card in the shelf.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSColor, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent, NSFont,
    NSFontWeightRegular, NSImage, NSImageScaling, NSImageView, NSMenu, NSTextAlignment, NSView,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSObjectProtocol};

use super::icons::{AppLook, IconCache};
use super::{label, rect, wrapping_label};
use crate::clipboard::classify::parse_color;
use crate::model::{Item, ItemKind, relative_time, truncate_chars};

pub const CARD_W: f64 = 232.0;
pub const CARD_H: f64 = 248.0;
const HEADER_H: f64 = 54.0;
const FOOTER_H: f64 = 26.0;
const PAD: f64 = 12.0;

pub struct CardIvars {
    index: Cell<usize>,
    drag_started: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PastedCardView"]
    #[ivars = CardIvars]
    pub struct CardView;

    impl CardView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().drag_started.set(false);
            crate::app::card_clicked(self.ivars().index.get(), event.clickCount());
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            if self.ivars().drag_started.replace(true) {
                return;
            }
            self.begin_drag(event);
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, _event: &NSEvent) -> Option<Retained<NSMenu>> {
            crate::app::card_menu(self.ivars().index.get())
        }
    }

    unsafe impl NSObjectProtocol for CardView {}

    unsafe impl NSDraggingSource for CardView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy
        }
    }
);

impl CardView {
    fn new(index: usize, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CardIvars { index: Cell::new(index), drag_started: Cell::new(false) });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, CARD_W, CARD_H)] };
        this.setWantsLayer(true);
        if let Some(layer) = this.layer() {
            layer.setCornerRadius(12.0);
            layer.setMasksToBounds(true);
            layer.setBackgroundColor(Some(&NSColor::textBackgroundColor().CGColor()));
        }
        this
    }

    pub fn index(&self) -> usize {
        self.ivars().index.get()
    }

    pub fn set_selected(&self, selected: bool) {
        if let Some(layer) = self.layer() {
            layer.setBorderWidth(if selected { 4.0 } else { 0.0 });
            layer.setBorderColor(Some(&NSColor::controlAccentColor().CGColor()));
        }
    }

    fn snapshot(&self) -> Option<Retained<NSImage>> {
        let bounds = self.bounds();
        let rep = self.bitmapImageRepForCachingDisplayInRect(bounds)?;
        self.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        let image = NSImage::initWithSize(NSImage::alloc(), bounds.size);
        image.addRepresentation(&rep);
        Some(image)
    }

    fn begin_drag(&self, event: &NSEvent) {
        let Some(writer) = crate::app::card_drag_item(self.index()) else { return };
        let item =
            NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), &ProtocolObject::from_retained(writer));
        let image = self.snapshot();
        // SAFETY: the image (if any) is a valid NSImage.
        unsafe { item.setDraggingFrame_contents(self.bounds(), image.as_deref().map(|i| i as _)) };
        self.beginDraggingSessionWithItems_event_source(
            &NSArray::from_retained_slice(&[item]),
            event,
            ProtocolObject::from_ref(self),
        );
    }
}

pub struct CardContent<'a> {
    pub item: &'a Item,
    pub index: usize,
    pub image: Option<Retained<NSImage>>,
    pub shortcut_number: Option<usize>,
}

fn footer_text(item: &Item) -> String {
    match item.kind {
        ItemKind::Image => match (item.meta.width, item.meta.height) {
            (Some(w), Some(h)) => format!("{w} \u{00d7} {h}"),
            _ => "Image".into(),
        },
        ItemKind::File => match item.meta.files.len() {
            1 => item.meta.files[0].rsplit('/').next().unwrap_or("").to_string(),
            n => format!("{n} files"),
        },
        ItemKind::Link => item
            .meta
            .url
            .as_deref()
            .and_then(|u| u.split("://").nth(1))
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("Link")
            .to_string(),
        ItemKind::Color => "Color".into(),
        ItemKind::Code => {
            let lines = item.text.lines().count();
            let lang = item.meta.language.as_deref().filter(|l| *l != "code").unwrap_or("Code");
            format!("{lang} \u{00b7} {lines} lines")
        }
        ItemKind::Text | ItemKind::RichText => {
            let n = item.text.chars().count();
            format!("{n} character{}", if n == 1 { "" } else { "s" })
        }
    }
}

pub fn build(content: CardContent, icons: &mut IconCache, mtm: MainThreadMarker) -> Retained<CardView> {
    let item = content.item;
    let card = CardView::new(content.index, mtm);
    let (w, h) = (CARD_W, CARD_H);
    // AppKit's origin is bottom-left; `top` converts from a top-left layout.
    let top = |y: f64, height: f64| h - y - height;

    // Header in the source app's color.
    let AppLook { icon, color } = icons.app(item.app_bundle_id.as_deref());
    let header = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, top(0.0, HEADER_H), w, HEADER_H));
    header.setWantsLayer(true);
    if let Some(layer) = header.layer() {
        layer.setBackgroundColor(Some(&color.CGColor()));
    }
    let heading = item.title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| item.kind.label().to_string());
    let title = label(&heading, 14.0, true, mtm);
    title.setTextColor(Some(&NSColor::whiteColor()));
    title.setFrame(rect(PAD, 24.0, w - PAD * 2.0 - 44.0, 20.0));
    header.addSubview(&title);
    let time = label(&relative_time(item.timestamp), 11.0, false, mtm);
    time.setTextColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.92)));
    time.setFrame(rect(PAD, 7.0, w - PAD * 2.0 - 44.0, 16.0));
    header.addSubview(&time);
    if let Some(icon) = icon {
        let iv = NSImageView::imageViewWithImage(icon, mtm);
        iv.setFrame(rect(w - 46.0, 7.0, 40.0, 40.0));
        header.addSubview(&iv);
    }
    card.addSubview(&header);

    // Body.
    let body_y = HEADER_H + 8.0;
    let body_h = h - body_y - FOOTER_H;
    let body = rect(PAD, top(body_y, body_h), w - PAD * 2.0, body_h);
    match item.kind {
        ItemKind::Image => {
            if let Some(image) = &content.image {
                let iv = NSImageView::imageViewWithImage(image, mtm);
                iv.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                iv.setFrame(body);
                card.addSubview(&iv);
            }
        }
        ItemKind::Color => {
            let swatch =
                NSView::initWithFrame(NSView::alloc(mtm), rect(PAD, top(body_y + 6.0, 110.0), w - PAD * 2.0, 110.0));
            swatch.setWantsLayer(true);
            if let (Some(layer), Some([r, g, b, a])) =
                (swatch.layer(), item.meta.color.as_deref().and_then(parse_color))
            {
                layer.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a).CGColor()));
                layer.setCornerRadius(10.0);
                layer.setBorderWidth(1.0);
                layer.setBorderColor(Some(&NSColor::separatorColor().CGColor()));
            }
            card.addSubview(&swatch);
            let hex = label(item.meta.color.as_deref().unwrap_or(""), 15.0, true, mtm);
            hex.setAlignment(NSTextAlignment::Center);
            hex.setFrame(rect(PAD, top(body_y + 128.0, 22.0), w - PAD * 2.0, 22.0));
            card.addSubview(&hex);
        }
        ItemKind::File => {
            if let Some(first) = item.meta.files.first() {
                let iv = NSImageView::imageViewWithImage(&IconCache::file_icon(first), mtm);
                iv.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                iv.setFrame(rect(PAD, top(body_y + 4.0, 100.0), w - PAD * 2.0, 100.0));
                card.addSubview(&iv);
            }
            let names: Vec<&str> = item.meta.files.iter().map(|f| f.rsplit('/').next().unwrap_or(f)).collect();
            let l = wrapping_label(&names.join("\n"), &NSFont::systemFontOfSize(12.0), 3, mtm);
            l.setAlignment(NSTextAlignment::Center);
            l.setFrame(rect(PAD, top(body_y + 110.0, body_h - 110.0), w - PAD * 2.0, body_h - 110.0));
            card.addSubview(&l);
        }
        ItemKind::Link => {
            let name = item.meta.url_name.clone().or_else(|| item.title.clone()).unwrap_or_default();
            let url = item.meta.url.clone().unwrap_or_else(|| item.text.trim().to_string());
            let mut y = body_y;
            if !name.is_empty() {
                let l = wrapping_label(&name, &NSFont::boldSystemFontOfSize(13.0), 3, mtm);
                l.setFrame(rect(PAD, top(y, 54.0), w - PAD * 2.0, 54.0));
                card.addSubview(&l);
                y += 58.0;
            }
            let l = wrapping_label(&truncate_chars(&url, 400), &NSFont::systemFontOfSize(12.0), 6, mtm);
            l.setTextColor(Some(&NSColor::linkColor()));
            l.setFrame(rect(PAD, top(y, body_h - (y - body_y)), w - PAD * 2.0, body_h - (y - body_y)));
            card.addSubview(&l);
        }
        ItemKind::Text | ItemKind::RichText | ItemKind::Code => {
            let font = if item.kind == ItemKind::Code {
                // SAFETY: reading an AppKit constant.
                NSFont::monospacedSystemFontOfSize_weight(11.0, unsafe { NSFontWeightRegular })
            } else {
                NSFont::systemFontOfSize(12.5)
            };
            let text = truncate_chars(&item.text, 700);
            let l = wrapping_label(&text, &font, 11, mtm);
            l.setFrame(body);
            card.addSubview(&l);
        }
    }

    // Footer.
    let footer = label(&footer_text(item), 11.0, false, mtm);
    footer.setTextColor(Some(&NSColor::secondaryLabelColor()));
    footer.setFrame(rect(PAD, 6.0, w - PAD * 2.0 - 34.0, 16.0));
    card.addSubview(&footer);
    if let Some(n) = content.shortcut_number {
        let k = label(&format!("\u{2318}{n}"), 11.0, false, mtm);
        k.setTextColor(Some(&NSColor::tertiaryLabelColor()));
        k.setAlignment(NSTextAlignment::Right);
        k.setFrame(rect(w - PAD - 32.0, 6.0, 32.0, 16.0));
        card.addSubview(&k);
    }
    card
}
