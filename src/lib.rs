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
pub mod stack;
pub mod stats_data;
pub mod store;
pub mod transform;
pub mod ui;
