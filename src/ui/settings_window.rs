//! The Settings window. Every change is applied and saved immediately; the
//! TOML file stays the source of truth and can still be edited by hand.

use std::cell::{Cell, OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSEvent,
    NSEventModifierFlags, NSFont, NSPopUpButton, NSResponder, NSScrollView, NSTabView, NSTabViewItem, NSTableColumn,
    NSTableView, NSTableViewDataSource, NSTextAlignment, NSTextField, NSTextView, NSView, NSWindow, NSWindowStyleMask,
    NSWorkspace,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSInteger, NSObject, NSObjectProtocol, NSString};

use super::{label, ns, rect, wrapping_label};
use crate::settings::{Retention, Settings};
use crate::shortcut::{Modifiers, Shortcut};

const W: f64 = 620.0;
const H: f64 = 520.0;
const LABEL_W: f64 = 190.0;
const CONTROL_X: f64 = 210.0;

const RETENTIONS: &[(&str, Retention)] = &[
    ("1 Day", Retention::Day),
    ("1 Week", Retention::Week),
    ("1 Month", Retention::Month),
    ("1 Year", Retention::Year),
    ("Forever", Retention::Forever),
];
const SIZES_MB: &[u64] = &[8, 16, 32, 64, 128, 256, 512];

// Button tags handled by `app::dispatch`.
pub const ADD_APP: u8 = 60;
pub const REMOVE_APP: u8 = 61;
pub const OPEN_ACCESSIBILITY: u8 = 62;
pub const RESTORE_SHORTCUTS: u8 = 63;
pub const SAVE_PATTERNS: u8 = 64;
pub const OPEN_DATA_FOLDER: u8 = 65;

pub struct RecorderIvars {
    shortcut: Cell<Option<Shortcut>>,
    recording: Cell<bool>,
    /// Global hotkeys need a modifier; in-shelf shortcuts need Command.
    require_command: bool,
    text: OnceCell<Retained<NSTextField>>,
}

define_class!(
    /// Click, then press a key combination. Esc cancels.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PastedShortcutRecorder"]
    #[ivars = RecorderIvars]
    pub struct ShortcutRecorder;

    impl ShortcutRecorder {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if let Some(window) = self.window() {
                let responder: &NSResponder = self;
                window.makeFirstResponder(Some(responder));
            }
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            self.ivars().recording.set(true);
            crate::app::set_hotkeys_suspended(true);
            self.refresh();
            true
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            self.ivars().recording.set(false);
            crate::app::set_hotkeys_suspended(false);
            self.refresh();
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.handle(event);
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            if self.ivars().recording.get() {
                self.handle(event);
                true
            } else {
                false
            }
        }
    }

    unsafe impl NSObjectProtocol for ShortcutRecorder {}
);

impl ShortcutRecorder {
    fn new(require_command: bool, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RecorderIvars {
            shortcut: Cell::new(None),
            recording: Cell::new(false),
            require_command,
            text: OnceCell::new(),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 180.0, 26.0)] };
        this.setWantsLayer(true);
        if let Some(layer) = this.layer() {
            layer.setCornerRadius(6.0);
            layer.setBorderWidth(1.0);
        }
        let text = label("", 13.0, false, mtm);
        text.setAlignment(NSTextAlignment::Center);
        text.setFrame(rect(4.0, 4.0, 172.0, 18.0));
        this.addSubview(&text);
        let _ = this.ivars().text.set(text);
        this.refresh();
        this
    }

    pub fn shortcut(&self) -> Option<Shortcut> {
        self.ivars().shortcut.get()
    }

    pub fn set_shortcut(&self, s: Shortcut) {
        self.ivars().shortcut.set(Some(s));
        self.refresh();
    }

    fn refresh(&self) {
        let recording = self.ivars().recording.get();
        if let Some(text) = self.ivars().text.get() {
            let value = if recording {
                "Type shortcut\u{2026}".to_string()
            } else {
                self.shortcut().map(|s| s.display()).unwrap_or_else(|| "None".into())
            };
            text.setStringValue(&ns(&value));
        }
        if let Some(layer) = self.layer() {
            let color = if recording { NSColor::controlAccentColor() } else { NSColor::separatorColor() };
            layer.setBorderColor(Some(&color.CGColor()));
            layer.setBorderWidth(if recording { 2.0 } else { 1.0 });
        }
    }

    fn finish(&self) {
        if let Some(window) = self.window() {
            window.makeFirstResponder(None);
        }
    }

    fn handle(&self, event: &NSEvent) {
        let flags = event.modifierFlags();
        let modifiers = Modifiers {
            cmd: flags.contains(NSEventModifierFlags::Command),
            shift: flags.contains(NSEventModifierFlags::Shift),
            option: flags.contains(NSEventModifierFlags::Option),
            control: flags.contains(NSEventModifierFlags::Control),
        };
        let code = event.keyCode();
        let no_modifiers = !(modifiers.cmd || modifiers.option || modifiers.control);
        if code == 53 && no_modifiers && !modifiers.shift {
            self.finish();
            return;
        }
        let valid = if self.ivars().require_command { modifiers.cmd } else { !no_modifiers };
        if !valid || crate::shortcut::key_name(code).is_none() {
            if let Some(text) = self.ivars().text.get() {
                let hint = if self.ivars().require_command {
                    "Include \u{2318}"
                } else {
                    "Include \u{2318}, \u{2325} or \u{2303}"
                };
                text.setStringValue(&ns(hint));
            }
            return;
        }
        self.ivars().shortcut.set(Some(Shortcut { key_code: code, modifiers }));
        self.finish();
        crate::app::settings_ui_changed();
    }
}

define_class!(
    /// Rows of the excluded apps table: display name and bundle identifier.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PastedAppsDataSource"]
    #[ivars = RefCell<Vec<(String, String)>>]
    pub struct AppsDataSource;

    unsafe impl NSObjectProtocol for AppsDataSource {}

    unsafe impl NSTableViewDataSource for AppsDataSource {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().borrow().len() as NSInteger
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn object_value(
            &self,
            _table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<AnyObject>> {
            let rows = self.ivars().borrow();
            rows.get(row as usize).map(|(name, id)| {
                let text = if name == id { id.clone() } else { format!("{name}  ({id})") };
                Retained::into_super(Retained::into_super(NSString::from_str(&text)))
            })
        }
    }
);

impl AppsDataSource {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(Vec::new()));
        unsafe { msg_send![super(this), init] }
    }
}

/// Human readable app name for a bundle identifier, if the app is installed.
pub fn app_name(bundle_id: &str) -> String {
    NSWorkspace::sharedWorkspace()
        .URLForApplicationWithBundleIdentifier(&ns(bundle_id))
        .and_then(|url| url.lastPathComponent())
        .map(|n| n.to_string().trim_end_matches(".app").to_string())
        .unwrap_or_else(|| bundle_id.to_string())
}

pub struct SettingsWindow {
    mtm: MainThreadMarker,
    pub window: Retained<NSWindow>,
    tabs: Retained<NSTabView>,
    retention: Retained<NSPopUpButton>,
    max_size: Retained<NSPopUpButton>,
    paste_directly: Retained<NSButton>,
    plain_default: Retained<NSButton>,
    sounds: Retained<NSButton>,
    launch_at_login: Retained<NSButton>,
    link_previews: Retained<NSButton>,
    trim_whitespace: Retained<NSButton>,
    clean_links: Retained<NSButton>,
    accessibility: Retained<NSTextField>,
    login_note: Retained<NSTextField>,
    open_shelf: Retained<ShortcutRecorder>,
    paste_stack: Retained<ShortcutRecorder>,
    next_board: Retained<ShortcutRecorder>,
    previous_board: Retained<ShortcutRecorder>,
    shortcut_warning: Retained<NSTextField>,
    ignore_concealed: Retained<NSButton>,
    ignore_secrets: Retained<NSButton>,
    visible_sharing: Retained<NSButton>,
    apps_table: Retained<NSTableView>,
    apps: Retained<AppsDataSource>,
    patterns: Retained<NSTextView>,
    patterns_status: Retained<NSTextField>,
    stats: Retained<NSTextField>,
    paste_status: Retained<NSTextField>,
    skip_excluded_apps: Retained<NSButton>,
    skip_secrets: Retained<NSButton>,
    skip_ignore_patterns: Retained<NSButton>,
    skip_password_mentions: Retained<NSButton>,
}

/// Lays out rows top to bottom inside one tab.
struct Form {
    view: Retained<NSView>,
    y: f64,
    mtm: MainThreadMarker,
}

impl Form {
    fn new(mtm: MainThreadMarker) -> Form {
        let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, W - 40.0, H - 90.0));
        Form { view, y: H - 110.0, mtm }
    }

    /// Adds a right-aligned row label and returns the y of the row.
    fn row(&mut self, title: &str, height: f64) -> f64 {
        self.y -= height;
        if !title.is_empty() {
            let l = label(title, 13.0, false, self.mtm);
            l.setAlignment(NSTextAlignment::Right);
            l.setFrame(rect(0.0, self.y + height - 22.0, LABEL_W, 18.0));
            self.view.addSubview(&l);
        }
        let y = self.y;
        self.y -= 10.0;
        y
    }

    fn add(&self, view: &NSView) {
        self.view.addSubview(view);
    }

    fn note(&mut self, text: &str, x: f64) -> Retained<NSTextField> {
        let l = wrapping_label(text, &NSFont::systemFontOfSize(11.0), 3, self.mtm);
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let y = self.row("", 44.0);
        l.setFrame(rect(x, y, W - 80.0 - x, 44.0));
        self.add(&l);
        l
    }
}

fn checkbox(title: &str, target: &AnyObject, mtm: MainThreadMarker) -> Retained<NSButton> {
    // SAFETY: the target implements `settingsChanged:`.
    unsafe { NSButton::checkboxWithTitle_target_action(&ns(title), Some(target), Some(sel!(settingsChanged:)), mtm) }
}

fn button(title: &str, tag_value: isize, target: &AnyObject, mtm: MainThreadMarker) -> Retained<NSButton> {
    // SAFETY: the target implements `buttonAction:`.
    let b =
        unsafe { NSButton::buttonWithTitle_target_action(&ns(title), Some(target), Some(sel!(buttonAction:)), mtm) };
    b.setTag(tag_value);
    b.sizeToFit();
    b
}

fn popup(items: &[String], target: &AnyObject, mtm: MainThreadMarker) -> Retained<NSPopUpButton> {
    let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(0.0, 0.0, 160.0, 26.0), false);
    let titles: Vec<Retained<NSString>> = items.iter().map(|s| ns(s)).collect();
    p.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
    // SAFETY: the target implements `settingsChanged:`.
    unsafe {
        p.setTarget(Some(target));
        p.setAction(Some(sel!(settingsChanged:)));
    }
    p
}

fn set_checked(b: &NSButton, on: bool) {
    b.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
}

fn checked(b: &NSButton) -> bool {
    b.state() == NSControlStateValueOn
}

impl SettingsWindow {
    pub fn new(target: &AnyObject, delegate: &crate::app::Target, mtm: MainThreadMarker) -> SettingsWindow {
        let tag = crate::ui::shelf::tag;
        // SAFETY: standard window creation; the window is kept alive by `SettingsWindow`.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, W, H),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: owned by `SettingsWindow`.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&ns("Pasted Settings"));
        window.setDelegate(Some(ProtocolObject::from_ref(delegate)));

        let tabs = NSTabView::initWithFrame(NSTabView::alloc(mtm), rect(14.0, 12.0, W - 28.0, H - 24.0));
        let add_tab = |title: &str, view: &NSView| {
            // SAFETY: a nil identifier is allowed.
            let item = unsafe { NSTabViewItem::initWithIdentifier(NSTabViewItem::alloc(), None) };
            item.setLabel(&ns(title));
            item.setView(Some(view));
            tabs.addTabViewItem(&item);
        };

        // General
        let mut f = Form::new(mtm);
        let retention = popup(&RETENTIONS.iter().map(|(l, _)| l.to_string()).collect::<Vec<_>>(), target, mtm);
        let y = f.row("Keep history for:", 26.0);
        retention.setFrame(rect(CONTROL_X, y, 160.0, 26.0));
        f.add(&retention);
        let max_size = popup(&SIZES_MB.iter().map(|m| format!("{m} MB")).collect::<Vec<_>>(), target, mtm);
        let y = f.row("Skip copies larger than:", 26.0);
        max_size.setFrame(rect(CONTROL_X, y, 160.0, 26.0));
        f.add(&max_size);
        let mut boxes = Vec::new();
        for (i, title) in [
            "Play a sound when something is copied",
            "Launch Pasted at login",
            "Show link previews (fetches page titles and images)",
        ]
        .iter()
        .enumerate()
        {
            let b = checkbox(title, target, mtm);
            let y = f.row(if i == 0 { "General:" } else { "" }, 20.0);
            b.setFrame(rect(CONTROL_X, y, W - CONTROL_X - 60.0, 20.0));
            f.add(&b);
            boxes.push(b);
        }
        let login_note = f.note("", CONTROL_X + 20.0);
        add_tab("General", &f.view);
        let [sounds, launch_at_login, link_previews]: [Retained<NSButton>; 3] =
            boxes.try_into().unwrap_or_else(|_| unreachable!());

        // Pasting
        let mut f = Form::new(mtm);
        let mut boxes = Vec::new();
        for (i, title) in [
            "Paste directly into the active app",
            "Paste as plain text by default (\u{21e7}Return does the opposite)",
            "Trim spaces and blank lines around text",
            "Remove tracking parameters from links (utm_, fbclid, \u{2026})",
        ]
        .iter()
        .enumerate()
        {
            let b = checkbox(title, target, mtm);
            let y = f.row(
                match i {
                    0 => "When pasting:",
                    2 => "Clean up automatically:",
                    _ => "",
                },
                20.0,
            );
            b.setFrame(rect(CONTROL_X, y, W - CONTROL_X - 40.0, 20.0));
            f.add(&b);
            boxes.push(b);
        }
        f.note(
            "Automatic cleanup applies to plain text and links; formatted text keeps its formatting. For one-off \
             changes, press \u{2325}Return in the shelf or right-click a card and choose Paste As.",
            CONTROL_X,
        );
        let accessibility = label("", 13.0, false, mtm);
        let y = f.row("Accessibility:", 26.0);
        accessibility.setFrame(rect(CONTROL_X, y + 4.0, 130.0, 18.0));
        f.add(&accessibility);
        let open_ax = button("Open System Settings\u{2026}", tag(OPEN_ACCESSIBILITY, 0), target, mtm);
        open_ax.setFrame(rect(CONTROL_X + 130.0, y - 2.0, open_ax.frame().size.width, 30.0));
        f.add(&open_ax);
        f.note(
            "Accessibility permission lets Pasted press \u{2318}V for you and lets Paste Stack notice pastes. \
             Without it, chosen items are only copied to the clipboard.",
            CONTROL_X,
        );
        add_tab("Pasting", &f.view);
        let [paste_directly, plain_default, trim_whitespace, clean_links]: [Retained<NSButton>; 4] =
            boxes.try_into().unwrap_or_else(|_| unreachable!());

        // Shortcuts
        let mut f = Form::new(mtm);
        let mut recorders = Vec::new();
        for (title, require_command) in
            [("Open Pasted:", false), ("Paste Stack:", false), ("Next pinboard:", true), ("Previous pinboard:", true)]
        {
            let r = ShortcutRecorder::new(require_command, mtm);
            let y = f.row(title, 26.0);
            r.setFrame(rect(CONTROL_X, y, 180.0, 26.0));
            f.add(&r);
            recorders.push(r);
        }
        let shortcut_warning = label("", 12.0, false, mtm);
        shortcut_warning.setTextColor(Some(&NSColor::systemRedColor()));
        let y = f.row("", 18.0);
        shortcut_warning.setFrame(rect(CONTROL_X, y, W - CONTROL_X - 60.0, 18.0));
        f.add(&shortcut_warning);
        f.note(
            "Click a shortcut and press the new combination; Esc cancels. Pinboard shortcuts work inside the \
             shelf and must include \u{2318}.",
            CONTROL_X,
        );
        let restore = button("Restore Defaults", tag(RESTORE_SHORTCUTS, 0), target, mtm);
        let y = f.row("", 30.0);
        restore.setFrame(rect(CONTROL_X, y, restore.frame().size.width, 30.0));
        f.add(&restore);
        add_tab("Shortcuts", &f.view);
        let [open_shelf, paste_stack, next_board, previous_board]: [Retained<ShortcutRecorder>; 4] =
            recorders.try_into().unwrap_or_else(|_| unreachable!());

        // Privacy
        let mut f = Form::new(mtm);
        f.y += 10.0;
        let mut boxes = Vec::new();
        for (i, title) in [
            "Ignore content marked confidential by password managers",
            "Skip passwords, API keys, tokens and card numbers",
            "Show Pasted while sharing or recording the screen",
        ]
        .iter()
        .enumerate()
        {
            let b = checkbox(title, target, mtm);
            let y = f.row(
                if i == 0 {
                    "Ignore:"
                } else if i == 2 {
                    "Screen sharing:"
                } else {
                    ""
                },
                20.0,
            );
            b.setFrame(rect(CONTROL_X, y, W - CONTROL_X - 50.0, 20.0));
            f.add(&b);
            boxes.push(b);
        }
        let [ignore_concealed, ignore_secrets, visible_sharing]: [Retained<NSButton>; 3] =
            boxes.try_into().unwrap_or_else(|_| unreachable!());

        let apps = AppsDataSource::new(mtm);
        let apps_table = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 340.0, 100.0));
        let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns("app"));
        column.setWidth(330.0);
        apps_table.addTableColumn(&column);
        apps_table.setHeaderView(None);
        // SAFETY: the data source is retained by `SettingsWindow`.
        unsafe { apps_table.setDataSource(Some(ProtocolObject::from_ref(&*apps))) };
        let apps_scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 360.0, 104.0));
        apps_scroll.setDocumentView(Some(&apps_table));
        apps_scroll.setHasVerticalScroller(true);
        let y = f.row("Never save copies from:", 104.0);
        apps_scroll.setFrame(rect(CONTROL_X - 20.0, y, 330.0, 104.0));
        f.add(&apps_scroll);
        let add = button("+", tag(ADD_APP, 0), target, mtm);
        add.setFrame(rect(CONTROL_X + 316.0, y + 74.0, 34.0, 30.0));
        f.add(&add);
        let remove = button("\u{2212}", tag(REMOVE_APP, 0), target, mtm);
        remove.setFrame(rect(CONTROL_X + 316.0, y + 44.0, 34.0, 30.0));
        f.add(&remove);

        let patterns = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 330.0, 70.0));
        patterns.setRichText(false);
        patterns
            .setFont(Some(&NSFont::userFixedPitchFontOfSize(12.0).unwrap_or_else(|| NSFont::systemFontOfSize(12.0))));
        patterns.setAutomaticQuoteSubstitutionEnabled(false);
        patterns.setAutomaticDashSubstitutionEnabled(false);
        let pattern_scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 330.0, 70.0));
        pattern_scroll.setDocumentView(Some(&patterns));
        pattern_scroll.setHasVerticalScroller(true);
        pattern_scroll.setBorderType(objc2_app_kit::NSBorderType::BezelBorder);
        let y = f.row("Ignore text matching:", 70.0);
        pattern_scroll.setFrame(rect(CONTROL_X - 20.0, y, 330.0, 70.0));
        f.add(&pattern_scroll);
        let save = button("Save Patterns", tag(SAVE_PATTERNS, 0), target, mtm);
        let y = f.row("", 30.0);
        save.setFrame(rect(CONTROL_X - 20.0, y, save.frame().size.width, 30.0));
        f.add(&save);
        let patterns_status = label("One regular expression per line.", 11.0, false, mtm);
        patterns_status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        patterns_status.setFrame(rect(CONTROL_X - 10.0 + save.frame().size.width, y + 7.0, 260.0, 16.0));
        f.add(&patterns_status);
        add_tab("Privacy", &f.view);

        // Data
        let mut f = Form::new(mtm);
        let stats = label("", 13.0, false, mtm);
        let y = f.row("History:", 20.0);
        stats.setFrame(rect(CONTROL_X, y, W - CONTROL_X - 60.0, 18.0));
        f.add(&stats);
        let clear = button("Clear History\u{2026}", tag(crate::app::CLEAR_HISTORY_TAG, 0), target, mtm);
        let y = f.row("", 30.0);
        clear.setFrame(rect(CONTROL_X - 6.0, y, clear.frame().size.width, 30.0));
        f.add(&clear);
        let paste_status = wrapping_label("", &NSFont::systemFontOfSize(13.0), 3, mtm);
        let y = f.row("Paste:", 36.0);
        paste_status.setFrame(rect(CONTROL_X, y, W - CONTROL_X - 60.0, 36.0));
        f.add(&paste_status);
        let import = button("Import from Paste\u{2026}", tag(crate::app::IMPORT_PASTE_TAG, 0), target, mtm);
        let y = f.row("", 30.0);
        import.setFrame(rect(CONTROL_X - 6.0, y, import.frame().size.width, 30.0));
        f.add(&import);
        let mut import_filters = Vec::new();
        for (i, title) in [
            "Items from apps on the Privacy ignore list",
            "API keys, tokens, private keys and card numbers",
            "Text matching your ignore patterns",
            "Short snippets that mention a password",
        ]
        .iter()
        .enumerate()
        {
            let b = checkbox(title, target, mtm);
            let y = f.row(if i == 0 { "When importing, skip:" } else { "" }, 20.0);
            b.setFrame(rect(CONTROL_X, y, W - CONTROL_X - 50.0, 20.0));
            f.add(&b);
            import_filters.push(b);
        }
        let [skip_excluded_apps, skip_secrets, skip_ignore_patterns, skip_password_mentions]: [Retained<NSButton>; 4] =
            import_filters.try_into().unwrap_or_else(|_| unreachable!());
        f.note(
            "Importing reads a copy of Paste's database, so Paste is never modified. Running it again only adds \
             new items. Skipped items are not imported; nothing already imported is removed.",
            CONTROL_X,
        );
        let folder = button("Open Data Folder", tag(OPEN_DATA_FOLDER, 0), target, mtm);
        let y = f.row("Files:", 30.0);
        folder.setFrame(rect(CONTROL_X - 6.0, y, folder.frame().size.width, 30.0));
        f.add(&folder);
        let file = button("Edit Settings File", tag(crate::app::OPEN_SETTINGS_TAG, 0), target, mtm);
        file.setFrame(rect(CONTROL_X + folder.frame().size.width, y, file.frame().size.width, 30.0));
        f.add(&file);
        add_tab("Data", &f.view);

        if let Some(content) = window.contentView() {
            content.addSubview(&tabs);
        }
        window.center();

        SettingsWindow {
            mtm,
            window,
            tabs,
            retention,
            max_size,
            paste_directly,
            plain_default,
            sounds,
            launch_at_login,
            link_previews,
            trim_whitespace,
            clean_links,
            accessibility,
            login_note,
            open_shelf,
            paste_stack,
            next_board,
            previous_board,
            shortcut_warning,
            ignore_concealed,
            ignore_secrets,
            visible_sharing,
            apps_table,
            apps,
            patterns,
            patterns_status,
            stats,
            paste_status,
            skip_excluded_apps,
            skip_secrets,
            skip_ignore_patterns,
            skip_password_mentions,
        }
    }

    pub fn show(&self) {
        super::activate_app(self.mtm);
        self.window.makeKeyAndOrderFront(None);
        self.window.orderFrontRegardless();
    }

    /// Selects a tab by its (case-insensitive) label.
    pub fn select_tab(&self, name: &str) {
        let index =
            ["general", "pasting", "shortcuts", "privacy", "data"].iter().position(|t| t.eq_ignore_ascii_case(name));
        if let Some(i) = index {
            self.tabs.selectTabViewItemAtIndex(i as isize);
        }
    }

    /// Puts every value from `s` into the controls.
    pub fn load(&self, s: &Settings) {
        let g = &s.general;
        let ri = RETENTIONS.iter().position(|(_, r)| *r == g.history_retention).unwrap_or(2);
        self.retention.selectItemAtIndex(ri as isize);
        let si = SIZES_MB.iter().position(|m| *m >= g.max_item_size_mb).unwrap_or(SIZES_MB.len() - 1);
        self.max_size.selectItemAtIndex(si as isize);
        set_checked(&self.paste_directly, g.paste_directly);
        set_checked(&self.plain_default, g.paste_plain_text_by_default);
        set_checked(&self.sounds, g.sounds);
        set_checked(&self.launch_at_login, g.launch_at_login);
        set_checked(&self.link_previews, g.link_previews);
        set_checked(&self.trim_whitespace, s.pasting.trim_whitespace);
        set_checked(&self.clean_links, s.pasting.clean_links);
        self.open_shelf.set_shortcut(s.shortcuts.open_shelf);
        self.paste_stack.set_shortcut(s.shortcuts.paste_stack);
        self.next_board.set_shortcut(s.shortcuts.next_pinboard);
        self.previous_board.set_shortcut(s.shortcuts.previous_pinboard);
        let p = &s.privacy;
        set_checked(&self.ignore_concealed, p.ignore_concealed);
        set_checked(&self.ignore_secrets, p.ignore_secrets);
        set_checked(&self.visible_sharing, p.visible_during_screen_sharing);
        let im = &s.import;
        set_checked(&self.skip_excluded_apps, im.skip_excluded_apps);
        set_checked(&self.skip_secrets, im.skip_secrets);
        set_checked(&self.skip_ignore_patterns, im.skip_ignore_patterns);
        set_checked(&self.skip_password_mentions, im.skip_password_mentions);
        *self.apps.ivars().borrow_mut() = p.excluded_apps.iter().map(|id| (app_name(id), id.clone())).collect();
        self.apps_table.reloadData();
        self.patterns.setString(&ns(&p.ignore_patterns.join("\n")));
    }

    /// Copies control values into `s` (everything except ignore patterns,
    /// which are saved explicitly so half-typed expressions are not applied).
    pub fn read(&self, s: &mut Settings) {
        let g = &mut s.general;
        if let Some((_, r)) = RETENTIONS.get(self.retention.indexOfSelectedItem().max(0) as usize) {
            g.history_retention = *r;
        }
        if let Some(m) = SIZES_MB.get(self.max_size.indexOfSelectedItem().max(0) as usize) {
            g.max_item_size_mb = *m;
        }
        g.paste_directly = checked(&self.paste_directly);
        g.paste_plain_text_by_default = checked(&self.plain_default);
        g.sounds = checked(&self.sounds);
        g.launch_at_login = checked(&self.launch_at_login);
        g.link_previews = checked(&self.link_previews);
        s.pasting.trim_whitespace = checked(&self.trim_whitespace);
        s.pasting.clean_links = checked(&self.clean_links);
        let sc = &mut s.shortcuts;
        for (rec, target) in [
            (&self.open_shelf, &mut sc.open_shelf),
            (&self.paste_stack, &mut sc.paste_stack),
            (&self.next_board, &mut sc.next_pinboard),
            (&self.previous_board, &mut sc.previous_pinboard),
        ] {
            if let Some(shortcut) = rec.shortcut() {
                *target = shortcut;
            }
        }
        let p = &mut s.privacy;
        p.ignore_concealed = checked(&self.ignore_concealed);
        p.ignore_secrets = checked(&self.ignore_secrets);
        p.visible_during_screen_sharing = checked(&self.visible_sharing);
        let im = &mut s.import;
        im.skip_excluded_apps = checked(&self.skip_excluded_apps);
        im.skip_secrets = checked(&self.skip_secrets);
        im.skip_ignore_patterns = checked(&self.skip_ignore_patterns);
        im.skip_password_mentions = checked(&self.skip_password_mentions);
    }

    /// Validated patterns from the text view, or the first error.
    pub fn read_patterns(&self) -> Result<Vec<String>, String> {
        let text = self.patterns.string().to_string();
        let mut out = Vec::new();
        for (i, line) in text.lines().map(str::trim).enumerate() {
            if line.is_empty() {
                continue;
            }
            regex::Regex::new(line).map_err(|e| {
                let first = e.to_string().lines().last().unwrap_or("invalid").trim().to_string();
                format!("Line {}: {first}", i + 1)
            })?;
            out.push(line.to_string());
        }
        Ok(out)
    }

    pub fn set_patterns_status(&self, text: &str, error: bool) {
        self.patterns_status.setStringValue(&ns(text));
        let color = if error { NSColor::systemRedColor() } else { NSColor::secondaryLabelColor() };
        self.patterns_status.setTextColor(Some(&color));
    }

    pub fn selected_app_row(&self) -> Option<usize> {
        let row = self.apps_table.selectedRow();
        (row >= 0).then_some(row as usize)
    }

    pub fn set_shortcut_warning(&self, failed: &[Shortcut]) {
        let text = if failed.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = failed.iter().map(|s| s.display()).collect();
            format!("{} is used by another app. Choose a different shortcut.", list.join(", "))
        };
        self.shortcut_warning.setStringValue(&ns(&text));
    }

    pub fn set_login_note(&self, text: Option<&str>) {
        self.login_note.setStringValue(&ns(text.unwrap_or("")));
    }

    pub fn set_accessibility(&self, trusted: bool) {
        let (text, color) = if trusted {
            ("\u{2713} Granted", NSColor::systemGreenColor())
        } else {
            ("Not granted", NSColor::systemOrangeColor())
        };
        self.accessibility.setStringValue(&ns(text));
        self.accessibility.setTextColor(Some(&color));
    }

    pub fn set_info(&self, stats: &str, paste: &str) {
        self.stats.setStringValue(&ns(stats));
        self.paste_status.setStringValue(&ns(paste));
    }
}
