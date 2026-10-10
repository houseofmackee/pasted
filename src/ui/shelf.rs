//! The bottom-of-screen panel that shows history and pinboards.

use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSImage, NSMenu, NSMenuItem, NSPanel, NSResponder, NSScreen, NSScrollView, NSSearchField, NSTextField, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindowCollectionBehavior, NSWindowSharingType, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSMutableAttributedString, NSObjectProtocol, NSRange, NSRect,
};

use super::card::{self, CARD_H, CARD_W, CardContent, CardView};
use super::icons::IconCache;
use super::{argb, label, ns, rect};
use crate::model::{IMAGE_UTIS, Item, ItemKind, Pinboard};
use crate::search::Query;
use crate::store::Store;

const PANEL_H: f64 = 330.0;
const TOP_BAR_H: f64 = 50.0;
const GAP: f64 = 14.0;
const MARGIN: f64 = 8.0;
const PAGE: usize = 150;
const STATUS_WINDOW_LEVEL: isize = 25;

define_class!(
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "PastedShelfPanel"]
    pub struct ShelfPanel;

    impl ShelfPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            true
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            crate::app::shelf_key_equivalent(event) || unsafe { msg_send![super(self), performKeyEquivalent: event] }
        }

        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            crate::app::shelf_escape();
        }
    }

    unsafe impl NSObjectProtocol for ShelfPanel {}
);

/// Menu and button tags pack an action code and an argument.
pub fn tag(code: u8, arg: i64) -> isize {
    ((code as i64) << 40 | (arg & 0xFF_FFFF_FFFF)) as isize
}

pub fn untag(tag: isize) -> (u8, i64) {
    let tag = tag as i64;
    let arg = tag & 0xFF_FFFF_FFFF;
    // Sign-extend so negative arguments survive.
    let arg = if arg & 0x80_0000_0000 != 0 { arg | !0xFF_FFFF_FFFF } else { arg };
    ((tag >> 40) as u8, arg)
}

pub struct Shelf {
    mtm: MainThreadMarker,
    pub panel: Retained<ShelfPanel>,
    pub search: Retained<NSSearchField>,
    tabs: Retained<NSView>,
    scroll: Retained<NSScrollView>,
    doc: Retained<NSView>,
    count: Retained<NSTextField>,
    cards: Vec<Retained<CardView>>,
    pub items: Vec<Item>,
    pub selected: usize,
    /// `None` shows clipboard history.
    pub board: Option<i64>,
    pub boards: Vec<Pinboard>,
    more_available: bool,
    query: Query,
    icons: IconCache,
    images: HashMap<i64, Retained<NSImage>>,
    rich: HashMap<i64, Retained<NSAttributedString>>,
    target: Retained<crate::app::Target>,
}

impl Shelf {
    pub fn new(target: &crate::app::Target, mtm: MainThreadMarker) -> Shelf {
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel: Retained<ShelfPanel> = unsafe {
            msg_send![ShelfPanel::alloc(mtm), initWithContentRect: rect(0.0, 0.0, 800.0, PANEL_H),
                styleMask: style, backing: NSBackingStoreType::Buffered, defer: false]
        };
        // SAFETY: the panel is kept alive by the Shelf.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setLevel(STATUS_WINDOW_LEVEL);
        panel.setFloatingPanel(true);
        panel.setHidesOnDeactivate(false);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(true);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::MoveToActiveSpace
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Transient,
        );

        let effect = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), rect(0.0, 0.0, 800.0, PANEL_H));
        effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::Active);
        effect.setWantsLayer(true);
        if let Some(layer) = effect.layer() {
            layer.setCornerRadius(16.0);
            layer.setMasksToBounds(true);
        }
        panel.setContentView(Some(&effect));

        let search = NSSearchField::initWithFrame(NSSearchField::alloc(mtm), rect(16.0, 0.0, 260.0, 28.0));
        search.setPlaceholderString(Some(&ns("Search  (type: app: after: board:)")));
        // SAFETY: the target outlives the search field (both live for the whole app).
        unsafe { search.setDelegate(Some(ProtocolObject::from_ref(target))) };
        effect.addSubview(&search);

        let tabs = NSView::initWithFrame(NSView::alloc(mtm), rect(292.0, 0.0, 400.0, 28.0));
        effect.addSubview(&tabs);

        let count = label("", 12.0, true, mtm);
        count.setTextColor(Some(&NSColor::labelColor()));
        effect.addSubview(&count);

        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 800.0, PANEL_H - TOP_BAR_H));
        scroll.setDrawsBackground(false);
        scroll.setHasHorizontalScroller(true);
        scroll.setHasVerticalScroller(false);
        scroll.setAutohidesScrollers(true);
        let doc = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 800.0, PANEL_H - TOP_BAR_H));
        scroll.setDocumentView(Some(&doc));
        let clip = scroll.contentView();
        clip.setPostsBoundsChangedNotifications(true);
        // SAFETY: the target implements `shelfScrolled:` and lives for the whole app.
        unsafe {
            objc2_foundation::NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                target,
                sel!(shelfScrolled:),
                Some(objc2_app_kit::NSViewBoundsDidChangeNotification),
                Some(&clip),
            )
        };
        effect.addSubview(&scroll);

        Shelf {
            mtm,
            panel,
            search,
            tabs,
            scroll,
            doc,
            count,
            cards: Vec::new(),
            items: Vec::new(),
            selected: 0,
            board: None,
            boards: Vec::new(),
            more_available: false,
            query: Query::default(),
            icons: IconCache::default(),
            images: HashMap::new(),
            rich: HashMap::new(),
            target: target.retain(),
        }
    }

    pub fn is_visible(&self) -> bool {
        self.panel.isVisible()
    }

    pub fn set_visible_in_screen_sharing(&self, visible: bool) {
        self.panel.setSharingType(if visible { NSWindowSharingType::ReadOnly } else { NSWindowSharingType::None });
    }

    /// Lays the panel out along the bottom of the screen under the mouse.
    fn position(&self) {
        let mouse = NSEvent::mouseLocation();
        let screens = NSScreen::screens(self.mtm);
        let screen = screens
            .iter()
            .find(|s| {
                let f = s.frame();
                mouse.x >= f.origin.x
                    && mouse.x <= f.origin.x + f.size.width
                    && mouse.y >= f.origin.y
                    && mouse.y <= f.origin.y + f.size.height
            })
            .or_else(|| NSScreen::mainScreen(self.mtm));
        let Some(screen) = screen else { return };
        let vf: NSRect = screen.visibleFrame();
        let w = vf.size.width - MARGIN * 2.0;
        self.panel.setFrame_display(rect(vf.origin.x + MARGIN, vf.origin.y + MARGIN, w, PANEL_H), true);
        let Some(content) = self.panel.contentView() else { return };
        content.setFrame(rect(0.0, 0.0, w, PANEL_H));
        let bar_y = PANEL_H - TOP_BAR_H + 11.0;
        self.search.setFrame(rect(16.0, bar_y, 260.0, 28.0));
        self.tabs.setFrame(rect(292.0, bar_y, (w - 292.0 - 150.0).max(100.0), 28.0));
        self.count.setFrame(rect(w - 146.0, bar_y + 5.0, 130.0, 18.0));
        self.scroll.setFrame(rect(0.0, 0.0, w, PANEL_H - TOP_BAR_H));
    }

    pub fn show(&mut self, store: &Store) {
        self.search.setStringValue(&ns(""));
        self.query = Query { board_id: self.board, ..Query::default() };
        self.position();
        self.reload_tabs(store);
        self.reload(store);
        self.panel.orderFrontRegardless();
        self.panel.makeKeyWindow();
        let responder: &NSResponder = &self.search;
        self.panel.makeFirstResponder(Some(responder));
    }

    pub fn hide(&self) {
        self.panel.orderOut(None);
    }

    pub fn set_query(&mut self, text: &str, store: &Store) {
        let mut q = Query::parse(text);
        if q.board_name.is_none() {
            q.board_id = self.board;
        }
        self.query = q;
        self.reload(store);
    }

    pub fn search_text(&self) -> String {
        self.search.stringValue().to_string()
    }

    pub fn reload_tabs(&mut self, store: &Store) {
        self.boards = store.pinboards().unwrap_or_default();
        if let Some(b) = self.board
            && !self.boards.iter().any(|p| p.id == b)
        {
            self.board = None;
        }
        for v in self.tabs.subviews().iter() {
            v.removeFromSuperview();
        }
        let mut x = 0.0;
        let target: &AnyObject = &self.target;
        let mut add = |title: &str, tag_value: isize, on: bool, color: Option<u32>, menu: Option<Retained<NSMenu>>| {
            // SAFETY: the target object implements `tabClicked:`.
            let b = unsafe {
                NSButton::buttonWithTitle_target_action(&ns(title), Some(target), Some(sel!(tabClicked:)), self.mtm)
            };
            b.setBordered(false);
            b.setTag(tag_value);
            b.setAttributedTitle(&tab_title(title, on, color));
            b.setWantsLayer(true);
            if let Some(layer) = b.layer() {
                layer.setBackgroundColor(Some(&tab_background(on).CGColor()));
                layer.setCornerRadius(TAB_H / 2.0);
                if !on {
                    layer.setBorderWidth(1.0);
                    layer.setBorderColor(Some(&NSColor::separatorColor().CGColor()));
                }
            }
            if let Some(menu) = menu {
                // SAFETY: the menu is retained by the button.
                unsafe { b.setMenu(Some(&menu)) };
            }
            b.sizeToFit();
            let width = b.frame().size.width + 24.0;
            b.setFrame(rect(x, 0.0, width, TAB_H));
            x += width + 6.0;
            self.tabs.addSubview(&b);
        };
        add("Clipboard", tag(TAB_SELECT, -1), self.board.is_none(), None, None);
        for p in &self.boards {
            let menu = board_menu(p, target, self.mtm);
            add(&p.name, tag(TAB_SELECT, p.id), self.board == Some(p.id), Some(p.color), Some(menu));
        }
        add("+ New", tag(TAB_NEW, 0), false, None, None);
    }

    pub fn select_board(&mut self, board: Option<i64>, store: &Store) {
        self.board = board;
        let text = self.search_text();
        self.reload_tabs(store);
        self.set_query(&text, store);
    }

    /// Cycles through Clipboard and the pinboards.
    pub fn cycle_board(&mut self, delta: i64, store: &Store) {
        let mut order: Vec<Option<i64>> = vec![None];
        order.extend(self.boards.iter().map(|b| Some(b.id)));
        let pos = order.iter().position(|b| *b == self.board).unwrap_or(0) as i64;
        let next = (pos + delta).rem_euclid(order.len() as i64) as usize;
        self.select_board(order[next], store);
    }

    fn image_for(&mut self, item: &Item, store: &Store) -> Option<Retained<NSImage>> {
        if item.kind != ItemKind::Image {
            return None;
        }
        if let Some(img) = self.images.get(&item.id) {
            return Some(img.clone());
        }
        let data = IMAGE_UTIS.iter().find_map(|uti| store.representation(item.id, uti).ok().flatten())?;
        let img = NSImage::initWithData(NSImage::alloc(), &objc2_foundation::NSData::with_bytes(&data))?;
        if self.images.len() > 400 {
            self.images.clear();
        }
        self.images.insert(item.id, img.clone());
        Some(img)
    }

    pub fn reload(&mut self, store: &Store) {
        self.items = store.search(&self.query, PAGE, 0).unwrap_or_else(|e| {
            tracing::error!("search failed: {e:#}");
            Vec::new()
        });
        self.more_available = self.items.len() == PAGE;
        self.selected = 0;
        for c in self.cards.drain(..) {
            c.removeFromSuperview();
        }
        self.resize_doc();
        let clip = self.scroll.contentView();
        clip.scrollToPoint(objc2_foundation::NSPoint::new(0.0, 0.0));
        self.scroll.reflectScrolledClipView(&clip);
        self.ensure_visible_built(store);
        self.update_count();
        self.select(0);
    }

    fn resize_doc(&self) {
        let width = 32.0 + self.items.len() as f64 * (CARD_W + GAP);
        self.doc.setFrame(rect(0.0, 0.0, width.max(self.scroll.frame().size.width), PANEL_H - TOP_BAR_H));
    }

    /// Builds cards up to and including `upto`. Cards are only realized
    /// when they come close to the visible area.
    fn ensure_built(&mut self, upto: usize, store: &Store) {
        let upto = upto.min(self.items.len().saturating_sub(1));
        if self.items.is_empty() || self.cards.len() > upto {
            return;
        }
        let y = ((PANEL_H - TOP_BAR_H - CARD_H) / 2.0).max(0.0) + 4.0;
        for i in self.cards.len()..=upto {
            let item = self.items[i].clone();
            let image = self.image_for(&item, store);
            let link = match (&item.kind, &item.meta.url) {
                (ItemKind::Link, Some(url)) => store.link_preview(url).ok().flatten(),
                _ => None,
            };
            let rich = self.rich_for(&item, store);
            let content =
                CardContent { item: &item, index: i, image, link, rich, shortcut_number: (i < 9).then_some(i + 1) };
            let card = card::build(content, &mut self.icons, self.mtm);
            card.setFrame(rect(16.0 + i as f64 * (CARD_W + GAP), y, CARD_W, CARD_H));
            if i == self.selected {
                card.set_selected(true);
            }
            self.doc.addSubview(&card);
            self.cards.push(card);
        }
    }

    /// Called when the shelf scrolls: realize cards that are about to appear.
    pub fn ensure_visible_built(&mut self, store: &Store) {
        let visible = self.scroll.contentView().bounds();
        let right = visible.origin.x + visible.size.width;
        let last = (right / (CARD_W + GAP)).ceil() as usize + 3;
        if last + 10 >= self.items.len() {
            self.load_more(store);
        }
        self.ensure_built(last, store);
    }

    fn load_more(&mut self, store: &Store) {
        if !self.more_available {
            return;
        }
        let more = store.search(&self.query, PAGE, self.items.len()).unwrap_or_default();
        self.more_available = more.len() == PAGE;
        self.items.extend(more);
        self.resize_doc();
        self.update_count();
    }

    fn update_count(&self) {
        let n = self.items.len();
        let text = if n == 0 {
            "No items".to_string()
        } else if self.more_available {
            format!("{n}+ items")
        } else {
            format!("{n} item{}", if n == 1 { "" } else { "s" })
        };
        self.count.setStringValue(&ns(&text));
    }

    pub fn select(&mut self, index: usize) {
        if self.items.is_empty() {
            return;
        }
        let index = index.min(self.items.len() - 1);
        if let Some(old) = self.cards.get(self.selected) {
            old.set_selected(false);
        }
        self.selected = index;
        let card = &self.cards[index];
        card.set_selected(true);
        let mut f = card.frame();
        f.origin.x -= GAP;
        f.size.width += GAP * 2.0;
        self.doc.scrollRectToVisible(f);
    }

    pub fn move_selection(&mut self, delta: isize, store: &Store) {
        if self.items.is_empty() {
            return;
        }
        let target = (self.selected as isize + delta).max(0) as usize;
        if target + 10 >= self.items.len() {
            self.load_more(store);
        }
        let target = target.min(self.items.len() - 1);
        self.ensure_built(target + 3, store);
        self.select(target);
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    pub fn forget_image(&mut self, id: i64) {
        self.images.remove(&id);
        self.rich.remove(&id);
    }

    fn rich_for(&mut self, item: &Item, store: &Store) -> Option<Retained<NSAttributedString>> {
        if item.kind != ItemKind::RichText {
            return None;
        }
        if let Some(r) = self.rich.get(&item.id) {
            return Some(r.clone());
        }
        let r = super::rich_text::render(store, item, super::rich_text::Target::Card, self.mtm)?;
        if self.rich.len() > 400 {
            self.rich.clear();
        }
        self.rich.insert(item.id, r.clone());
        Some(r)
    }
}

const TAB_H: f64 = 28.0;

/// Tabs use opaque backgrounds so their contrast does not depend on what is
/// behind the translucent panel: the selected tab is near-black with light
/// text (inverted in dark mode), others are a light pill with dark text.
fn tab_background(selected: bool) -> Retained<NSColor> {
    if selected { NSColor::labelColor() } else { NSColor::controlBackgroundColor() }
}

/// Tab label, with a dot in the pinboard's color when there is one.
fn tab_title(name: &str, selected: bool, color: Option<u32>) -> Retained<NSAttributedString> {
    let text = match color {
        Some(_) => format!("\u{25cf} {name}"),
        None => name.to_string(),
    };
    let text = NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &ns(&text));
    // SAFETY: reading an AppKit constant.
    let font = NSFont::systemFontOfSize_weight(13.0, unsafe { objc2_app_kit::NSFontWeightSemibold });
    let fg = if selected { NSColor::windowBackgroundColor() } else { NSColor::labelColor() };
    let all = NSRange::new(0, text.length());
    // SAFETY: standard attribute keys with values of the documented types.
    unsafe {
        text.addAttribute_value_range(NSFontAttributeName, &font, all);
        text.addAttribute_value_range(NSForegroundColorAttributeName, &fg, all);
        if let Some(c) = color {
            text.addAttribute_value_range(NSForegroundColorAttributeName, &argb(c), NSRange::new(0, 1));
        }
    }
    Retained::into_super(text)
}

pub const TAB_SELECT: u8 = 1;
pub const TAB_NEW: u8 = 2;
pub const BOARD_RENAME: u8 = 3;
pub const BOARD_DELETE: u8 = 4;
pub const BOARD_COLOR: u8 = 5;

fn board_menu(board: &Pinboard, target: &AnyObject, mtm: MainThreadMarker) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    let add = |title: &str, t: isize| {
        let item = menu_item(title, t, target, mtm);
        menu.addItem(&item);
    };
    add("Rename Pinboard\u{2026}", tag(BOARD_RENAME, board.id));
    let colors = NSMenu::new(mtm);
    for (i, (name, _)) in BOARD_COLOR_NAMES.iter().enumerate() {
        colors.addItem(&menu_item(name, tag(BOARD_COLOR, board.id << 8 | i as i64), target, mtm));
    }
    let color_item = menu_item("Color", 0, target, mtm);
    color_item.setSubmenu(Some(&colors));
    menu.addItem(&color_item);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    add("Delete Pinboard\u{2026}", tag(BOARD_DELETE, board.id));
    menu
}

pub const BOARD_COLOR_NAMES: &[(&str, u32)] = &[
    ("Red", 0xFFFF3B30),
    ("Orange", 0xFFFF9500),
    ("Yellow", 0xFFFFCC00),
    ("Green", 0xFF34C759),
    ("Teal", 0xFF00C7BE),
    ("Blue", 0xFF007AFF),
    ("Indigo", 0xFF5856D6),
    ("Purple", 0xFFAF52DE),
    ("Pink", 0xFFFF2D55),
    ("Gray", 0xFF8E8E93),
];

pub fn menu_item(title: &str, tag_value: isize, target: &AnyObject, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    // SAFETY: `menuAction:` is implemented by the target.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &ns(title),
            Some(sel!(menuAction:)),
            &ns(""),
        )
    };
    // SAFETY: the target lives for the whole app lifetime.
    unsafe { item.setTarget(Some(target)) };
    item.setTag(tag_value);
    item
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_round_trip() {
        for (code, arg) in [(1u8, -1i64), (2, 0), (5, 1234 << 8 | 7), (200, 99_999_999)] {
            assert_eq!(untag(tag(code, arg)), (code, arg));
        }
    }
}
