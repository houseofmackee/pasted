//! Pasted: a clipboard manager for macOS. The binary in `main.rs` is a thin
//! wrapper; the library exists so benchmarks and UI tests can reach the code.

#[cfg(not(target_os = "macos"))]
compile_error!("pasted only supports macOS");

pub mod app;
pub mod cli;
pub mod clipboard;
pub mod encryption;
pub mod export;
pub mod fuzzy;
pub mod hotkeys;
pub mod import;
pub mod link_preview;
pub mod login_item;
pub mod mcp;
pub mod model;
pub mod ocr;
pub mod paste;
pub mod paths;
pub mod rich;
pub mod rules;
pub mod search;
pub mod settings;
pub mod shortcut;
pub mod snippet;
pub mod sounds;
pub mod stack;
pub mod stats_data;
pub mod store;
pub mod transform;
pub mod ui;
pub mod updates;

/// Shared by unit tests that use AppKit objects (pasteboards, sounds), so
/// they run one at a time: run in parallel, they occasionally crashed the
/// test process on CI.
#[cfg(test)]
pub(crate) fn appkit_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
