//! Updates through Sparkle. `scripts/bundle.sh` embeds Sparkle.framework;
//! it is loaded at runtime, so builds without it (`cargo run`, tests) work
//! the same, minus Check for Updates….
//!
//! The feed is the latest regular release's `appcast.xml`: pre-releases are
//! never offered. Sparkle asks on the second launch whether to check
//! automatically, and Settings > General can change that later. As a menu
//! bar app, Pasted uses Sparkle's gentle reminders: an update found by a
//! scheduled check does not steal focus; the menu offers it instead.

use std::cell::RefCell;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSBundle, NSString};

struct Updater {
    /// The SPUStandardUpdaterController.
    controller: Retained<AnyObject>,
    /// Kept alive: Sparkle holds its delegates weakly.
    _delegate: Retained<Delegate>,
}

thread_local! {
    static UPDATER: RefCell<Option<Updater>> = const { RefCell::new(None) };
    /// A version a scheduled check found, until the user looks at it.
    static FOUND: RefCell<Option<String>> = const { RefCell::new(None) };
}

define_class!(
    /// Sparkle's updater and user driver delegate. Sparkle asks with
    /// `respondsToSelector:`, so only the methods used are defined.
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct Delegate;

    impl Delegate {
        /// Debug builds, or with `PASTED_UPDATE_TEST`: `PASTED_FEED_URL`
        /// points Sparkle at a test feed (it must still be signed with the
        /// release key).
        #[unsafe(method(feedURLStringForUpdater:))]
        fn feed_url(&self, _updater: &AnyObject) -> *mut NSString {
            if !cfg!(debug_assertions) && std::env::var_os("PASTED_UPDATE_TEST").is_none() {
                return std::ptr::null_mut();
            }
            std::env::var("PASTED_FEED_URL")
                .ok()
                .filter(|url| !url.is_empty())
                .map_or(std::ptr::null_mut(), |url| {
                    Retained::autorelease_return(NSString::from_str(&url))
                })
        }

        #[unsafe(method(supportsGentleScheduledUpdateReminders))]
        fn supports_gentle_reminders(&self) -> Bool {
            Bool::YES
        }

        #[unsafe(method(standardUserDriverWillHandleShowingUpdate:forUpdate:state:))]
        fn will_show_update(&self, _handle: Bool, update: &AnyObject, state: &AnyObject) {
            // SAFETY: an SPUUserUpdateState and an SUAppcastItem.
            let user_initiated: Bool = unsafe { msg_send![state, userInitiated] };
            if user_initiated.as_bool() {
                return;
            }
            // SAFETY: SUAppcastItem's displayVersionString is an NSString.
            let version: Option<Retained<NSString>> =
                unsafe { msg_send![update, displayVersionString] };
            let version = version.map(|v| v.to_string()).unwrap_or_default();
            tracing::info!(%version, "update available");
            FOUND.with(|f| *f.borrow_mut() = Some(version));
        }

        #[unsafe(method(standardUserDriverDidReceiveUserAttentionForUpdate:))]
        fn did_get_attention(&self, _update: &AnyObject) {
            FOUND.with(|f| f.borrow_mut().take());
        }

        #[unsafe(method(standardUserDriverWillFinishUpdateSession))]
        fn will_finish_session(&self) {
            FOUND.with(|f| f.borrow_mut().take());
        }
    }
);

/// Loads Sparkle from the bundle and starts the updater, if the bundle has
/// it.
pub fn start(mtm: MainThreadMarker) {
    let Some(frameworks) = NSBundle::mainBundle().privateFrameworksPath() else {
        return;
    };
    let path = NSString::from_str(&format!("{frameworks}/Sparkle.framework"));
    let Some(bundle) = NSBundle::bundleWithPath(&path) else {
        return;
    };
    // SAFETY: loading Sparkle, signed with the app's own identity, from
    // the app's Frameworks folder.
    if !unsafe { bundle.load() } {
        tracing::warn!("Sparkle.framework is in the bundle but did not load");
        return;
    }
    let Some(class) = AnyClass::get(c"SPUStandardUpdaterController") else {
        tracing::warn!("Sparkle loaded without SPUStandardUpdaterController");
        return;
    };
    let delegate: Retained<Delegate> = {
        let this = Delegate::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    };
    // SAFETY: Sparkle's documented initializer; the delegates conform to
    // the protocols' optional methods used.
    let controller: Option<Retained<AnyObject>> = unsafe {
        let allocated: Allocated<AnyObject> = msg_send![class, alloc];
        msg_send![
            allocated,
            initWithStartingUpdater: Bool::YES,
            updaterDelegate: &*delegate,
            userDriverDelegate: &*delegate
        ]
    };
    let Some(controller) = controller else {
        tracing::warn!("the Sparkle updater did not start");
        return;
    };
    tracing::info!("updates through Sparkle");
    UPDATER.with(|u| *u.borrow_mut() = Some(Updater { controller, _delegate: delegate }));
    // Update tests: check right away, as a scheduled check would
    // (`PASTED_UPDATE_TEST=ui`: as Check for Updates… would, showing the result).
    match std::env::var("PASTED_UPDATE_TEST").as_deref() {
        Ok("ui") => check(mtm),
        Ok(_) => {
            if let Some(updater) = updater_object() {
                // SAFETY: SPUUpdater's checkForUpdatesInBackground.
                unsafe {
                    let _: () = msg_send![&*updater, checkForUpdatesInBackground];
                }
            }
        }
        Err(_) => {}
    }
}

/// Whether this build can update itself.
pub fn available() -> bool {
    UPDATER.with(|u| u.borrow().is_some())
}

/// The version a scheduled check found and the user has not seen yet.
pub fn found() -> Option<String> {
    FOUND.with(|f| f.borrow().clone())
}

/// Check for Updates…: shows Sparkle's window with the result.
pub fn check(mtm: MainThreadMarker) {
    UPDATER.with(|u| {
        if let Some(updater) = u.borrow().as_ref() {
            NSApplication::sharedApplication(mtm).activate();
            // SAFETY: SPUStandardUpdaterController's checkForUpdates: action.
            unsafe {
                let _: () = msg_send![&*updater.controller, checkForUpdates: None::<&AnyObject>];
            }
        }
    });
}

fn updater_object() -> Option<Retained<AnyObject>> {
    UPDATER.with(|u| {
        u.borrow().as_ref().and_then(|updater| {
            // SAFETY: SPUStandardUpdaterController's updater property.
            unsafe { msg_send![&*updater.controller, updater] }
        })
    })
}

/// Whether Sparkle checks for updates on its own.
pub fn checks_automatically() -> bool {
    updater_object().is_some_and(|updater| {
        // SAFETY: SPUUpdater's automaticallyChecksForUpdates property.
        let on: Bool = unsafe { msg_send![&*updater, automaticallyChecksForUpdates] };
        on.as_bool()
    })
}

pub fn set_checks_automatically(on: bool) {
    if let Some(updater) = updater_object() {
        // SAFETY: SPUUpdater's automaticallyChecksForUpdates setter.
        unsafe {
            let _: () = msg_send![&*updater, setAutomaticallyChecksForUpdates: Bool::new(on)];
        }
    }
}
