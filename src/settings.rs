use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::shortcut::Shortcut;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Retention {
    Day,
    Week,
    Month,
    Year,
    Forever,
}

impl Retention {
    pub fn max_age_ms(self) -> Option<i64> {
        const DAY: i64 = 24 * 60 * 60 * 1000;
        match self {
            Retention::Day => Some(DAY),
            Retention::Week => Some(7 * DAY),
            Retention::Month => Some(31 * DAY),
            Retention::Year => Some(366 * DAY),
            Retention::Forever => None,
        }
    }

    /// Paste stores history capacity as a number of days.
    pub fn from_days(days: i64) -> Retention {
        match days {
            d if d <= 0 => Retention::Forever,
            1 => Retention::Day,
            2..=7 => Retention::Week,
            8..=31 => Retention::Month,
            32..=366 => Retention::Year,
            _ => Retention::Forever,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub history_retention: Retention,
    /// Skip single copies larger than this.
    pub max_item_size_mb: u64,
    pub paste_plain_text_by_default: bool,
    /// Simulate Cmd+V after choosing an item (needs Accessibility permission).
    pub paste_directly: bool,
    pub sounds: bool,
    pub launch_at_login: bool,
    /// Fetch page titles and images for copied links (the only network access).
    pub link_previews: bool,
}

impl Default for General {
    fn default() -> Self {
        General {
            history_retention: Retention::Month,
            max_item_size_mb: 64,
            paste_plain_text_by_default: false,
            paste_directly: true,
            sounds: false,
            launch_at_login: false,
            link_previews: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    pub open_shelf: Shortcut,
    pub paste_stack: Shortcut,
    pub next_pinboard: Shortcut,
    pub previous_pinboard: Shortcut,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Shortcuts {
            open_shelf: Shortcut::parse("shift+cmd+v").unwrap(),
            paste_stack: Shortcut::parse("shift+cmd+c").unwrap(),
            next_pinboard: Shortcut::parse("cmd+right").unwrap(),
            previous_pinboard: Shortcut::parse("cmd+left").unwrap(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Privacy {
    /// Honor the nspasteboard.org concealed / transient / auto-generated markers.
    pub ignore_concealed: bool,
    /// Skip text that looks like an API key, private key, JWT or card number.
    pub ignore_secrets: bool,
    /// Bundle identifiers whose copies are never saved.
    pub excluded_apps: Vec<String>,
    /// Extra regular expressions; matching text is not saved.
    pub ignore_patterns: Vec<String>,
    /// Show clip contents while the screen is being shared or recorded.
    pub visible_during_screen_sharing: bool,
}

pub const DEFAULT_EXCLUDED_APPS: &[&str] = &[
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "com.bitwarden.desktop",
    "com.apple.keychainaccess",
    "com.apple.Passwords",
    "org.keepassxc.keepassxc",
    "com.lastpass.LastPass",
    "com.dashlane.dashlanephonefinal",
    "in.sinew.Enpass-Desktop",
];

impl Default for Privacy {
    fn default() -> Self {
        Privacy {
            ignore_concealed: true,
            ignore_secrets: true,
            excluded_apps: DEFAULT_EXCLUDED_APPS.iter().map(|s| s.to_string()).collect(),
            ignore_patterns: Vec::new(),
            visible_during_screen_sharing: false,
        }
    }
}

/// Options applied when pasting. The automatic cleanups are off by default
/// so pastes match what was copied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pasting {
    pub trim_whitespace: bool,
    pub clean_links: bool,
    /// Expand `{date}`, `{cursor}`, `{input:Name}` and other placeholders in
    /// pinboard items. History items are never expanded.
    pub expand_placeholders: bool,
}

impl Default for Pasting {
    fn default() -> Self {
        Pasting { trim_whitespace: false, clean_links: false, expand_placeholders: true }
    }
}

impl Pasting {
    /// Applies the enabled cleanups to `text`.
    pub fn clean(&self, text: &str) -> String {
        let mut out = text.to_string();
        if self.clean_links {
            out = crate::transform::clean_links(&out);
        }
        if self.trim_whitespace {
            out = out.trim().to_string();
        }
        out
    }
}

/// Optional privacy checks applied to items brought in by an import. Each is
/// off by default, so an import copies history as it is unless asked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportFilter {
    /// Skip items copied from apps in `privacy.excluded_apps`.
    pub skip_excluded_apps: bool,
    /// Skip text that looks like an API key, token, private key or card number.
    pub skip_secrets: bool,
    /// Skip text matching `privacy.ignore_patterns`.
    pub skip_ignore_patterns: bool,
    /// Skip short text that mentions a password.
    pub skip_password_mentions: bool,
}

impl ImportFilter {
    pub fn any(&self) -> bool {
        self.skip_excluded_apps || self.skip_secrets || self.skip_ignore_patterns || self.skip_password_mentions
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub general: General,
    pub shortcuts: Shortcuts,
    pub privacy: Privacy,
    pub pasting: Pasting,
    pub import: ImportFilter,
}

impl Settings {
    /// Loads settings, writing defaults if the file does not exist yet.
    pub fn load(path: &Path) -> Result<Settings> {
        if !path.exists() {
            let settings = Settings::default();
            settings.save(path)?;
            return Ok(settings);
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self)?;
        let text = format!(
            "# Pasted settings. Edit and choose \"Reload Settings\" from the menu bar icon.\n\
             # Shortcuts use names like \"shift+cmd+v\"; retention is day, week, month, year or forever.\n\n{body}"
        );
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let s = Settings::load(&path).unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(Settings::load(&path).unwrap(), s);
    }

    #[test]
    fn partial_file_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "[general]\nhistory_retention = \"forever\"\n[shortcuts]\nopen_shelf = \"opt+space\"\n")
            .unwrap();
        let s = Settings::load(&path).unwrap();
        assert_eq!(s.general.history_retention, Retention::Forever);
        assert_eq!(s.shortcuts.open_shelf.to_string(), "opt+space");
        assert!(s.privacy.ignore_concealed);
    }

    #[test]
    fn retention_days() {
        assert_eq!(Retention::from_days(365), Retention::Year);
        assert_eq!(Retention::from_days(31), Retention::Month);
        assert_eq!(Retention::from_days(0), Retention::Forever);
    }
}
