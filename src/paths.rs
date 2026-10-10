use std::path::PathBuf;

use anyhow::{Context, Result};

/// Root directory for all Pasted data. `PASTED_HOME` overrides the default,
/// which keeps tests and experiments away from real history.
pub fn data_dir() -> Result<PathBuf> {
    let dir = match std::env::var_os("PASTED_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::data_dir().context("could not locate the Application Support directory")?.join("Pasted"),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

pub fn db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("pasted.db"))
}

pub fn blobs_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("blobs"))
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("settings.toml"))
}
