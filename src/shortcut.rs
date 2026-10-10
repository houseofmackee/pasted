//! Keyboard shortcuts stored as human readable strings like "shift+cmd+v".

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub cmd: bool,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
}

impl Modifiers {
    /// Carbon modifier mask used by `RegisterEventHotKey`.
    pub fn carbon(self) -> u32 {
        (self.cmd as u32) << 8 | (self.shift as u32) << 9 | (self.option as u32) << 11 | (self.control as u32) << 12
    }

    /// From `NSEvent.modifierFlags` bits (also the format Paste stores).
    pub fn from_ns_flags(flags: u64) -> Modifiers {
        Modifiers {
            shift: flags & 0x20000 != 0,
            control: flags & 0x40000 != 0,
            option: flags & 0x80000 != 0,
            cmd: flags & 0x100000 != 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub key_code: u16,
    pub modifiers: Modifiers,
}

/// macOS virtual key codes (ANSI layout).
const KEYS: &[(&str, u16)] = &[
    ("a", 0),
    ("s", 1),
    ("d", 2),
    ("f", 3),
    ("h", 4),
    ("g", 5),
    ("z", 6),
    ("x", 7),
    ("c", 8),
    ("v", 9),
    ("b", 11),
    ("q", 12),
    ("w", 13),
    ("e", 14),
    ("r", 15),
    ("y", 16),
    ("t", 17),
    ("1", 18),
    ("2", 19),
    ("3", 20),
    ("4", 21),
    ("6", 22),
    ("5", 23),
    ("=", 24),
    ("9", 25),
    ("7", 26),
    ("-", 27),
    ("8", 28),
    ("0", 29),
    ("]", 30),
    ("o", 31),
    ("u", 32),
    ("[", 33),
    ("i", 34),
    ("p", 35),
    ("return", 36),
    ("l", 37),
    ("j", 38),
    ("'", 39),
    ("k", 40),
    (";", 41),
    ("\\", 42),
    (",", 43),
    ("/", 44),
    ("n", 45),
    ("m", 46),
    (".", 47),
    ("tab", 48),
    ("space", 49),
    ("`", 50),
    ("delete", 51),
    ("escape", 53),
    ("f5", 96),
    ("f6", 97),
    ("f7", 98),
    ("f3", 99),
    ("f8", 100),
    ("f9", 101),
    ("f11", 103),
    ("f10", 109),
    ("f12", 111),
    ("home", 115),
    ("pageup", 116),
    ("forwarddelete", 117),
    ("f4", 118),
    ("end", 119),
    ("f2", 120),
    ("pagedown", 121),
    ("f1", 122),
    ("left", 123),
    ("right", 124),
    ("down", 125),
    ("up", 126),
];

pub fn key_code(name: &str) -> Option<u16> {
    let name = name.to_ascii_lowercase();
    let name = match name.as_str() {
        "esc" => "escape",
        "enter" => "return",
        "backspace" => "delete",
        other => other,
    };
    KEYS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

pub fn key_name(code: u16) -> Option<&'static str> {
    KEYS.iter().find(|(_, c)| *c == code).map(|(n, _)| *n)
}

impl Shortcut {
    pub fn parse(s: &str) -> Result<Shortcut, String> {
        let mut modifiers = Modifiers::default();
        let mut key = None;
        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "\u{2318}" => modifiers.cmd = true,
                "shift" | "\u{21e7}" => modifiers.shift = true,
                "opt" | "option" | "alt" | "\u{2325}" => modifiers.option = true,
                "ctrl" | "control" | "\u{2303}" => modifiers.control = true,
                other => {
                    if key.is_some() {
                        return Err(format!("shortcut {s:?} has more than one key"));
                    }
                    key = Some(key_code(other).ok_or_else(|| format!("unknown key {other:?}"))?);
                }
            }
        }
        let key_code = key.ok_or_else(|| format!("shortcut {s:?} has no key"))?;
        Ok(Shortcut { key_code, modifiers })
    }

    /// Paste stores shortcuts as JSON: `{"keyCode":9,"internalModifiers":1179648}`.
    pub fn from_paste_json(json: &[u8]) -> Option<Shortcut> {
        let v: serde_json::Value = serde_json::from_slice(json).ok()?;
        let key_code = v.get("keyCode")?.as_u64()? as u16;
        let flags = v.get("internalModifiers").and_then(|m| m.as_u64()).unwrap_or(0);
        Some(Shortcut { key_code, modifiers: Modifiers::from_ns_flags(flags) })
    }

    /// Symbols for menus, for example "⇧⌘V".
    pub fn display(&self) -> String {
        let m = self.modifiers;
        let mut s = String::new();
        if m.control {
            s.push('\u{2303}');
        }
        if m.option {
            s.push('\u{2325}');
        }
        if m.shift {
            s.push('\u{21e7}');
        }
        if m.cmd {
            s.push('\u{2318}');
        }
        let key = key_name(self.key_code).unwrap_or("?");
        match key {
            "left" => s.push('\u{2190}'),
            "right" => s.push('\u{2192}'),
            "up" => s.push('\u{2191}'),
            "down" => s.push('\u{2193}'),
            "space" => s.push_str("Space"),
            "return" => s.push('\u{21a9}'),
            k => s.push_str(&k.to_uppercase()),
        }
        s
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        let mut parts = Vec::new();
        if m.control {
            parts.push("ctrl");
        }
        if m.option {
            parts.push("opt");
        }
        if m.shift {
            parts.push("shift");
        }
        if m.cmd {
            parts.push("cmd");
        }
        parts.push(key_name(self.key_code).unwrap_or("?"));
        f.write_str(&parts.join("+"))
    }
}

impl Serialize for Shortcut {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Shortcut {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Shortcut::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Shortcut::parse("Shift+Cmd+V").unwrap();
        assert_eq!(s.key_code, 9);
        assert!(s.modifiers.cmd && s.modifiers.shift && !s.modifiers.option);
        assert_eq!(s.to_string(), "shift+cmd+v");
        assert_eq!(Shortcut::parse(&s.to_string()).unwrap(), s);
        assert_eq!(s.display(), "\u{21e7}\u{2318}V");
    }

    #[test]
    fn errors() {
        assert!(Shortcut::parse("cmd").is_err());
        assert!(Shortcut::parse("cmd+a+b").is_err());
        assert!(Shortcut::parse("cmd+nope").is_err());
    }

    #[test]
    fn paste_format() {
        let s = Shortcut::from_paste_json(br#"{"keyCode":9,"internalModifiers":1179648}"#).unwrap();
        assert_eq!(s, Shortcut::parse("shift+cmd+v").unwrap());
        assert_eq!(s.modifiers.carbon(), 256 | 512);
    }
}
