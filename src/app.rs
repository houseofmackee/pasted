//! The menu bar app: owns all state on the main thread and wires AppKit
//! callbacks to actions.
//!
//! State lives in a thread local and is reached through `with`. AppKit can
//! re-enter our callbacks while a modal loop runs, so state is never borrowed
//! across a modal dialog or menu; see `run_modal`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSButton, NSControl,
    NSControlStateValueOn, NSControlTextEditingDelegate, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType,
    NSImage, NSMenu, NSMenuItem, NSModalResponseOK, NSOpenPanel, NSPasteboard, NSPasteboardItem, NSResponder,
    NSRunningApplication, NSSavePanel, NSSearchFieldDelegate, NSStatusBar, NSStatusItem, NSTextFieldDelegate,
    NSTextView, NSWindowDelegate, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSBundle, NSData, NSDistributedNotificationCenter, NSNotification, NSNumber, NSObject,
    NSObjectProtocol, NSString, NSTimer, NSURL,
};

use crate::clipboard::capture::{Capture, capture};
use crate::clipboard::pasteboard;
use crate::clipboard::privacy::PrivacyFilter;
use crate::hotkeys::{HotkeyAction, Hotkeys};
use crate::import::paste_app::{self, ImportOptions};
use crate::model::{IMAGE_UTIS, ItemKind, now_ms};
use crate::settings::Settings;
use crate::sounds::Sound;
use crate::store::Store;
use crate::ui::preview::Preview;
use crate::ui::settings_window::{self, SettingsWindow};
use crate::ui::shelf::{self, Shelf, menu_item, tag, untag};
use crate::ui::stack_panel::StackPanel;
use crate::ui::{dialogs, ns};

// Menu action codes (see `shelf::tag`).
const OPEN_SHELF: u8 = 10;
const TOGGLE_PAUSE: u8 = 11;
const IMPORT_PASTE: u8 = 12;
const OPEN_SETTINGS: u8 = 13;
const WELCOME: u8 = 15;
const ABOUT: u8 = 16;
const QUIT: u8 = 17;
const CLEAR_HISTORY: u8 = 18;
const PASTE_STACK: u8 = 19;
const SETTINGS: u8 = 20;
const STATISTICS: u8 = 21;
const CHECK_UPDATES: u8 = 22;
pub const IMPORT_PASTE_TAG: u8 = IMPORT_PASTE;
pub const CLEAR_HISTORY_TAG: u8 = CLEAR_HISTORY;
pub const OPEN_SETTINGS_TAG: u8 = OPEN_SETTINGS;
const ITEM_PASTE: u8 = 30;
const ITEM_PASTE_PLAIN: u8 = 31;
const ITEM_COPY: u8 = 32;
const ITEM_PREVIEW: u8 = 33;
const ITEM_EDIT: u8 = 34;
const ITEM_RENAME: u8 = 35;
const ITEM_PIN: u8 = 36;
const ITEM_PIN_NEW: u8 = 37;
const ITEM_UNPIN: u8 = 38;
const ITEM_DELETE: u8 = 39;
const ITEM_OPEN: u8 = 40;
const ITEM_PASTE_AS: u8 = 41;
/// Show the item (arg) in clipboard history.
const ITEM_SHOW_HISTORY: u8 = 42;
/// Show the selected item in the pinboard (arg).
const ITEM_SHOW_BOARD: u8 = 43;
/// Transform index meaning "paste the snippet without expanding placeholders".
const RAW_SNIPPET: i64 = 0xff;

const POLL_INTERVAL: f64 = 0.3;
/// Posted by `pasted open` to show the shelf of the running app.
pub const OPEN_NOTIFICATION: &str = "io.github.houseofmackee.pasted.open";
/// Posted by `pasted settings` to show the Settings window.
pub const SETTINGS_NOTIFICATION: &str = "io.github.houseofmackee.pasted.settings";
/// Posted by `pasted stats --show` to show the Statistics window.
pub const STATS_NOTIFICATION: &str = "io.github.houseofmackee.pasted.stats";
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(3600);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PastedTarget"]
    pub struct Target;

    impl Target {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            tick();
        }

        #[unsafe(method(menuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            dispatch(sender.tag());
        }

        #[unsafe(method(tabClicked:))]
        fn tab_clicked(&self, sender: &NSButton) {
            dispatch(sender.tag());
        }

        #[unsafe(method(tableAction:))]
        fn table_action(&self, sender: &NSControl) {
            dispatch(sender.tag());
        }

        #[unsafe(method(buttonAction:))]
        fn button_action(&self, sender: &NSButton) {
            dispatch(sender.tag());
        }

        #[unsafe(method(settingsChanged:))]
        fn settings_changed(&self, _sender: Option<&AnyObject>) {
            settings_ui_changed();
        }

        #[unsafe(method(remoteOpen:))]
        fn remote_open(&self, _notification: &NSNotification) {
            with(|app| {
                if !app.shelf.is_visible() {
                    app.open_shelf();
                }
            });
        }

        #[unsafe(method(remoteStats:))]
        fn remote_stats(&self, _notification: &NSNotification) {
            with(|app| app.open_stats());
        }

        #[unsafe(method(statsRange:))]
        fn stats_range(&self, _sender: Option<&AnyObject>) {
            with(|app| app.refresh_stats());
        }

        #[unsafe(method(remoteSettings:))]
        fn remote_settings(&self, notification: &NSNotification) {
            let tab = notification.object().and_then(|o| o.downcast::<NSString>().ok()).map(|s| s.to_string());
            with(|app| {
                app.open_settings();
                if let (Some(tab), Some(win)) = (tab, &app.settings_window) {
                    win.select_tab(&tab);
                }
            });
        }

        #[unsafe(method(shelfScrolled:))]
        fn shelf_scrolled(&self, _notification: &NSNotification) {
            with(|app| app.shelf.ensure_visible_built(&app.store));
        }

        #[unsafe(method(stackAdvance:))]
        fn stack_advance(&self, _obj: Option<&AnyObject>) {
            with(|app| app.advance_stack());
        }

        #[unsafe(method(snippetInputs:))]
        fn snippet_inputs(&self, _obj: Option<&AnyObject>) {
            snippet_inputs();
        }

        #[unsafe(method(moveCursor:))]
        fn move_cursor(&self, n: Option<&NSNumber>) {
            if let Some(n) = n {
                crate::paste::post_left_arrows(n.as_usize());
            }
        }

        #[unsafe(method(statusItemClicked:))]
        fn status_item_clicked(&self, _sender: Option<&AnyObject>) {
            status_item_clicked();
        }

        #[unsafe(method(showStatusMenu:))]
        fn show_status_menu(&self, _obj: Option<&AnyObject>) {
            show_status_menu();
        }

        #[unsafe(method(postPaste:))]
        fn post_paste(&self, _obj: Option<&AnyObject>) {
            with(|app| {
                if crate::paste::post_cmd_v() {
                    // The Cmd+V listener sees this keystroke too; it is ours.
                    app.own_paste_at = Some(Instant::now());
                    app.play(Sound::Paste);
                }
            });
        }
    }

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSControlTextEditingDelegate for Target {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            search_changed();
        }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, _control: &NSControl, _text_view: &NSTextView, command: Sel) -> bool {
            shelf_command(command)
        }
    }

    unsafe impl NSTextFieldDelegate for Target {}
    unsafe impl NSSearchFieldDelegate for Target {}

    unsafe impl NSWindowDelegate for Target {
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            shelf_resigned();
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            with(|app| app.refresh_settings_window());
        }
    }
);

impl Target {
    /// The app's action target (public so UI tests can build windows).
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// Mirrors the `link_previews` setting for the background fetcher.
static LINK_PREVIEWS_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

type ImportResult = Arc<Mutex<Option<Result<String, String>>>>;

/// A snippet waiting for its `{input:...}` values.
struct PendingSnippet {
    id: i64,
    plain: bool,
    then_paste: bool,
    labels: Vec<String>,
}

/// Paste Stack: copies are queued and each Cmd+V pastes the next one.
/// How long after Pasted sends Cmd+V itself the listener ignores Cmd+V.
const OWN_PASTE_WINDOW: Duration = Duration::from_millis(400);

struct PasteStack {
    queue: crate::stack::StackQueue,
    /// Global key monitor that notices Cmd+V in other apps.
    monitor: Option<Retained<AnyObject>>,
    panel: StackPanel,
}

pub struct App {
    mtm: MainThreadMarker,
    target: Retained<Target>,
    settings: Settings,
    filter: PrivacyFilter,
    store: Store,
    pasteboard: Retained<NSPasteboard>,
    last_change: isize,
    /// Change count produced by our own write, so it is not captured again.
    own_change: Option<isize>,
    paused: bool,
    device: Option<String>,
    status_item: Retained<NSStatusItem>,
    /// Shown on right-click; a left click opens the shelf.
    status_menu: Retained<NSMenu>,
    /// Notices Cmd+V in other apps for the paste click (only while sounds
    /// are on and Accessibility is allowed).
    paste_listener: Option<Retained<AnyObject>>,
    /// When Pasted last sent Cmd+V itself.
    own_paste_at: Option<Instant>,
    hotkeys: Hotkeys,
    shelf: Shelf,
    preview: Preview,
    /// App that was frontmost when the shelf opened; receives the paste.
    previous_app: Option<Retained<NSRunningApplication>>,
    modal: bool,
    asked_accessibility: bool,
    last_maintenance: Instant,
    import_job: Option<ImportResult>,
    /// Set while encryption is being switched on or off; the real store is
    /// closed and capture pauses.
    switching: Option<ImportResult>,
    /// A running export or restore: dialog title and result slot.
    file_job: Option<(&'static str, ImportResult)>,
    stack: Option<PasteStack>,
    pending_snippet: Option<PendingSnippet>,
    settings_window: Option<SettingsWindow>,
    stats_window: Option<crate::ui::stats_window::StatsWindow>,
    welcome: Option<crate::ui::welcome::Welcome>,
    /// Modification time of settings.toml when last read or written, to
    /// notice hand edits.
    settings_mtime: Option<std::time::SystemTime>,
    last_settings_check: Instant,
    rules: crate::rules::Rules,
    /// Problems with rules from settings, shown in the Rules tab.
    rule_errors: Vec<String>,
    last_expiry_check: Instant,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app state. Returns `None` if state is unavailable or
/// already borrowed (a re-entrant callback), which is logged and skipped.
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().map(f),
        Err(_) => {
            tracing::debug!("re-entrant callback skipped");
            None
        }
    })
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("UI code runs on the main thread")
}

/// Runs a modal dialog without holding the state borrow, keeping the shelf open.
fn run_modal<T>(f: impl FnOnce(MainThreadMarker) -> T) -> T {
    let shelf_visible = with(|app| {
        app.modal = true;
        app.preview.hide();
        app.shelf.is_visible()
    })
    .unwrap_or(false);
    let result = f(mtm());
    with(|app| {
        app.modal = false;
        if shelf_visible {
            app.shelf.panel.makeKeyWindow();
            let responder: &NSResponder = &app.shelf.search;
            app.shelf.panel.makeFirstResponder(Some(responder));
        }
    });
    result
}

fn shift_held() -> bool {
    NSApplication::sharedApplication(mtm())
        .currentEvent()
        .map(|e| e.modifierFlags().contains(NSEventModifierFlags::Shift))
        .unwrap_or(false)
}

fn device_name() -> Option<String> {
    let out = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output().ok()?;
    let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

pub fn run() -> Result<()> {
    let mtm = MainThreadMarker::new().context("the app must start on the main thread")?;

    // Single instance: a second copy would fight over hotkeys and the pasteboard.
    let lock_path = crate::paths::data_dir()?.join("pasted.lock");
    let lock = std::fs::File::create(&lock_path)?;
    if lock.try_lock().is_err() {
        anyhow::bail!("Pasted is already running");
    }

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    // Developer aid: force light or dark to check both without changing the system.
    if let Ok(mode) = std::env::var("PASTED_APPEARANCE") {
        // SAFETY: reading AppKit appearance name constants.
        let name = unsafe {
            if mode == "dark" { objc2_app_kit::NSAppearanceNameDarkAqua } else { objc2_app_kit::NSAppearanceNameAqua }
        };
        if let Some(appearance) = objc2_app_kit::NSAppearance::appearanceNamed(name) {
            app.setAppearance(Some(&appearance));
        }
    }
    install_main_menu(mtm);

    let settings_path = crate::paths::settings_path()?;
    let (settings, settings_error) = match Settings::load(&settings_path) {
        Ok(s) => (s, None),
        Err(e) => (Settings::default(), Some(format!("{e:#}"))),
    };
    let store = Store::open_default()?;
    let target = Target::new(mtm);
    let shelf = Shelf::new(&target, mtm);
    shelf.panel.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    shelf.set_visible_in_screen_sharing(settings.privacy.visible_during_screen_sharing);

    let status_item = NSStatusBar::systemStatusBar().statusItemWithLength(-1.0);
    if let Some(button) = status_item.button(mtm) {
        let image =
            NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns("doc.on.clipboard"), Some(&ns("Pasted")));
        if let Some(image) = image {
            image.setTemplate(true);
            button.setImage(Some(&image));
        } else {
            button.setTitle(&ns("Pasted"));
        }
        // Clicks come to us instead of opening a menu, so left and right
        // clicks can do different things.
        connect_status_button(&button, &target);
        add_show_menu_action(&button);
        objc2_app_kit::NSAccessibility::setAccessibilityHelp(
            &*button,
            Some(&ns("Opens Pasted. Right-click or Control-click for more options.")),
        );
    }

    let pb = pasteboard::general();
    let state = App {
        mtm,
        filter: PrivacyFilter::new(&settings.privacy),
        last_change: pb.changeCount(),
        pasteboard: pb,
        own_change: None,
        paused: false,
        device: device_name(),
        hotkeys: Hotkeys::install(),
        preview: Preview::new(mtm),
        previous_app: None,
        modal: false,
        asked_accessibility: false,
        last_maintenance: Instant::now() - MAINTENANCE_INTERVAL,
        import_job: None,
        file_job: None,
        switching: None,
        stack: None,
        pending_snippet: None,
        settings_window: None,
        stats_window: None,
        welcome: None,
        settings_mtime: settings_mtime(),
        last_settings_check: Instant::now(),
        rules: crate::rules::Rules::new(&[]).0,
        rule_errors: Vec::new(),
        last_expiry_check: Instant::now() - Duration::from_secs(60),
        store,
        settings,
        status_item,
        status_menu: NSMenu::new(mtm),
        paste_listener: None,
        own_paste_at: None,
        shelf,
        target,
    };
    APP.with(|cell| *cell.borrow_mut() = Some(state));

    with(|app| {
        app.apply_settings();
        app.rebuild_status_menu();
        // SAFETY: the target implements `tick:` and lives for the whole app.
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                POLL_INTERVAL,
                &app.target,
                sel!(tick:),
                None,
                true,
            )
        };
        // SAFETY: the target implements `remoteOpen:` and is never deallocated.
        unsafe {
            NSDistributedNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &app.target,
                sel!(remoteOpen:),
                Some(&ns(OPEN_NOTIFICATION)),
                None,
            )
        };
        // SAFETY: as above.
        unsafe {
            NSDistributedNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &app.target,
                sel!(remoteSettings:),
                Some(&ns(SETTINGS_NOTIFICATION)),
                None,
            )
        };
        // SAFETY: as above.
        unsafe {
            NSDistributedNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &app.target,
                sel!(remoteStats:),
                Some(&ns(STATS_NOTIFICATION)),
                None,
            )
        };
    });

    if let Some(e) = settings_error {
        dialogs::info("Settings file has an error", &format!("Using defaults until it is fixed.\n\n{e}"), mtm);
    }
    crate::ocr::spawn_worker();
    crate::link_preview::spawn_worker(&LINK_PREVIEWS_ENABLED);
    crate::updates::start(mtm);
    show_welcome_once();
    tracing::info!("Pasted is running");
    app.run();
    drop(lock);
    Ok(())
}

fn install_main_menu(mtm: MainThreadMarker) {
    // Accessory apps still need an Edit menu so Cmd+C/V/X/A/Z work in text fields.
    let main = NSMenu::new(mtm);
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Edit"));
    for (title, action, key) in [
        ("Undo", sel!(undo:), "z"),
        ("Redo", sel!(redo:), "Z"),
        ("Cut", sel!(cut:), "x"),
        ("Copy", sel!(copy:), "c"),
        ("Paste", sel!(paste:), "v"),
        ("Select All", sel!(selectAll:), "a"),
    ] {
        // SAFETY: standard responder chain selectors.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), Some(action), &ns(key))
        };
        edit.addItem(&item);
    }
    let app_item = NSMenuItem::new(mtm);
    main.addItem(&app_item);
    let file = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("File"));
    // SAFETY: standard responder chain selector.
    let close = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &ns("Close"),
            Some(sel!(performClose:)),
            &ns("w"),
        )
    };
    file.addItem(&close);
    let file_item = NSMenuItem::new(mtm);
    file_item.setSubmenu(Some(&file));
    main.addItem(&file_item);
    let edit_item = NSMenuItem::new(mtm);
    edit_item.setSubmenu(Some(&edit));
    main.addItem(&edit_item);
    NSApplication::sharedApplication(mtm).setMainMenu(Some(&main));
}

impl App {
    fn apply_rules(&mut self, id: i64, item: &crate::model::NewItem) {
        if self.rules.is_empty() {
            return;
        }
        let app = item.source_app.as_ref();
        let outcome = self.rules.evaluate(
            item.kind,
            &item.text,
            app.map(|a| a.bundle_id.as_str()),
            app.and_then(|a| a.name.as_deref()),
        );
        if outcome.matched.is_empty() {
            return;
        }
        tracing::debug!("rules matched item {id}: {:?}", outcome.matched);
        match self.store.apply_rules(id, &outcome, now_ms()) {
            Ok(pinned) if !pinned.is_empty() && self.shelf.is_visible() => self.shelf.reload_tabs(&self.store),
            Ok(_) => {}
            Err(e) => tracing::error!("applying rules failed: {e:#}"),
        }
    }

    /// Removes items whose expiry has passed. Checked every 5 s so sensitive
    /// items do not outlive their deadline by much (the query is indexed).
    fn expire_items(&mut self) {
        if self.last_expiry_check.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.last_expiry_check = Instant::now();
        match self.store.expire_due(now_ms()) {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!("{n} items expired");
                if self.shelf.is_visible() {
                    let keep = self.shelf.selected;
                    let text = self.shelf.search_text();
                    self.shelf.set_query(&text, &self.store);
                    self.shelf.select(keep);
                }
            }
            Err(e) => tracing::error!("expiring items failed: {e:#}"),
        }
    }

    fn apply_settings(&mut self) {
        let (rules, errors) = crate::rules::Rules::new(&self.settings.rules);
        for e in &errors {
            tracing::warn!("rule skipped: {e}");
        }
        self.rules = rules;
        self.rule_errors = errors;
        LINK_PREVIEWS_ENABLED.store(self.settings.general.link_previews, std::sync::atomic::Ordering::Relaxed);
        self.filter = PrivacyFilter::new(&self.settings.privacy);
        self.shelf.set_visible_in_screen_sharing(self.settings.privacy.visible_during_screen_sharing);
        self.shelf.fuzzy = self.settings.general.fuzzy_search;
        self.shelf.bar_alignment = self.settings.general.shelf_bar_alignment;
        self.shelf.layout_bar();
        let s = &self.settings.shortcuts;
        let failed =
            self.hotkeys.set(&[(HotkeyAction::OpenShelf, s.open_shelf), (HotkeyAction::PasteStack, s.paste_stack)]);
        for f in &failed {
            tracing::warn!("shortcut {f} is unavailable");
        }
        let login_problem = crate::login_item::sync(self.settings.general.launch_at_login);
        if let Some(problem) = &login_problem {
            tracing::warn!("{problem}");
        }
        if let Some(win) = &self.settings_window {
            win.set_shortcut_warning(&failed);
            win.set_login_note(login_problem.as_deref());
        }
    }

    /// Writes settings to disk and applies them.
    fn save_settings(&mut self) {
        match crate::paths::settings_path().and_then(|p| self.settings.save(&p)) {
            Ok(()) => self.settings_mtime = settings_mtime(),
            Err(e) => tracing::error!("saving settings failed: {e:#}"),
        }
        self.apply_settings();
        self.rebuild_status_menu();
    }

    /// Picks up hand edits to settings.toml.
    fn check_settings_file(&mut self) {
        if self.last_settings_check.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_settings_check = Instant::now();
        let mtime = settings_mtime();
        if mtime == self.settings_mtime {
            return;
        }
        self.settings_mtime = mtime;
        match crate::paths::settings_path().and_then(|p| Settings::load(&p)) {
            Ok(s) if s != self.settings => {
                tracing::info!("settings file changed, reloading");
                self.settings = s;
                self.apply_settings();
                self.rebuild_status_menu();
                if let Some(win) = &self.settings_window {
                    win.load(&self.settings);
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("settings file has an error, keeping current settings: {e:#}"),
        }
    }

    fn open_welcome(&mut self) {
        self.close_shelf();
        if self.welcome.is_none() {
            let t: &AnyObject = &self.target;
            let shortcut = self.settings.shortcuts.open_shelf.display();
            let paste_found = !paste_app::detect().is_empty();
            self.welcome = Some(crate::ui::welcome::Welcome::new(t, &shortcut, paste_found, self.mtm));
        }
        if let Some(win) = &self.welcome {
            win.refresh();
            win.show(self.mtm);
        }
    }

    /// Keeps the welcome window's permission status current while it is open.
    fn refresh_welcome(&mut self) {
        if let Some(win) = &self.welcome
            && win.is_visible()
            && win.refresh()
        {
            self.rebuild_status_menu();
        }
    }

    fn open_stats(&mut self) {
        self.close_shelf();
        if self.stats_window.is_none() {
            let t: &AnyObject = &self.target;
            self.stats_window = Some(crate::ui::stats_window::StatsWindow::new(t, self.mtm));
        }
        self.refresh_stats();
        if let Some(win) = &self.stats_window {
            win.show(self.mtm);
        }
    }

    /// Copies the statistics tables: tab-separated for spreadsheets, HTML
    /// for documents.
    fn copy_stats_data(&mut self) {
        use crate::model::{Representation, UTI_HTML, UTI_PLAIN_TEXT};
        let Some(win) = &mut self.stats_window else { return };
        let (tsv, html) = win.data();
        let reps = [
            Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: tsv.as_bytes().to_vec() },
            Representation { index: 0, uti: UTI_HTML.into(), data: html.as_bytes().to_vec() },
        ];
        let cc = pasteboard::write(&self.pasteboard, &reps);
        // Not captured into history: it is generated by Pasted, not copied from an app.
        self.own_change = Some(cc);
        self.last_change = cc;
        win.mark_copied();
    }

    fn refresh_stats(&mut self) {
        let Some(win) = &mut self.stats_window else { return };
        if let Ok(names) = self.store.device_names() {
            win.set_device_names(&names);
        }
        match self.store.report(win.days(), now_ms(), win.device().as_deref()) {
            Ok(report) => win.update(&report),
            Err(e) => tracing::error!("statistics failed: {e:#}"),
        }
    }

    fn open_settings(&mut self) {
        self.close_shelf();
        if self.settings_window.is_none() {
            let t: &AnyObject = &self.target;
            self.settings_window = Some(SettingsWindow::new(t, &self.target, self.mtm));
        }
        if let Some(win) = &self.settings_window {
            win.load(&self.settings);
        }
        self.apply_settings();
        self.refresh_settings_window();
        if let Some(win) = &self.settings_window {
            win.show();
        }
    }

    /// Updates the parts of the Settings window that reflect outside state.
    fn refresh_settings_window(&self) {
        let Some(win) = &self.settings_window else { return };
        win.set_encryption(if self.switching.is_some() { None } else { Some(self.store.is_encrypted()) });
        win.set_accessibility(crate::paste::is_trusted());
        let stats = self
            .store
            .stats()
            .map(|s| {
                format!(
                    "{} items, {:.1} MB, {} pinboard{}",
                    s.items,
                    s.bytes as f64 / 1_048_576.0,
                    s.pinboards,
                    if s.pinboards == 1 { "" } else { "s" }
                )
            })
            .unwrap_or_default();
        let paste = match paste_app::detect().into_iter().next() {
            None => "No Paste installation found on this Mac.".to_string(),
            Some(install) => {
                let last = self.store.last_import().ok().flatten().map(|(when, n)| {
                    let date = chrono::DateTime::from_timestamp_millis(when)
                        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default();
                    format!(" Last import {date}: {n} new items.")
                });
                format!("Found {}.{}", install.bundle_id, last.unwrap_or_else(|| " Not imported yet.".into()))
            }
        };
        win.set_info(&stats, &paste);
    }

    fn rebuild_status_menu(&mut self) {
        let mtm = self.mtm;
        let menu = NSMenu::new(mtm);
        let t: &AnyObject = &self.target;
        let add = |title: &str, code: u8| {
            let item = menu_item(title, tag(code, 0), t, mtm);
            menu.addItem(&item);
            item
        };
        add(&format!("Open Pasted    {}", self.settings.shortcuts.open_shelf.display()), OPEN_SHELF);
        let stack = add(&format!("Paste Stack    {}", self.settings.shortcuts.paste_stack.display()), PASTE_STACK);
        if self.stack.is_some() {
            stack.setState(NSControlStateValueOn);
        }
        let pause = add(if self.paused { "Resume Capturing" } else { "Pause Capturing" }, TOGGLE_PAUSE);
        if self.paused {
            pause.setState(NSControlStateValueOn);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("Import from Paste\u{2026}", IMPORT_PASTE);
        let clear = NSMenu::new(mtm);
        for (title, minutes) in CLEAR_CHOICES {
            clear.addItem(&menu_item(&format!("{title}\u{2026}"), tag(CLEAR_HISTORY, *minutes), t, mtm));
            if *minutes == 24 * 60 {
                clear.addItem(&NSMenuItem::separatorItem(mtm));
            }
        }
        add("Clear History", 0).setSubmenu(Some(&clear));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("Statistics\u{2026}", STATISTICS);
        add("Settings\u{2026}", SETTINGS);
        add("Welcome\u{2026}", WELCOME);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        if crate::updates::available() {
            match crate::updates::found() {
                Some(version) => add(&format!("Update to Pasted {version}\u{2026}"), CHECK_UPDATES),
                None => add("Check for Updates\u{2026}", CHECK_UPDATES),
            };
        }
        add("About Pasted", ABOUT);
        add("Quit Pasted", QUIT);
        self.status_menu = menu;
    }

    fn poll_clipboard(&mut self) {
        if self.switching.is_some() {
            // Leave `last_change` alone so the latest copy is captured afterwards.
            return;
        }
        let cc = self.pasteboard.changeCount();
        if cc == self.last_change {
            return;
        }
        self.last_change = cc;
        if self.own_change == Some(cc) || self.paused {
            return;
        }
        let max = (self.settings.general.max_item_size_mb as usize).saturating_mul(1024 * 1024);
        match capture(&self.pasteboard, &self.filter, max, self.device.as_deref()) {
            Capture::Item(item) => match self.store.insert(&item, true) {
                Ok(inserted) => {
                    tracing::debug!("captured {:?} item {inserted:?}", item.kind);
                    self.apply_rules(inserted.id(), &item);
                    self.push_stack(inserted.id());
                    self.play(Sound::Copy);
                    if self.shelf.is_visible() && self.shelf.board.is_none() {
                        self.shelf.reload(&self.store);
                    }
                }
                Err(e) => tracing::error!("saving clip failed: {e:#}"),
            },
            Capture::Sensitive(item, reason, ms) => match self.store.insert(&item, true) {
                Ok(inserted) => {
                    // Rules never pin sensitive items; they only live until the deadline.
                    let id = inserted.id();
                    if let Err(e) = self.store.mark_sensitive(id, now_ms() + ms) {
                        tracing::error!("marking item sensitive failed: {e:#}");
                    }
                    tracing::info!("kept sensitive item {id} ({reason:?}) for {}", crate::rules::format_duration(ms));
                    self.push_stack(id);
                    self.play(Sound::Copy);
                    if self.shelf.is_visible() && self.shelf.board.is_none() {
                        self.shelf.reload(&self.store);
                    }
                }
                Err(e) => tracing::error!("saving sensitive clip failed: {e:#}"),
            },
            Capture::Ignored(reason) => tracing::info!("not saved: {reason:?}"),
            Capture::TooLarge(n) => tracing::info!("not saved: {n} bytes exceeds the size limit"),
            Capture::Empty => {}
        }
    }

    fn toggle_stack(&mut self) {
        if let Some(stack) = self.stack.take() {
            stack.panel.hide();
            if let Some(m) = stack.monitor {
                // SAFETY: `m` was returned by addGlobalMonitorForEventsMatchingMask.
                unsafe { NSEvent::removeMonitor(&m) };
            }
            self.rebuild_status_menu();
            return;
        }
        if !crate::paste::is_trusted() {
            // Watching for Cmd+V in other apps needs Accessibility permission.
            crate::paste::request_trust();
        }
        let target = self.target.clone();
        let handler = RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| {
            // SAFETY: AppKit passes a valid event for the duration of the call.
            let event = unsafe { event.as_ref() };
            let flags = event.modifierFlags();
            if event.keyCode() == 9
                && flags.contains(NSEventModifierFlags::Command)
                && !flags.contains(NSEventModifierFlags::Shift)
            {
                // Let the target app read the pasteboard before it changes.
                unsafe {
                    let _: () = msg_send![&*target, performSelector: sel!(stackAdvance:), withObject: None::<&AnyObject>, afterDelay: 0.25f64];
                }
            }
        });
        let monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler);
        // The order is fixed for the life of this stack.
        let queue = crate::stack::StackQueue::new(self.settings.general.paste_stack_order);
        let stack = PasteStack { queue, monitor, panel: StackPanel::new(self.mtm) };
        stack.panel.show(&[], stack.queue.order());
        self.stack = Some(stack);
        self.rebuild_status_menu();
    }

    fn stack_items(&self) -> Vec<crate::model::Item> {
        let Some(stack) = &self.stack else { return Vec::new() };
        stack.queue.in_paste_order().filter_map(|id| self.store.get(id).ok().flatten()).collect()
    }

    fn push_stack(&mut self, id: i64) {
        let Some(stack) = &mut self.stack else { return };
        // The pasteboard must hold the item pasted next, which (oldest first)
        // is not the copy just made.
        if let Some(head) = stack.queue.push(id) {
            self.write_item(head, false);
        }
        let items = self.stack_items();
        if let Some(stack) = &self.stack {
            stack.panel.show(&items, stack.queue.order());
        }
    }

    fn advance_stack(&mut self) {
        let Some(stack) = &mut self.stack else { return };
        match stack.queue.advance() {
            Some(next) => {
                self.write_item(next, false);
                let items = self.stack_items();
                if let Some(stack) = &self.stack {
                    stack.panel.show(&items, stack.queue.order());
                }
            }
            None => self.toggle_stack(),
        }
    }

    fn maintenance(&mut self) {
        if self.last_maintenance.elapsed() < MAINTENANCE_INTERVAL {
            return;
        }
        self.last_maintenance = Instant::now();
        match self.store.prune(self.settings.general.history_retention, now_ms()) {
            Ok(n) if n > 0 => tracing::info!("pruned {n} old items"),
            Ok(_) => {}
            Err(e) => tracing::error!("prune failed: {e:#}"),
        }
        if let Err(e) = self.store.collect_garbage() {
            tracing::error!("blob cleanup failed: {e:#}");
        }
    }

    fn open_shelf(&mut self) {
        if self.switching.is_some() {
            objc2_app_kit::NSBeep();
            return;
        }
        if self.shelf.is_visible() {
            self.close_shelf();
            return;
        }
        let me = NSRunningApplication::currentApplication();
        self.previous_app = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .filter(|a| a.processIdentifier() != me.processIdentifier());
        let started = Instant::now();
        self.shelf.show(&self.store);
        tracing::debug!("shelf opened in {:?}", started.elapsed());
    }

    fn close_shelf(&mut self) {
        self.preview.hide();
        self.shelf.hide();
    }

    /// Leaves a search for the item's place: in history if it is there,
    /// otherwise in its first pinboard.
    fn show_item(&mut self, id: i64) {
        let in_history = self.store.item_in_history(id).unwrap_or(false);
        let board = if in_history { None } else { self.store.item_pinboards(id).unwrap_or_default().first().copied() };
        if (in_history || board.is_some()) && self.shelf.jump_to(id, board, &self.store) {
            return;
        }
        objc2_app_kit::NSBeep();
    }

    fn selected_id(&self) -> Option<i64> {
        self.shelf.selected_item().map(|i| i.id)
    }

    /// Puts an item on the pasteboard. Returns false on failure.
    fn write_item(&mut self, id: i64, plain: bool) -> bool {
        let result = if plain {
            self.store.get(id).map(|i| i.map(|i| pasteboard::write_text(&self.pasteboard, &i.text)))
        } else {
            self.store.representations(id).map(|reps| Some(pasteboard::write(&self.pasteboard, &reps)))
        };
        match result {
            Ok(Some(cc)) => {
                self.own_change = Some(cc);
                self.last_change = cc;
                let _ = self.store.mark_used(id);
                true
            }
            Ok(None) => false,
            Err(e) => {
                tracing::error!("could not load item {id}: {e:#}");
                false
            }
        }
    }

    fn paste(&mut self, id: i64, plain: bool) {
        let plain = plain ^ self.settings.general.paste_plain_text_by_default;
        if let Some(item) = self.snippet_item(id) {
            self.start_snippet(item, plain, true);
            return;
        }
        // Automatic cleanup applies to plain text and links; formatted text is left as is.
        if (self.settings.pasting.trim_whitespace || self.settings.pasting.clean_links)
            && let Ok(Some(item)) = self.store.get(id)
        {
            let cleanable = plain || matches!(item.kind, ItemKind::Text | ItemKind::Code | ItemKind::Link);
            let cleaned = self.settings.pasting.clean(&item.text);
            if cleanable && cleaned != item.text {
                let link = (item.kind == ItemKind::Link).then(|| cleaned.trim().to_string());
                self.paste_output(id, crate::transform::Output { text: cleaned, html: None }, link);
                return;
            }
        }
        self.close_shelf();
        if self.write_item(id, plain) {
            self.finish_paste();
        }
    }

    /// Pastes `item` transformed by `t`.
    fn paste_as(&mut self, id: i64, t: crate::transform::Transform) {
        let Ok(Some(item)) = self.store.get(id) else { return };
        let Some(output) = t.apply(&item.text) else { return };
        let link = (item.kind == ItemKind::Link && t == crate::transform::Transform::CleanLinks)
            .then(|| output.text.trim().to_string());
        self.paste_output(id, output, link);
    }

    /// Puts transformed text (and HTML or a URL, if any) on the pasteboard and pastes it.
    fn paste_output(&mut self, id: i64, output: crate::transform::Output, url: Option<String>) {
        self.write_output(id, output, url);
        self.finish_paste();
    }

    /// Puts transformed text (and HTML or a URL, if any) on the pasteboard.
    fn write_output(&mut self, id: i64, output: crate::transform::Output, url: Option<String>) {
        use crate::model::{Representation, UTI_HTML, UTI_PLAIN_TEXT, UTI_URL};
        self.close_shelf();
        let mut reps = vec![Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: output.text.into_bytes() }];
        if let Some(html) = output.html {
            reps.push(Representation { index: 0, uti: UTI_HTML.into(), data: html.into_bytes() });
        }
        if let Some(url) = url {
            reps.push(Representation { index: 0, uti: UTI_URL.into(), data: url.into_bytes() });
        }
        let cc = pasteboard::write(&self.pasteboard, &reps);
        self.own_change = Some(cc);
        self.last_change = cc;
        let _ = self.store.mark_used(id);
    }

    /// Adds or removes the Cmd+V listener to match the settings and the
    /// Accessibility permission (global key monitors need it).
    fn update_paste_listener(&mut self) {
        let wanted = self.settings.general.sounds && crate::paste::is_trusted();
        if wanted == self.paste_listener.is_some() {
            return;
        }
        if let Some(m) = self.paste_listener.take() {
            // SAFETY: `m` was returned by addGlobalMonitorForEventsMatchingMask.
            unsafe { NSEvent::removeMonitor(&m) };
            return;
        }
        let handler = RcBlock::new(|event: std::ptr::NonNull<NSEvent>| {
            // SAFETY: AppKit passes a valid event for the duration of the call.
            let event = unsafe { event.as_ref() };
            // Only Cmd+V (with or without Shift or Option, as in Paste and
            // Match Style); nothing else is looked at or kept.
            let flags = event.modifierFlags();
            if event.keyCode() != 9
                || !flags.contains(NSEventModifierFlags::Command)
                || flags.contains(NSEventModifierFlags::Control)
                || event.isARepeat()
            {
                return;
            }
            with(|app| {
                if app.own_paste_at.is_none_or(|t| t.elapsed() > OWN_PASTE_WINDOW) {
                    app.play(Sound::Paste);
                }
            });
        });
        self.paste_listener = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler);
    }

    fn play(&self, sound: Sound) {
        if self.settings.general.sounds {
            crate::sounds::play(sound);
        }
    }

    /// Presses Cmd+V in the app that was active before the shelf opened.
    fn finish_paste(&mut self) {
        if !self.settings.general.paste_directly || !crate::paste::is_trusted() {
            // Only copied; the user presses Cmd+V.
            self.play(Sound::Copy);
        }
        if !self.settings.general.paste_directly {
            return;
        }
        if !crate::paste::is_trusted() {
            if !self.asked_accessibility {
                self.asked_accessibility = true;
                self.open_welcome();
            }
            return;
        }
        if NSApplication::sharedApplication(self.mtm).isActive()
            && let Some(prev) = &self.previous_app
        {
            prev.activateWithOptions(NSApplicationActivationOptions::empty());
        }
        // Give the target app a moment to become key before the keystroke.
        unsafe {
            let _: () = msg_send![&*self.target, performSelector: sel!(postPaste:), withObject: None::<&AnyObject>, afterDelay: 0.06f64];
        }
    }

    /// "Paste As" submenu for an item: only transforms that change its text.
    fn paste_as_menu(&self, item: &crate::model::Item) -> Option<Retained<NSMenu>> {
        if !matches!(item.kind, ItemKind::Text | ItemKind::RichText | ItemKind::Code | ItemKind::Link) {
            return None;
        }
        let options = crate::transform::Transform::applicable(&item.text);
        let raw = self.snippet_item(item.id).is_some();
        if options.is_empty() && !raw {
            return None;
        }
        let menu = NSMenu::new(self.mtm);
        let t: &AnyObject = &self.target;
        if raw {
            menu.addItem(&menu_item(
                "Without Placeholders",
                tag(ITEM_PASTE_AS, item.id << 8 | RAW_SNIPPET),
                t,
                self.mtm,
            ));
            if !options.is_empty() {
                menu.addItem(&NSMenuItem::separatorItem(self.mtm));
            }
        }
        for (transform, _) in options {
            let arg = item.id << 8 | transform.index() as i64;
            menu.addItem(&menu_item(transform.label(), tag(ITEM_PASTE_AS, arg), t, self.mtm));
        }
        Some(menu)
    }

    fn copy(&mut self, id: i64) {
        if let Some(item) = self.snippet_item(id) {
            self.start_snippet(item, false, false);
            return;
        }
        self.close_shelf();
        self.write_item(id, false);
    }

    /// The item, if pasting it should expand snippet placeholders: it is on a
    /// pinboard, is text, and contains placeholders.
    fn snippet_item(&self, id: i64) -> Option<crate::model::Item> {
        if !self.settings.pasting.expand_placeholders {
            return None;
        }
        let item = self.store.get(id).ok()??;
        let text_like = matches!(item.kind, ItemKind::Text | ItemKind::RichText | ItemKind::Code | ItemKind::Link);
        let on_pinboard = !self.store.item_pinboards(id).unwrap_or_default().is_empty();
        (text_like && on_pinboard && crate::snippet::has_placeholders(&item.text)).then_some(item)
    }

    /// Expands a snippet, asking for `{input:...}` values first if it has any.
    fn start_snippet(&mut self, item: crate::model::Item, plain: bool, then_paste: bool) {
        let labels = crate::snippet::inputs(&item.text);
        self.close_shelf();
        if labels.is_empty() {
            self.finish_snippet(item.id, plain, then_paste, HashMap::new());
            return;
        }
        self.pending_snippet = Some(PendingSnippet { id: item.id, plain, then_paste, labels });
        // The input dialog is modal, so it runs after this callback returns.
        unsafe {
            let _: () = msg_send![&*self.target, performSelector: sel!(snippetInputs:), withObject: None::<&AnyObject>, afterDelay: 0.0f64];
        }
    }

    fn finish_snippet(&mut self, id: i64, plain: bool, then_paste: bool, inputs: HashMap<String, String>) {
        let Ok(Some(item)) = self.store.get(id) else { return };
        let clipboard = self.pasteboard.stringForType(&ns(crate::model::UTI_PLAIN_TEXT)).map(|s| s.to_string());
        let ctx = crate::snippet::Context { now: chrono::Local::now(), clipboard, inputs };
        let expanded = crate::snippet::expand(&item.text, &ctx);
        let html = if plain || item.kind != ItemKind::RichText {
            None
        } else {
            self.store
                .representation(id, crate::model::UTI_HTML)
                .ok()
                .flatten()
                .map(|h| crate::snippet::expand_html(&String::from_utf8_lossy(&h), &ctx))
        };
        let cursor = expanded.cursor_from_end.filter(|n| *n > 0 && html.is_none());
        let output = crate::transform::Output { text: expanded.text, html };
        if !then_paste {
            self.write_output(id, output, None);
            return;
        }
        self.paste_output(id, output, None);
        if let Some(n) = cursor
            && self.settings.general.paste_directly
            && crate::paste::is_trusted()
        {
            let n = NSNumber::new_usize(n);
            // After the paste lands, move the cursor back to where {cursor} was.
            unsafe {
                let _: () = msg_send![&*self.target, performSelector: sel!(moveCursor:), withObject: Some(&*n), afterDelay: 0.3f64];
            }
        }
    }

    fn delete(&mut self, id: i64) {
        // In a smart pinboard (a search over history) delete removes from history.
        let result = match self.shelf.board.filter(|_| !self.shelf.in_smart_board()) {
            Some(board) => self.store.unpin(id, board),
            None => self.store.remove_from_history(id),
        };
        if let Err(e) = result {
            tracing::error!("delete failed: {e:#}");
        }
        self.shelf.forget_image(id);
        let keep = self.shelf.selected;
        let text = self.shelf.search_text();
        self.shelf.set_query(&text, &self.store);
        self.shelf.select(keep);
    }

    fn toggle_preview(&mut self) {
        if self.preview.is_visible() {
            self.preview.hide();
            return;
        }
        let Some(item) = self.shelf.selected_item().cloned() else { return };
        let image = if item.kind == ItemKind::Image {
            IMAGE_UTIS
                .iter()
                .find_map(|uti| self.store.representation(item.id, uti).ok().flatten())
                .and_then(|d| NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(&d)))
        } else {
            None
        };
        let link = match (&item.kind, &item.meta.url) {
            (ItemKind::Link, Some(url)) => self.store.link_preview(url).ok().flatten(),
            _ => None,
        };
        let rich = crate::ui::rich_text::render(&self.store, &item, crate::ui::rich_text::Target::Preview, self.mtm);
        self.preview.show(&item, image, link, rich, self.shelf.panel.frame(), self.mtm);
    }

    fn open_item(&mut self, id: i64) {
        let Ok(Some(item)) = self.store.get(id) else { return };
        let ws = NSWorkspace::sharedWorkspace();
        if let Some(url) = item.meta.url.as_deref().and_then(|u| NSURL::URLWithString(&ns(u))) {
            self.close_shelf();
            ws.openURL(&url);
        } else if !item.meta.files.is_empty() {
            self.close_shelf();
            let urls: Vec<_> = item.meta.files.iter().map(|f| NSURL::fileURLWithPath(&ns(f))).collect();
            ws.activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&urls));
        }
    }

    /// Finished encryption switch: reopens the real store.
    fn poll_switch(&mut self) -> Option<Result<String, String>> {
        let done = self.switching.as_ref()?.lock().ok()?.take()?;
        self.switching = None;
        match Store::open_default() {
            Ok(store) => self.store = store,
            Err(e) => return Some(Err(format!("{e:#}"))),
        }
        self.refresh_settings_window();
        Some(done)
    }

    /// Finished export or restore: (title, result).
    fn poll_file_job(&mut self) -> Option<(&'static str, Result<String, String>)> {
        let (title, slot) = self.file_job.as_ref()?;
        let done = slot.lock().ok()?.take()?;
        let title = *title;
        self.file_job = None;
        if self.shelf.is_visible() {
            self.shelf.reload_tabs(&self.store);
            self.shelf.reload(&self.store);
        }
        self.refresh_settings_window();
        Some((title, done))
    }

    fn poll_import(&mut self) -> Option<Result<String, String>> {
        let done = self.import_job.as_ref()?.lock().ok()?.take()?;
        self.import_job = None;
        if let Ok(s) = Settings::load(&crate::paths::settings_path().ok()?) {
            self.settings = s;
            self.apply_settings();
            self.rebuild_status_menu();
        }
        if self.shelf.is_visible() {
            self.shelf.reload_tabs(&self.store);
            self.shelf.reload(&self.store);
        }
        self.settings_mtime = settings_mtime();
        if let Some(win) = &self.settings_window {
            win.load(&self.settings);
        }
        self.refresh_settings_window();
        Some(done)
    }
}

fn tick() {
    let finished = with(|app| {
        app.poll_clipboard();
        app.maintenance();
        app.check_settings_file();
        app.expire_items();
        app.refresh_welcome();
        app.update_paste_listener();
        if let Some(win) = &mut app.stats_window {
            win.tick();
        }
        (app.poll_import(), app.poll_file_job(), app.poll_switch())
    });
    let Some((import, file_job, switch)) = finished else { return };
    if let Some(result) = switch {
        let mtm = mtm();
        match result {
            Ok(summary) => dialogs::info("Encryption", &summary, mtm),
            Err(e) => dialogs::info("Encryption could not be changed", &e, mtm),
        }
    }
    if let Some(result) = import {
        let mtm = mtm();
        match result {
            Ok(summary) => dialogs::info("Import from Paste finished", &summary, mtm),
            Err(e) => dialogs::info("Import from Paste failed", &e, mtm),
        }
    }
    if let Some((title, result)) = file_job {
        let mtm = mtm();
        match result {
            Ok(summary) => dialogs::info(&format!("{title} finished"), &summary, mtm),
            Err(e) => dialogs::info(&format!("{title} failed"), &e, mtm),
        }
    }
}

/// Runs `work` on a background thread with its own database connection and
/// reports the outcome in a dialog when it finishes.
/// Turns encryption at rest on or off after confirming.
fn toggle_encryption() {
    let Some(state) = with(|app| {
        let busy = app.switching.is_some() || app.file_job.is_some() || app.import_job.is_some();
        (busy, app.store.is_encrypted())
    }) else {
        return;
    };
    if state.0 {
        run_modal(|mtm| dialogs::info("Please wait", "An import, export or encryption change is still running.", mtm));
        return;
    }
    let enable = !state.1;
    let (title, message, action) = if enable {
        (
            "Encrypt your clipboard history?",
            "Pasted creates a key in your login Keychain and uses it to encrypt the history database and stored \
             images and files, so other apps, other users and backups cannot read them.\n\n\
             Keep in mind:\n\
             \u{2022} If the Keychain item \u{201c}io.github.houseofmackee.pasted\u{201d} is deleted, the history cannot be recovered.\n\
             \u{2022} Exports and backups you make are not encrypted.\n\
             \u{2022} FileVault already protects the disk while the Mac is off; this adds protection while you are logged in.\n\
             \u{2022} Earlier unencrypted copies may remain in free disk space until it is reused.\n\n\
             Copying pauses for a moment while this runs.",
            "Encrypt",
        )
    } else {
        (
            "Turn off encryption?",
            "Your history is decrypted and stored as plain files again, and the key is removed from the Keychain.",
            "Decrypt",
        )
    };
    if !run_modal(|mtm| dialogs::confirm(title, message, action, mtm)) {
        return;
    }
    let slot: ImportResult = Arc::new(Mutex::new(None));
    with(|app| {
        app.close_shelf();
        // Close the real store; workers stop at the gate below.
        app.store = Store::placeholder().expect("in-memory store");
        app.switching = Some(slot.clone());
        app.refresh_settings_window();
    });
    std::thread::spawn(move || {
        let result = (|| -> Result<String> {
            let _gate = crate::encryption::DB_GATE.write();
            let (db, blobs) = (crate::paths::db_path()?, crate::paths::blobs_dir()?);
            let s =
                if enable { crate::encryption::enable(&db, &blobs)? } else { crate::encryption::disable(&db, &blobs)? };
            Ok(format!(
                "Your history is now {}. {} stored files were {}.",
                if enable { "encrypted" } else { "stored without encryption" },
                s.blobs,
                if enable { "encrypted" } else { "decrypted" }
            ))
        })()
        .map_err(|e| format!("{e:#}\n\nNothing was changed; your history is as it was."));
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(result);
        }
    });
}

fn start_file_job(title: &'static str, work: impl FnOnce(&mut Store) -> Result<String> + Send + 'static) {
    if with(|app| app.file_job.is_some() || app.import_job.is_some() || app.switching.is_some()).unwrap_or(false) {
        run_modal(|mtm| dialogs::info("Please wait", "Another import or export is still running.", mtm));
        return;
    }
    let slot: ImportResult = Arc::new(Mutex::new(None));
    with(|app| app.file_job = Some((title, slot.clone())));
    std::thread::spawn(move || {
        let result = Store::open_default().and_then(|mut store| work(&mut store)).map_err(|e| format!("{e:#}"));
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(result);
        }
    });
}

fn export_history() {
    use crate::export::Format;
    let chosen = run_modal(|mtm| {
        let panel = NSSavePanel::savePanel(mtm);
        panel.setMessage(Some(&ns("Export clipboard history and pinboards")));
        panel.setNameFieldStringValue(&ns(&crate::export::default_file_name(Format::Zip).to_string_lossy()));
        let formats = objc2_app_kit::NSPopUpButton::initWithFrame_pullsDown(
            objc2_app_kit::NSPopUpButton::alloc(mtm),
            crate::ui::rect(0.0, 0.0, 380.0, 26.0),
            false,
        );
        for f in Format::ALL {
            formats.addItemWithTitle(&ns(f.label()));
        }
        let accessory = objc2_app_kit::NSView::initWithFrame(
            objc2_app_kit::NSView::alloc(mtm),
            crate::ui::rect(0.0, 0.0, 420.0, 40.0),
        );
        formats.setFrame(crate::ui::rect(20.0, 8.0, 380.0, 26.0));
        accessory.addSubview(&formats);
        panel.setAccessoryView(Some(&accessory));
        if panel.runModal() != NSModalResponseOK {
            return None;
        }
        let format = Format::ALL.get(formats.indexOfSelectedItem().max(0) as usize).copied().unwrap_or(Format::Zip);
        let mut path = std::path::PathBuf::from(panel.URL()?.path()?.to_string());
        if Format::from_path(&path) != Some(format) {
            path.set_extension(format.extension());
        }
        Some((path, format))
    });
    let Some((path, format)) = chosen else { return };
    start_file_job("Export", move |store| {
        let summary = crate::export::export(store, &path, format, None)?;
        Ok(summary.describe(&path))
    });
}

fn restore_backup() {
    let chosen = run_modal(|mtm| {
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        panel.setMessage(Some(&ns("Choose a Pasted backup (.zip). Items you already have are skipped.")));
        (panel.runModal() == NSModalResponseOK).then(|| panel.URL()).flatten()
    });
    let Some(path) = chosen.and_then(|u| u.path()).map(|p| std::path::PathBuf::from(p.to_string())) else { return };
    start_file_job("Restore", move |store| Ok(crate::export::restore(store, &path)?.describe()));
}

pub fn on_hotkey(action: HotkeyAction) {
    match action {
        HotkeyAction::OpenShelf => {
            with(|app| app.open_shelf());
        }
        HotkeyAction::PasteStack => {
            with(|app| app.toggle_stack());
        }
    }
}

fn search_changed() {
    with(|app| {
        let text = app.shelf.search_text();
        if text == " " {
            // Space on an empty search toggles the preview, like Quick Look.
            app.shelf.search.setStringValue(&ns(""));
            app.toggle_preview();
            return;
        }
        app.preview.hide();
        app.shelf.set_query(&text, &app.store);
    });
}

fn shelf_command(command: Sel) -> bool {
    let name = command.name().to_str().unwrap_or("");
    let shift = shift_held();
    if name == "insertNewlineIgnoringFieldEditor:" {
        // Option-Return: choose a cleanup, then paste.
        show_paste_as_menu();
        return true;
    }
    with(|app| match name {
        "moveLeft:" => {
            app.shelf.move_selection(-1, &app.store);
            app.preview.hide();
            true
        }
        "moveRight:" => {
            app.shelf.move_selection(1, &app.store);
            app.preview.hide();
            true
        }
        "moveUp:" | "moveDown:" => true,
        "moveToBeginningOfDocument:" | "scrollToBeginningOfDocument:" => {
            app.shelf.select(0);
            true
        }
        "moveToEndOfDocument:" | "scrollToEndOfDocument:" => {
            let last = app.shelf.items.len().saturating_sub(1);
            app.shelf.select(last);
            true
        }
        "insertNewline:" => {
            if let Some(id) = app.selected_id() {
                app.paste(id, shift);
            }
            true
        }
        "insertTab:" => {
            app.shelf.cycle_board(1, &app.store);
            true
        }
        "insertBacktab:" => {
            app.shelf.cycle_board(-1, &app.store);
            true
        }
        "cancelOperation:" => {
            escape(app);
            true
        }
        _ => false,
    })
    .unwrap_or(false)
}

/// Asks for a pending snippet's `{input:...}` values, then pastes it.
fn snippet_inputs() {
    let Some(p) = with(|app| app.pending_snippet.take()).flatten() else { return };
    let message = "Values are inserted where the snippet has {input:...} placeholders.";
    let Some(values) = run_modal(|mtm| dialogs::prompt_fields("Fill In Snippet", message, &p.labels, mtm)) else {
        return;
    };
    let inputs: HashMap<String, String> = p.labels.into_iter().zip(values).collect();
    with(|app| app.finish_snippet(p.id, p.plain, p.then_paste, inputs));
}

/// Pops the "Paste As" menu over the selected card. The menu runs its own
/// event loop, so it is shown without holding the state borrow.
fn show_paste_as_menu() {
    let Some((menu, card)) = with(|app| {
        let item = app.shelf.selected_item()?.clone();
        Some((app.paste_as_menu(&item)?, app.shelf.selected_card()?))
    })
    .flatten() else {
        objc2_app_kit::NSBeep();
        return;
    };
    let point = objc2_foundation::NSPoint::new(12.0, crate::ui::card::CARD_H - 62.0);
    menu.popUpMenuPositioningItem_atLocation_inView(None, point, Some(&card));
}

fn escape(app: &mut App) {
    if app.preview.is_visible() {
        app.preview.hide();
    } else if !app.shelf.search_text().is_empty() {
        app.shelf.search.setStringValue(&ns(""));
        app.shelf.set_query("", &app.store);
    } else {
        app.close_shelf();
    }
}

pub fn shelf_escape() {
    with(escape);
}

fn shelf_resigned() {
    with(|app| {
        if !app.modal {
            app.close_shelf();
        }
    });
}

fn search_has_selection(app: &App) -> bool {
    // SAFETY: asking for the existing field editor of our own search field.
    unsafe { app.shelf.panel.fieldEditor_forObject(false, Some(&app.shelf.search)) }
        .map(|editor| editor.selectedRange().length > 0)
        .unwrap_or(false)
}

/// Cmd shortcuts inside the shelf. Returns true when handled.
pub fn shelf_key_equivalent(event: &NSEvent) -> bool {
    let flags = event.modifierFlags();
    if !flags.contains(NSEventModifierFlags::Command) {
        return false;
    }
    let shift = flags.contains(NSEventModifierFlags::Shift);
    let key = event.charactersIgnoringModifiers().map(|s| s.to_string().to_lowercase()).unwrap_or_default();
    let code = event.keyCode();

    enum After {
        Nothing,
        Edit,
        Rename,
        NewBoard(Option<i64>),
        SaveSearch,
        Settings,
    }
    let after = with(|app| {
        let mods = crate::shortcut::Modifiers {
            cmd: true,
            shift,
            option: flags.contains(NSEventModifierFlags::Option),
            control: flags.contains(NSEventModifierFlags::Control),
        };
        let pressed = crate::shortcut::Shortcut { key_code: code, modifiers: mods };
        if pressed == app.settings.shortcuts.next_pinboard {
            app.shelf.cycle_board(1, &app.store);
            return Some(After::Nothing);
        }
        if pressed == app.settings.shortcuts.previous_pinboard {
            app.shelf.cycle_board(-1, &app.store);
            return Some(After::Nothing);
        }
        let selected = app.selected_id();
        match key.as_str() {
            k if k.len() == 1 && ('1'..='9').contains(&k.chars().next().unwrap()) => {
                let index = k.parse::<usize>().unwrap() - 1;
                if let Some(id) = app.shelf.items.get(index).map(|i| i.id) {
                    app.paste(id, shift);
                }
                Some(After::Nothing)
            }
            "c" if !search_has_selection(app) => {
                if let Some(id) = selected {
                    app.copy(id)
                }
                Some(After::Nothing)
            }
            "y" => {
                app.toggle_preview();
                Some(After::Nothing)
            }
            "e" => Some(After::Edit),
            "r" => Some(After::Rename),
            "n" => Some(After::NewBoard(None)),
            "o" => {
                if let Some(id) = selected {
                    app.open_item(id)
                }
                Some(After::Nothing)
            }
            "f" => {
                let responder: &NSResponder = &app.shelf.search;
                app.shelf.panel.makeFirstResponder(Some(responder));
                Some(After::Nothing)
            }
            "w" => {
                app.close_shelf();
                Some(After::Nothing)
            }
            "g" => {
                if let Some(id) = selected {
                    app.show_item(id);
                }
                Some(After::Nothing)
            }
            "," => Some(After::Settings),
            "s" => Some(After::SaveSearch),
            _ if code == 51 => {
                if let Some(id) = selected {
                    app.delete(id)
                }
                Some(After::Nothing)
            }
            _ => None,
        }
    })
    .flatten();

    match after {
        None => false,
        Some(After::Nothing) => true,
        Some(After::Edit) => {
            edit_selected();
            true
        }
        Some(After::Rename) => {
            rename_selected();
            true
        }
        Some(After::NewBoard(item)) => {
            new_pinboard(item);
            true
        }
        Some(After::SaveSearch) => {
            save_search_as_smart_pinboard();
            true
        }
        Some(After::Settings) => {
            with(|app| app.open_settings());
            true
        }
    }
}

pub fn card_clicked(index: usize, clicks: isize) {
    with(|app| {
        app.shelf.select(index);
        if clicks >= 2
            && let Some(id) = app.selected_id()
        {
            app.paste(id, shift_held());
        }
    });
}

pub fn card_menu(index: usize) -> Option<Retained<NSMenu>> {
    with(|app| {
        app.shelf.select(index);
        let item = app.shelf.selected_item()?.clone();
        let mtm = app.mtm;
        let t: &AnyObject = &app.target;
        let menu = NSMenu::new(mtm);
        let add = |m: &NSMenu, title: &str, code: u8, arg: i64| {
            let it = menu_item(title, tag(code, arg), t, mtm);
            m.addItem(&it);
            it
        };
        add(&menu, "Paste", ITEM_PASTE, item.id);
        add(&menu, "Paste as Plain Text", ITEM_PASTE_PLAIN, item.id);
        if let Some(sub) = app.paste_as_menu(&item) {
            let paste_as = menu_item("Paste As", 0, t, mtm);
            paste_as.setSubmenu(Some(&sub));
            menu.addItem(&paste_as);
        }
        add(&menu, "Copy", ITEM_COPY, item.id);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(&menu, "Preview", ITEM_PREVIEW, item.id);
        if item.meta.url.is_some() {
            add(&menu, "Open Link", ITEM_OPEN, item.id);
        } else if !item.meta.files.is_empty() {
            add(&menu, "Show in Finder", ITEM_OPEN, item.id);
        }
        if matches!(item.kind, ItemKind::Text | ItemKind::RichText | ItemKind::Code | ItemKind::Link | ItemKind::Color)
        {
            add(&menu, "Edit\u{2026}", ITEM_EDIT, item.id);
        }
        add(&menu, "Rename\u{2026}", ITEM_RENAME, item.id);
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Leave a search (or another pinboard) for the item's place in history or a pinboard.
        let on = app.store.item_pinboards(item.id).unwrap_or_default();
        let mut shows = 0;
        if (app.shelf.is_searching() || app.shelf.board.is_some())
            && app.store.item_in_history(item.id).unwrap_or(false)
        {
            add(&menu, "Show in Clipboard", ITEM_SHOW_HISTORY, item.id).setKeyEquivalent(&ns("g"));
            shows += 1;
        }
        for b in app.shelf.boards.iter().filter(|b| on.contains(&b.id)) {
            if app.shelf.board != Some(b.id) || app.shelf.is_searching() {
                add(&menu, &format!("Show in {}", b.name), ITEM_SHOW_BOARD, b.id);
                shows += 1;
            }
        }
        if shows > 0 {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
        }

        let pins = NSMenu::new(mtm);
        for b in app.shelf.boards.iter().filter(|b| !b.is_smart()) {
            let it = add(&pins, &b.name, ITEM_PIN, b.id);
            if on.contains(&b.id) {
                it.setState(NSControlStateValueOn);
            }
        }
        if app.shelf.boards.iter().any(|b| !b.is_smart()) {
            pins.addItem(&NSMenuItem::separatorItem(mtm));
        }
        add(&pins, "New Pinboard\u{2026}", ITEM_PIN_NEW, item.id);
        let pin_item = menu_item("Pin to", 0, t, mtm);
        pin_item.setSubmenu(Some(&pins));
        menu.addItem(&pin_item);
        if app.shelf.board.is_some() && !app.shelf.in_smart_board() {
            add(&menu, "Remove from Pinboard", ITEM_UNPIN, item.id);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(&menu, "Delete", ITEM_DELETE, item.id);
        Some(menu)
    })
    .flatten()
}

pub fn card_drag_item(index: usize) -> Option<Retained<NSPasteboardItem>> {
    with(|app| {
        let id = app.shelf.items.get(index)?.id;
        let reps = app.store.representations(id).ok()?;
        let first = reps.first()?.index;
        let item = NSPasteboardItem::new();
        for r in reps.iter().filter(|r| r.index == first) {
            item.setData_forType(&NSData::with_bytes(&r.data), &ns(&r.uti));
        }
        Some(item)
    })
    .flatten()
}

/// Left click opens (or closes) the shelf; right-click or Control-click
/// shows the menu.
fn status_item_clicked() {
    let mtm = mtm();
    // Only a click that just happened counts: an Accessibility press (for
    // example from VoiceOver) leaves an older event as the current one.
    let now = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
    let wants_menu = NSApplication::sharedApplication(mtm).currentEvent().is_some_and(|e| {
        now - e.timestamp() < 1.0
            && (e.r#type() == NSEventType::RightMouseUp || e.modifierFlags().contains(NSEventModifierFlags::Control))
    });
    if !wants_menu {
        with(|app| app.open_shelf());
        return;
    }
    show_status_menu();
}

/// The menu bar icon's menu, under the icon.
fn show_status_menu() {
    let mtm = mtm();
    // Not borrowed while the menu is open: choosing an item runs its action
    // before the menu returns.
    let Some((item, menu)) = with(|app| {
        app.close_shelf();
        // Fresh, so an update found in the background shows up.
        app.rebuild_status_menu();
        (app.status_item.clone(), app.status_menu.clone())
    }) else {
        return;
    };
    let Some(button) = item.button(mtm) else { return };
    // Popped up under the icon rather than attached to the status item, so
    // the button keeps sending clicks to us.
    // Same look as a system menu bar menu: the system appearance, not the
    // menu bar's (which follows the wallpaper).
    objc2_app_kit::NSAppearanceCustomization::setAppearance(
        &*menu,
        Some(&NSApplication::sharedApplication(mtm).effectiveAppearance()),
    );
    button.highlight(true);
    let below = objc2_foundation::NSPoint::new(0.0, button.bounds().size.height + 5.0);
    menu.popUpMenuPositioningItem_atLocation_inView(None, below, Some(&button));
    button.highlight(false);
}

/// VoiceOver's "show menu" (VO-Shift-M) on the menu bar icon opens the
/// same menu as a right-click. The system's status bar button cannot be
/// subclassed, so the method is added to its class.
fn add_show_menu_action(button: &objc2_app_kit::NSStatusBarButton) {
    extern "C-unwind" fn perform_show_menu(_this: &AnyObject, _cmd: Sel) -> objc2::runtime::Bool {
        // Later: the menu runs its own event loop, which would hold up
        // the Accessibility request until it closes.
        if let Some(target) = with(|app| app.target.clone()) {
            // SAFETY: the target implements `showStatusMenu:`.
            unsafe {
                let _: () = msg_send![&*target, performSelector: sel!(showStatusMenu:), withObject: None::<&AnyObject>, afterDelay: 0.0f64];
            }
        }
        objc2::runtime::Bool::YES
    }
    // SAFETY: the function matches the selector's signature
    // (`- (BOOL)accessibilityPerformShowMenu`, encoded "B@:").
    unsafe {
        let class = objc2::ffi::object_getClass((button as *const objc2_app_kit::NSStatusBarButton).cast());
        let imp: objc2::runtime::Imp =
            std::mem::transmute(perform_show_menu as extern "C-unwind" fn(&AnyObject, Sel) -> objc2::runtime::Bool);
        objc2::ffi::class_addMethod(class.cast_mut(), sel!(accessibilityPerformShowMenu), imp, c"B@:".as_ptr());
    }
}

/// Sends left and right clicks on the menu bar icon to `statusItemClicked:`.
fn connect_status_button(button: &objc2_app_kit::NSStatusBarButton, target: &Target) {
    // SAFETY: the target implements `statusItemClicked:` and lives as long as the app.
    unsafe {
        button.setTarget(Some(target));
        button.setAction(Some(sel!(statusItemClicked:)));
    }
    button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
}

fn dispatch(raw_tag: isize) {
    let (code, arg) = untag(raw_tag);
    match code {
        OPEN_SHELF => {
            with(|app| app.open_shelf());
        }
        TOGGLE_PAUSE => {
            with(|app| {
                app.paused = !app.paused;
                app.rebuild_status_menu();
            });
        }
        IMPORT_PASTE => import_from_paste(),
        PASTE_STACK => {
            with(|app| app.toggle_stack());
        }
        CLEAR_HISTORY => clear_history(arg),
        OPEN_SETTINGS => open_settings_file(),
        STATISTICS => {
            with(|app| app.open_stats());
        }
        SETTINGS => {
            with(|app| app.open_settings());
        }
        settings_window::ADD_APP => add_excluded_app(),
        settings_window::ADD_RULE => edit_rule(None),
        settings_window::EXPORT => export_history(),
        settings_window::TOGGLE_ENCRYPTION => toggle_encryption(),
        settings_window::COPY_CLAUDE_CODE | settings_window::COPY_DESKTOP_CONFIG => {
            let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "pasted".into());
            let text = if code == settings_window::COPY_CLAUDE_CODE {
                format!("claude mcp add pasted -- \"{exe}\" mcp")
            } else {
                serde_json::to_string_pretty(&serde_json::json!({
                    "mcpServers": { "pasted": { "command": exe, "args": ["mcp"] } }
                }))
                .unwrap_or_default()
            };
            with(|app| {
                let cc = pasteboard::write_text(&app.pasteboard, &text);
                // Not captured into history: it is a setup snippet, not something the user copied.
                app.own_change = Some(cc);
                app.last_change = cc;
            });
            let (title, message) = if code == settings_window::COPY_CLAUDE_CODE {
                ("Command copied", "Paste it into Terminal to add Pasted to Claude Code.")
            } else {
                (
                    "Configuration copied",
                    "In Claude Desktop, open Settings > Developer > Edit Config and merge this into claude_desktop_config.json, then restart Claude Desktop.",
                )
            };
            run_modal(|mtm| dialogs::info(title, message, mtm));
        }
        settings_window::OPEN_MCP_LOG => {
            if let Ok(dir) = crate::paths::data_dir() {
                let log = dir.join("mcp.log");
                if !log.exists() {
                    let _ = std::fs::write(&log, "");
                }
                let _ = std::process::Command::new("open").arg("-t").arg(&log).spawn();
            }
        }
        settings_window::RESTORE => restore_backup(),
        settings_window::EDIT_RULE => {
            if let Some(row) = with(|app| app.settings_window.as_ref().and_then(|w| w.selected_rule_row())).flatten() {
                edit_rule(Some(row));
            }
        }
        settings_window::REMOVE_RULE => {
            let Some((row, name)) = with(|app| {
                let row = app.settings_window.as_ref()?.selected_rule_row()?;
                let r = app.settings.rules.get(row)?;
                Some((row, if r.name.is_empty() { r.summary() } else { r.name.clone() }))
            })
            .flatten() else {
                return;
            };
            if run_modal(|mtm| dialogs::confirm(&format!("Remove the rule \u{201c}{name}\u{201d}?"), "", "Remove", mtm))
            {
                with(|app| {
                    if row < app.settings.rules.len() {
                        app.settings.rules.remove(row);
                        app.save_settings();
                        if let Some(win) = &app.settings_window {
                            win.load(&app.settings);
                        }
                    }
                });
            }
        }
        settings_window::REMOVE_APP => {
            with(|app| {
                let Some(row) = app.settings_window.as_ref().and_then(|w| w.selected_app_row()) else { return };
                if row < app.settings.privacy.excluded_apps.len() {
                    app.settings.privacy.excluded_apps.remove(row);
                    app.save_settings();
                    if let Some(win) = &app.settings_window {
                        win.load(&app.settings);
                    }
                }
            });
        }
        settings_window::OPEN_ACCESSIBILITY => {
            crate::paste::request_trust();
            if let Some(url) = NSURL::URLWithString(&ns(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            )) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
        }
        settings_window::RESTORE_SHORTCUTS => {
            with(|app| {
                app.settings.shortcuts = Default::default();
                app.save_settings();
                if let Some(win) = &app.settings_window {
                    win.load(&app.settings);
                }
            });
        }
        settings_window::SAVE_PATTERNS => {
            with(|app| {
                let Some(result) = app.settings_window.as_ref().map(|w| w.read_patterns()) else { return };
                match result {
                    Ok(patterns) => {
                        let n = patterns.len();
                        app.settings.privacy.ignore_patterns = patterns;
                        app.save_settings();
                        if let Some(win) = &app.settings_window {
                            win.set_patterns_status(
                                &format!("Saved {n} pattern{}.", if n == 1 { "" } else { "s" }),
                                false,
                            );
                        }
                    }
                    Err(e) => {
                        if let Some(win) = &app.settings_window {
                            win.set_patterns_status(&e, true);
                        }
                    }
                }
            });
        }
        settings_window::OPEN_DATA_FOLDER => {
            if let Ok(dir) = crate::paths::data_dir() {
                NSWorkspace::sharedWorkspace().openURL(&NSURL::fileURLWithPath(&ns(&dir.display().to_string())));
            }
        }

        crate::ui::stats_window::COPY_DATA => {
            with(|app| app.copy_stats_data());
        }
        WELCOME => {
            with(|app| app.open_welcome());
        }
        crate::ui::welcome::WELCOME_DONE => {
            with(|app| {
                if let Some(win) = &app.welcome {
                    win.close();
                }
            });
        }
        ABOUT => about(),
        CHECK_UPDATES => crate::updates::check(mtm()),
        QUIT => NSApplication::sharedApplication(mtm()).terminate(None),
        ITEM_SHOW_HISTORY => {
            with(|app| {
                if !app.shelf.jump_to(arg, None, &app.store) {
                    objc2_app_kit::NSBeep();
                }
            });
        }
        ITEM_SHOW_BOARD => {
            with(|app| {
                let Some(id) = app.selected_id() else { return };
                if !app.shelf.jump_to(id, Some(arg), &app.store) {
                    objc2_app_kit::NSBeep();
                }
            });
        }
        ITEM_PASTE | ITEM_PASTE_PLAIN => {
            with(|app| app.paste(arg, code == ITEM_PASTE_PLAIN));
        }
        ITEM_PASTE_AS => {
            let (id, index) = (arg >> 8, (arg & 0xff) as usize);
            if index == RAW_SNIPPET as usize {
                with(|app| {
                    if let Ok(Some(item)) = app.store.get(id) {
                        app.paste_output(id, crate::transform::Output { text: item.text, html: None }, None);
                    }
                });
            } else if let Some(t) = crate::transform::Transform::ALL.get(index).copied() {
                with(|app| app.paste_as(id, t));
            }
        }
        ITEM_COPY => {
            with(|app| app.copy(arg));
        }
        ITEM_PREVIEW => {
            with(|app| app.toggle_preview());
        }
        ITEM_OPEN => {
            with(|app| app.open_item(arg));
        }
        ITEM_EDIT => edit_selected(),
        ITEM_RENAME => rename_selected(),
        ITEM_PIN => {
            with(|app| {
                let Some(id) = app.selected_id() else { return };
                let pinned = app.store.item_pinboards(id).unwrap_or_default().contains(&arg);
                let result = if pinned { app.store.unpin(id, arg) } else { app.store.pin(id, arg, None) };
                if let Err(e) = result {
                    tracing::error!("pin failed: {e:#}");
                }
                app.shelf.reload_tabs(&app.store);
            });
        }
        ITEM_PIN_NEW => new_pinboard(Some(arg)),
        ITEM_UNPIN | ITEM_DELETE => {
            with(|app| app.delete(arg));
        }
        shelf::TAB_SELECT => {
            with(|app| app.shelf.select_board(if arg < 0 { None } else { Some(arg) }, &app.store));
        }
        shelf::TAB_NEW => {
            let searching = with(|app| app.shelf.is_searching()).unwrap_or(false);
            if !searching {
                new_pinboard(None);
                return;
            }
            let choice = run_modal(|mtm| {
                dialogs::choose(
                    "New pinboard",
                    "A smart pinboard shows everything in history that matches the current search.",
                    &["Smart Pinboard from Search", "Pinboard"],
                    mtm,
                )
            });
            match choice {
                Some(0) => save_search_as_smart_pinboard(),
                Some(_) => new_pinboard(None),
                None => {}
            }
        }
        shelf::BOARD_EDIT_SEARCH => {
            let Some(query) = with(|app| app.store.pinboard_query(arg).ok().flatten()).flatten() else { return };
            let Some(new) = run_modal(|mtm| {
                dialogs::prompt("Edit Search", "Items in history matching this search are shown:", &query, mtm)
            }) else {
                return;
            };
            with(|app| {
                if let Err(e) = app.store.set_pinboard_query(arg, &new) {
                    tracing::warn!("smart pinboard search not changed: {e:#}");
                    return;
                }
                let text = app.shelf.search_text();
                app.shelf.reload_tabs(&app.store);
                app.shelf.set_query(&text, &app.store);
            });
        }
        shelf::BOARD_RENAME => {
            let Some(name) =
                with(|app| app.shelf.boards.iter().find(|b| b.id == arg).map(|b| b.name.clone())).flatten()
            else {
                return;
            };
            if let Some(new) = run_modal(|mtm| dialogs::prompt("Rename Pinboard", "", &name, mtm))
                && !new.trim().is_empty()
            {
                with(|app| {
                    let _ = app.store.rename_pinboard(arg, &new);
                    app.shelf.reload_tabs(&app.store);
                });
            }
        }
        shelf::BOARD_COLOR => {
            let (board, color_index) = (arg >> 8, (arg & 0xff) as usize);
            if let Some((_, color)) = shelf::BOARD_COLOR_NAMES.get(color_index) {
                with(|app| {
                    let _ = app.store.set_pinboard_color(board, *color);
                    app.shelf.reload_tabs(&app.store);
                });
            }
        }
        shelf::BOARD_DELETE => {
            let Some((name, smart)) =
                with(|app| app.shelf.boards.iter().find(|b| b.id == arg).map(|b| (b.name.clone(), b.is_smart())))
                    .flatten()
            else {
                return;
            };
            let message = if smart {
                "Only the saved search is removed; no items are deleted."
            } else {
                "Items that are only on this pinboard will be deleted."
            };
            let ok = run_modal(|mtm| {
                dialogs::confirm(&format!("Delete the pinboard \u{201c}{name}\u{201d}?"), message, "Delete", mtm)
            });
            if ok {
                with(|app| {
                    let _ = app.store.delete_pinboard(arg);
                    if app.shelf.board == Some(arg) {
                        app.shelf.board = None;
                    }
                    app.shelf.reload_tabs(&app.store);
                    let text = app.shelf.search_text();
                    app.shelf.set_query(&text, &app.store);
                });
            }
        }
        _ => {}
    }
}

fn edit_selected() {
    let Some((id, text)) = with(|app| app.shelf.selected_item().map(|i| (i.id, i.text.clone()))).flatten() else {
        return;
    };
    if let Some(new) = run_modal(|mtm| dialogs::edit_text("Edit Item", &text, mtm)) {
        with(|app| {
            if let Err(e) = app.store.set_text(id, &new) {
                tracing::error!("edit failed: {e:#}");
            }
            let keep = app.shelf.selected;
            app.shelf.reload(&app.store);
            app.shelf.select(keep);
        });
    }
}

fn rename_selected() {
    let Some((id, title)) =
        with(|app| app.shelf.selected_item().map(|i| (i.id, i.title.clone().unwrap_or_default()))).flatten()
    else {
        return;
    };
    if let Some(new) = run_modal(|mtm| dialogs::prompt("Rename Item", "Leave empty to remove the title.", &title, mtm))
    {
        with(|app| {
            let _ = app.store.set_title(id, Some(&new));
            let keep = app.shelf.selected;
            app.shelf.reload(&app.store);
            app.shelf.select(keep);
        });
    }
}

/// Creates a pinboard, optionally pinning `item` to it.
/// Saves the shelf's current search as a smart pinboard and opens it. In a
/// pinboard tab the search stays limited to that pinboard; in a smart one it
/// adds to its saved search.
fn save_search_as_smart_pinboard() {
    let Some((query, suggested)) = with(|app| {
        let typed = app.shelf.search_text().trim().to_string();
        let base = app.shelf.current_board().map(|b| match &b.query {
            Some(saved) => saved.clone(),
            None => format!("board:{}", b.name.split_whitespace().collect::<Vec<_>>().join("")),
        });
        let query = [base.unwrap_or_default(), typed.clone()].join(" ").trim().to_string();
        (!typed.is_empty()).then_some((query, typed))
    })
    .flatten() else {
        objc2_app_kit::NSBeep();
        return;
    };
    let message = format!("Shows everything in history matching \u{201c}{query}\u{201d}.\n\nName:");
    let Some(name) = run_modal(|mtm| dialogs::prompt("New Smart Pinboard", &message, &suggested, mtm)) else { return };
    if name.trim().is_empty() {
        return;
    }
    with(|app| match app.store.create_smart_pinboard(&name, &query, None) {
        Ok(board) => {
            app.shelf.search.setStringValue(&ns(""));
            app.shelf.select_board(Some(board), &app.store);
        }
        Err(e) => tracing::error!("creating smart pinboard failed: {e:#}"),
    });
}

fn new_pinboard(item: Option<i64>) {
    let Some(name) = run_modal(|mtm| dialogs::prompt("New Pinboard", "Name:", "", mtm)) else { return };
    if name.trim().is_empty() {
        return;
    }
    with(|app| match app.store.create_pinboard(&name, None, None) {
        Ok(board) => {
            if let Some(id) = item {
                let _ = app.store.pin(id, board, None);
            }
            app.shelf.reload_tabs(&app.store);
        }
        Err(e) => tracing::error!("creating pinboard failed: {e:#}"),
    });
}

/// Periods offered for clearing history: (label, minutes; 0 means everything).
const CLEAR_CHOICES: &[(&str, i64)] =
    &[("Last 15 Minutes", 15), ("Last Hour", 60), ("Last 24 Hours", 24 * 60), ("All History", 0)];

/// Clears history copied in the last `minutes` (0: all of it), after
/// confirming. A negative value asks which period first.
fn clear_history(minutes: i64) {
    let minutes = if minutes < 0 {
        let labels: Vec<&str> = CLEAR_CHOICES.iter().map(|(l, _)| *l).collect();
        let choice = run_modal(|mtm| {
            dialogs::choose(
                "Clear clipboard history",
                "Which items should be removed? Pinboards are kept.",
                &labels,
                mtm,
            )
        });
        match choice {
            Some(i) => CLEAR_CHOICES[i].1,
            None => return,
        }
    } else {
        minutes
    };
    let since = (minutes > 0).then(|| now_ms() - minutes * 60_000);
    let Some(Ok(count)) = with(|app| app.store.count_history_since(since)) else { return };
    let period = CLEAR_CHOICES.iter().find(|(_, m)| *m == minutes).map_or("", |(l, _)| *l).to_lowercase();
    if count == 0 {
        run_modal(|mtm| dialogs::info("Nothing to clear", &format!("Nothing was copied in the {period}."), mtm));
        return;
    }
    let items = if count == 1 { "1 item".to_string() } else { format!("{count} items") };
    let (title, message) = if since.is_some() {
        (format!("Clear {items} copied in the {period}?"), "They are removed from history. Pinboards are kept.")
    } else {
        (format!("Clear all {items} in history?"), "Pinboards are kept.")
    };
    if !run_modal(|mtm| dialogs::confirm(&title, message, "Clear", mtm)) {
        return;
    }
    with(|app| {
        if let Err(e) = app.store.clear_history(since) {
            tracing::error!("clear history failed: {e:#}");
        }
        let _ = app.store.collect_garbage();
        if app.shelf.is_visible() && app.shelf.board.is_none() {
            app.shelf.reload(&app.store);
        }
        app.refresh_settings_window();
    });
}

fn open_settings_file() {
    if let Ok(path) = crate::paths::settings_path() {
        if !path.exists() {
            let _ = Settings::default().save(&path);
        }
        let _ = std::process::Command::new("open").arg("-t").arg(&path).spawn();
    }
}

fn settings_mtime() -> Option<std::time::SystemTime> {
    let path = crate::paths::settings_path().ok()?;
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// A control in the Settings window changed.
pub fn settings_ui_changed() {
    with(|app| {
        let Some(win) = &app.settings_window else { return };
        let mut settings = app.settings.clone();
        win.read(&mut settings);
        if settings != app.settings {
            app.settings = settings;
            app.save_settings();
        }
    });
}

/// Global hotkeys are paused while a shortcut field records, so the current
/// combination can be typed into it.
pub fn set_hotkeys_suspended(suspended: bool) {
    with(|app| {
        if suspended {
            app.hotkeys.set(&[]);
        } else {
            app.apply_settings();
        }
    });
}

/// Adds a rule (`None`) or edits the rule at `row`.
fn edit_rule(row: Option<usize>) {
    let Some((rule, boards)) = with(|app| {
        let rule = match row {
            Some(r) => app.settings.rules.get(r)?.clone(),
            None => crate::rules::Rule::default(),
        };
        let boards: Vec<String> =
            app.store.pinboards().unwrap_or_default().into_iter().filter(|b| !b.is_smart()).map(|b| b.name).collect();
        Some((rule, boards))
    })
    .flatten() else {
        return;
    };
    let title = if row.is_some() { "Edit Rule" } else { "New Rule" };
    let Some(edited) = run_modal(|mtm| crate::ui::rule_editor::edit_rule(&rule, title, &boards, mtm)) else { return };
    with(|app| {
        match row {
            Some(r) if r < app.settings.rules.len() => app.settings.rules[r] = edited,
            _ => app.settings.rules.push(edited),
        }
        app.save_settings();
        if let Some(win) = &app.settings_window {
            win.load(&app.settings);
            win.show();
        }
    });
}

fn add_excluded_app() {
    let url = run_modal(|mtm| {
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&ns("/Applications"))));
        panel.setMessage(Some(&ns("Choose an app whose copies Pasted should never save")));
        (panel.runModal() == NSModalResponseOK).then(|| panel.URL()).flatten()
    });
    let Some(url) = url else { return };
    let Some(bundle_id) = NSBundle::bundleWithURL(&url).and_then(|b| b.bundleIdentifier()).map(|s| s.to_string())
    else {
        run_modal(|mtm| dialogs::info("Not an app", "Choose an application bundle (.app).", mtm));
        return;
    };
    with(|app| {
        if !app.settings.privacy.excluded_apps.contains(&bundle_id) {
            app.settings.privacy.excluded_apps.push(bundle_id);
            app.save_settings();
        }
        if let Some(win) = &app.settings_window {
            win.load(&app.settings);
        }
    });
}

fn about() {
    let dir = crate::paths::data_dir().map(|d| d.display().to_string()).unwrap_or_default();
    run_modal(|mtm| {
        dialogs::info(
            &format!("Pasted {}", env!("CARGO_PKG_VERSION")),
            &format!(
                "A fast, private clipboard manager for macOS.\nOpen source under MIT OR Apache-2.0.\n\nData: {dir}"
            ),
            mtm,
        )
    });
}

fn import_from_paste() {
    if with(|app| app.import_job.is_some()).unwrap_or(false) {
        run_modal(|mtm| dialogs::info("Import in progress", "Please wait for the current import to finish.", mtm));
        return;
    }
    let Some(install) = paste_app::detect().into_iter().next() else {
        run_modal(|mtm| dialogs::info("Paste not found", "No Paste history was found on this Mac.", mtm));
        return;
    };
    let running = paste_app::is_running(&install);
    let mut message = format!(
        "History, pinboards and settings will be copied from {}.\nPaste's own data is not modified, and running the import again only adds new items.",
        install.data_dir.display()
    );
    if running {
        message.push_str("\n\nPaste is running. Quitting it first gives the most complete import.");
    }
    let filtering = with(|app| app.settings.import.any()).unwrap_or(false);
    message.push_str(if filtering {
        "\n\nThe import privacy filter is on (Settings > Data): matching items are skipped."
    } else {
        "\n\nEverything is imported as is. To skip sensitive items, choose what to skip in Settings > Data."
    });
    if !run_modal(|mtm| dialogs::confirm("Import from Paste?", &message, "Import", mtm)) {
        return;
    }
    let slot: ImportResult = Arc::new(Mutex::new(None));
    with(|app| app.import_job = Some(slot.clone()));
    std::thread::spawn(move || {
        let result = (|| -> Result<String> {
            let mut store = Store::open_default()?;
            let path = crate::paths::settings_path()?;
            let mut settings = Settings::load(&path)?;
            let opts = ImportOptions { dry_run: false, history: true, settings: true };
            let report = paste_app::import(&install, &mut store, &mut settings, &opts)?;
            settings.save(&path)?;
            Ok(report.summary())
        })()
        .map_err(|e| format!("{e:#}"));
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(result);
        }
    });
}

/// Shows the welcome window once, on first launch. It covers the shortcut,
/// Accessibility permission and, when Paste is installed, importing from it.
fn show_welcome_once() {
    let Ok(marker) = crate::paths::data_dir().map(|d| d.join(".welcomed")) else { return };
    if marker.exists() {
        return;
    }
    let _ = std::fs::write(&marker, b"");
    with(|app| app.open_welcome());
}
