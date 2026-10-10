//! The menu bar app: owns all state on the main thread and wires AppKit
//! callbacks to actions.
//!
//! State lives in a thread local and is reached through `with`. AppKit can
//! re-enter our callbacks while a modal loop runs, so state is never borrowed
//! across a modal dialog or menu; see `run_modal`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSButton, NSControl,
    NSControlStateValueOn, NSControlTextEditingDelegate, NSEvent, NSEventMask, NSEventModifierFlags, NSImage, NSMenu,
    NSMenuItem, NSPasteboard, NSPasteboardItem, NSResponder, NSRunningApplication, NSSearchFieldDelegate, NSSound,
    NSStatusBar, NSStatusItem, NSTextFieldDelegate, NSTextView, NSWindowDelegate, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSData, NSDistributedNotificationCenter, NSNotification, NSObject, NSObjectProtocol,
    NSTimer, NSURL,
};

use crate::clipboard::capture::{Capture, capture};
use crate::clipboard::pasteboard;
use crate::clipboard::privacy::PrivacyFilter;
use crate::hotkeys::{HotkeyAction, Hotkeys};
use crate::import::paste_app::{self, ImportOptions};
use crate::model::{IMAGE_UTIS, ItemKind, now_ms};
use crate::settings::Settings;
use crate::store::Store;
use crate::ui::preview::Preview;
use crate::ui::shelf::{self, Shelf, menu_item, tag, untag};
use crate::ui::stack_panel::StackPanel;
use crate::ui::{dialogs, ns};

// Menu action codes (see `shelf::tag`).
const OPEN_SHELF: u8 = 10;
const TOGGLE_PAUSE: u8 = 11;
const IMPORT_PASTE: u8 = 12;
const OPEN_SETTINGS: u8 = 13;
const RELOAD_SETTINGS: u8 = 14;
const ACCESSIBILITY: u8 = 15;
const ABOUT: u8 = 16;
const QUIT: u8 = 17;
const CLEAR_HISTORY: u8 = 18;
const PASTE_STACK: u8 = 19;
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

const POLL_INTERVAL: f64 = 0.3;
/// Posted by `pasted open` to show the shelf of the running app.
pub const OPEN_NOTIFICATION: &str = "io.pasted.app.open";
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

        #[unsafe(method(remoteOpen:))]
        fn remote_open(&self, _notification: &NSNotification) {
            with(|app| {
                if !app.shelf.is_visible() {
                    app.open_shelf();
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

        #[unsafe(method(postPaste:))]
        fn post_paste(&self, _obj: Option<&AnyObject>) {
            crate::paste::post_cmd_v();
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
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

type ImportResult = Arc<Mutex<Option<Result<String, String>>>>;

/// Paste Stack: copies are queued and each Cmd+V pastes the next one.
struct PasteStack {
    queue: VecDeque<i64>,
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
    hotkeys: Hotkeys,
    shelf: Shelf,
    preview: Preview,
    /// App that was frontmost when the shelf opened; receives the paste.
    previous_app: Option<Retained<NSRunningApplication>>,
    modal: bool,
    asked_accessibility: bool,
    last_maintenance: Instant,
    import_job: Option<ImportResult>,
    stack: Option<PasteStack>,
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
        stack: None,
        store,
        settings,
        status_item,
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
    });

    if let Some(e) = settings_error {
        dialogs::info("Settings file has an error", &format!("Using defaults until it is fixed.\n\n{e}"), mtm);
    }
    crate::ocr::spawn_worker();
    offer_first_run_import();
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
    let edit_item = NSMenuItem::new(mtm);
    edit_item.setSubmenu(Some(&edit));
    main.addItem(&edit_item);
    NSApplication::sharedApplication(mtm).setMainMenu(Some(&main));
}

impl App {
    fn apply_settings(&mut self) {
        self.filter = PrivacyFilter::new(&self.settings.privacy);
        self.shelf.set_visible_in_screen_sharing(self.settings.privacy.visible_during_screen_sharing);
        let s = &self.settings.shortcuts;
        let failed =
            self.hotkeys.set(&[(HotkeyAction::OpenShelf, s.open_shelf), (HotkeyAction::PasteStack, s.paste_stack)]);
        for f in failed {
            tracing::warn!("shortcut {f} is unavailable");
        }
        if let Some(problem) = crate::login_item::sync(self.settings.general.launch_at_login) {
            tracing::warn!("{problem}");
        }
    }

    fn rebuild_status_menu(&self) {
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
        add("Clear History\u{2026}", CLEAR_HISTORY);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("Open Settings File", OPEN_SETTINGS);
        add("Reload Settings", RELOAD_SETTINGS);
        if !crate::paste::is_trusted() {
            add("Grant Accessibility Permission\u{2026}", ACCESSIBILITY);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("About Pasted", ABOUT);
        add("Quit Pasted", QUIT);
        self.status_item.setMenu(Some(&menu));
    }

    fn poll_clipboard(&mut self) {
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
                    self.push_stack(inserted.id());
                    if self.settings.general.sounds
                        && let Some(s) = NSSound::soundNamed(&ns("Tink"))
                    {
                        s.play();
                    }
                    if self.shelf.is_visible() && self.shelf.board.is_none() {
                        self.shelf.reload(&self.store);
                    }
                }
                Err(e) => tracing::error!("saving clip failed: {e:#}"),
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
        let stack = PasteStack { queue: VecDeque::new(), monitor, panel: StackPanel::new(self.mtm) };
        stack.panel.show(&[]);
        self.stack = Some(stack);
        self.rebuild_status_menu();
    }

    fn stack_items(&self) -> Vec<crate::model::Item> {
        let Some(stack) = &self.stack else { return Vec::new() };
        stack.queue.iter().filter_map(|id| self.store.get(*id).ok().flatten()).collect()
    }

    fn push_stack(&mut self, id: i64) {
        let Some(stack) = &mut self.stack else { return };
        stack.queue.push_back(id);
        // The pasteboard must hold the head of the queue, not the latest copy.
        let head = stack.queue.front().copied();
        if stack.queue.len() > 1
            && let Some(head) = head
        {
            self.write_item(head, false);
        }
        let items = self.stack_items();
        if let Some(stack) = &self.stack {
            stack.panel.show(&items);
        }
    }

    fn advance_stack(&mut self) {
        let Some(stack) = &mut self.stack else { return };
        stack.queue.pop_front();
        match stack.queue.front().copied() {
            Some(next) => {
                self.write_item(next, false);
                let items = self.stack_items();
                if let Some(stack) = &self.stack {
                    stack.panel.show(&items);
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
        self.close_shelf();
        if !self.write_item(id, plain) || !self.settings.general.paste_directly {
            return;
        }
        if !crate::paste::is_trusted() {
            if !self.asked_accessibility {
                self.asked_accessibility = true;
                crate::paste::request_trust();
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

    fn copy(&mut self, id: i64) {
        self.close_shelf();
        self.write_item(id, false);
    }

    fn delete(&mut self, id: i64) {
        let result = match self.shelf.board {
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
        self.preview.show(&item, image, self.shelf.panel.frame(), self.mtm);
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
        Some(done)
    }
}

fn tick() {
    let finished = with(|app| {
        app.poll_clipboard();
        app.maintenance();
        app.poll_import()
    })
    .flatten();
    if let Some(result) = finished {
        let mtm = mtm();
        match result {
            Ok(summary) => dialogs::info("Import from Paste finished", &summary, mtm),
            Err(e) => dialogs::info("Import from Paste failed", &e, mtm),
        }
    }
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
        "insertNewline:" | "insertNewlineIgnoringFieldEditor:" => {
            let plain = shift || name == "insertNewlineIgnoringFieldEditor:";
            if let Some(id) = app.selected_id() {
                app.paste(id, plain);
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
            "," => {
                app.close_shelf();
                open_settings_file();
                Some(After::Nothing)
            }
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

        let pins = NSMenu::new(mtm);
        let on = app.store.item_pinboards(item.id).unwrap_or_default();
        for b in &app.shelf.boards {
            let it = add(&pins, &b.name, ITEM_PIN, b.id);
            if on.contains(&b.id) {
                it.setState(NSControlStateValueOn);
            }
        }
        if !app.shelf.boards.is_empty() {
            pins.addItem(&NSMenuItem::separatorItem(mtm));
        }
        add(&pins, "New Pinboard\u{2026}", ITEM_PIN_NEW, item.id);
        let pin_item = menu_item("Pin to", 0, t, mtm);
        pin_item.setSubmenu(Some(&pins));
        menu.addItem(&pin_item);
        if app.shelf.board.is_some() {
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
        CLEAR_HISTORY => clear_history(),
        OPEN_SETTINGS => open_settings_file(),
        RELOAD_SETTINGS => reload_settings(),
        ACCESSIBILITY => {
            crate::paste::request_trust();
        }
        ABOUT => about(),
        QUIT => NSApplication::sharedApplication(mtm()).terminate(None),
        ITEM_PASTE | ITEM_PASTE_PLAIN => {
            with(|app| app.paste(arg, code == ITEM_PASTE_PLAIN));
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
        shelf::TAB_NEW => new_pinboard(None),
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
            let Some(name) =
                with(|app| app.shelf.boards.iter().find(|b| b.id == arg).map(|b| b.name.clone())).flatten()
            else {
                return;
            };
            let ok = run_modal(|mtm| {
                dialogs::confirm(
                    &format!("Delete the pinboard \u{201c}{name}\u{201d}?"),
                    "Items that are only on this pinboard will be deleted.",
                    "Delete",
                    mtm,
                )
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

fn clear_history() {
    let ok = run_modal(|mtm| {
        dialogs::confirm(
            "Clear clipboard history?",
            "All history items will be deleted. Pinboards are kept.",
            "Clear History",
            mtm,
        )
    });
    if ok {
        with(|app| {
            if let Err(e) = app.store.clear_history() {
                tracing::error!("clear history failed: {e:#}");
            }
            let _ = app.store.collect_garbage();
        });
    }
}

fn open_settings_file() {
    if let Ok(path) = crate::paths::settings_path() {
        if !path.exists() {
            let _ = Settings::default().save(&path);
        }
        let _ = std::process::Command::new("open").arg("-t").arg(&path).spawn();
    }
}

fn reload_settings() {
    let result = crate::paths::settings_path().and_then(|p| Settings::load(&p));
    match result {
        Ok(settings) => {
            with(|app| {
                app.settings = settings;
                app.apply_settings();
                app.rebuild_status_menu();
            });
        }
        Err(e) => run_modal(|mtm| dialogs::info("Settings file has an error", &format!("{e:#}"), mtm)),
    }
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

/// On first launch with an empty history, offer to bring over Paste's data.
fn offer_first_run_import() {
    let empty = with(|app| app.store.stats().map(|s| s.items == 0).unwrap_or(false)).unwrap_or(false);
    let marker = crate::paths::data_dir().map(|d| d.join(".import-offered"));
    let Ok(marker) = marker else { return };
    if !empty || marker.exists() || paste_app::detect().is_empty() {
        return;
    }
    let _ = std::fs::write(&marker, b"");
    import_from_paste();
}
