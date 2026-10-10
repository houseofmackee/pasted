use serde::{Deserialize, Serialize};

pub const UTI_PLAIN_TEXT: &str = "public.utf8-plain-text";
pub const UTI_UTF16_PLAIN_TEXT: &str = "public.utf16-external-plain-text";
pub const UTI_RTF: &str = "public.rtf";
pub const UTI_RTFD: &str = "com.apple.flat-rtfd";
pub const UTI_HTML: &str = "public.html";
pub const UTI_URL: &str = "public.url";
pub const UTI_URL_NAME: &str = "public.url-name";
pub const UTI_FILE_URL: &str = "public.file-url";
pub const UTI_PNG: &str = "public.png";
pub const UTI_TIFF: &str = "public.tiff";
pub const UTI_JPEG: &str = "public.jpeg";
pub const UTI_GIF: &str = "com.compuserve.gif";
pub const UTI_HEIC: &str = "public.heic";

pub const IMAGE_UTIS: &[&str] = &[UTI_PNG, UTI_TIFF, UTI_JPEG, UTI_GIF, UTI_HEIC];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Text,
    RichText,
    Link,
    Image,
    File,
    Color,
    Code,
}

impl ItemKind {
    /// Kinds that can be shared with AI tools (images and files are not readable as text).
    pub const ALL_FOR_MCP: [ItemKind; 5] =
        [ItemKind::Text, ItemKind::RichText, ItemKind::Link, ItemKind::Code, ItemKind::Color];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Text => "text",
            ItemKind::RichText => "richtext",
            ItemKind::Link => "link",
            ItemKind::Image => "image",
            ItemKind::File => "file",
            ItemKind::Color => "color",
            ItemKind::Code => "code",
        }
    }

    pub fn parse(s: &str) -> Option<ItemKind> {
        let s = s.to_ascii_lowercase();
        Some(match s.as_str() {
            "text" | "plain" => ItemKind::Text,
            "richtext" | "rich" | "rtf" => ItemKind::RichText,
            "link" | "url" => ItemKind::Link,
            "image" | "img" | "picture" => ItemKind::Image,
            "file" | "files" => ItemKind::File,
            "color" | "colour" => ItemKind::Color,
            "code" => ItemKind::Code,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Text => "Text",
            ItemKind::RichText => "Rich Text",
            ItemKind::Link => "Link",
            ItemKind::Image => "Image",
            ItemKind::File => "File",
            ItemKind::Color => "Color",
            ItemKind::Code => "Code",
        }
    }
}

/// One pasteboard representation. `index` is the pasteboard item it belongs
/// to, since a single copy can contain several items (for example many files).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Representation {
    pub index: u32,
    pub uti: String,
    pub data: Vec<u8>,
}

/// Decodes `public.utf16-external-plain-text`: UTF-16 with a byte order
/// mark, little-endian when there is none.
pub fn decode_utf16_external(data: &[u8]) -> Option<String> {
    let (body, big_endian) = match data {
        [0xFF, 0xFE, rest @ ..] => (rest, false),
        [0xFE, 0xFF, rest @ ..] => (rest, true),
        _ => (data, false),
    };
    let (pairs, rest) = body.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    let units: Vec<u16> =
        pairs.iter().map(|p| if big_endian { u16::from_be_bytes(*p) } else { u16::from_le_bytes(*p) }).collect();
    String::from_utf16(&units).ok()
}

/// macOS adds a UTF-16 copy of the plain text to many copies (rich text, for
/// example). When it says exactly what the UTF-8 text of the same item says,
/// it is redundant: macOS recreates it from the UTF-8 text when pasting.
pub fn is_redundant_utf16(r: &Representation, reps: &[Representation]) -> bool {
    r.uti == UTI_UTF16_PLAIN_TEXT
        && reps.iter().any(|p| {
            p.index == r.index
                && p.uti == UTI_PLAIN_TEXT
                && decode_utf16_external(&r.data).is_some_and(|t| t.as_bytes() == p.data.as_slice())
        })
}

pub fn without_redundant_utf16(reps: Vec<Representation>) -> Vec<Representation> {
    let redundant: Vec<bool> = reps.iter().map(|r| is_redundant_utf16(r, &reps)).collect();
    reps.into_iter().zip(redundant).filter(|(_, drop)| !drop).map(|(r, _)| r).collect()
}

/// Extra details derived from the content, stored as JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SourceApp {
    pub bundle_id: String,
    pub name: Option<String>,
}

/// A captured clip that has not been stored yet.
#[derive(Debug, Clone)]
pub struct NewItem {
    pub kind: ItemKind,
    pub title: Option<String>,
    /// Plain text used for previews and search.
    pub text: String,
    pub meta: ItemMeta,
    pub representations: Vec<Representation>,
    pub source_app: Option<SourceApp>,
    pub device: Option<String>,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Unix milliseconds of the most recent copy. Drives history ordering.
    pub timestamp: i64,
    /// Identifier in a foreign store (for idempotent imports).
    pub external_id: Option<String>,
    pub in_history: bool,
}

/// A stored item without its representations.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: i64,
    pub kind: ItemKind,
    pub title: Option<String>,
    pub text: String,
    pub meta: ItemMeta,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub device: Option<String>,
    pub created_at: i64,
    pub timestamp: i64,
    pub use_count: i64,
    pub size_bytes: i64,
    /// Unix ms when a rule will remove this item from history.
    pub expires_at: Option<i64>,
    /// Concealed or secret-looking content kept briefly; masked on cards and
    /// never shared with AI tools or exported.
    pub sensitive: bool,
}

impl Item {
    /// Single line summary for lists and card headers.
    pub fn summary(&self, max_chars: usize) -> String {
        if self.sensitive {
            return "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022} (sensitive, hidden)".to_string();
        }
        let base = match (&self.title, self.kind) {
            (Some(t), _) if !t.is_empty() => t.clone(),
            (_, ItemKind::Image) => match (self.meta.width, self.meta.height) {
                (Some(w), Some(h)) => format!("Image {w}x{h}"),
                _ => "Image".to_string(),
            },
            (_, ItemKind::File) if !self.meta.files.is_empty() => {
                let names: Vec<_> =
                    self.meta.files.iter().map(|f| f.rsplit('/').next().unwrap_or(f).to_string()).collect();
                names.join(", ")
            }
            _ => self.text.clone(),
        };
        let line: String = base.split_whitespace().collect::<Vec<_>>().join(" ");
        truncate_chars(&line, max_chars)
    }
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct Pinboard {
    pub id: i64,
    pub name: String,
    /// ARGB color.
    pub color: u32,
    pub item_count: i64,
}

pub fn relative_time(ms: i64) -> String {
    let secs = (now_ms() - ms) / 1000;
    match secs {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 86_400 * 30 => format!("{}d ago", s / 86_400),
        _ => chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(index: u32, uti: &str, data: &[u8]) -> Representation {
        Representation { index, uti: uti.into(), data: data.to_vec() }
    }

    fn utf16(text: &str, bom: &[u8], big_endian: bool) -> Vec<u8> {
        let mut out = bom.to_vec();
        for u in text.encode_utf16() {
            out.extend(if big_endian { u.to_be_bytes() } else { u.to_le_bytes() });
        }
        out
    }

    #[test]
    fn decodes_utf16_with_or_without_a_byte_order_mark() {
        assert_eq!(decode_utf16_external(&utf16("Café ✓", &[0xFF, 0xFE], false)).as_deref(), Some("Café ✓"));
        assert_eq!(decode_utf16_external(&utf16("Café ✓", &[0xFE, 0xFF], true)).as_deref(), Some("Café ✓"));
        assert_eq!(decode_utf16_external(&utf16("plain", &[], false)).as_deref(), Some("plain"));
        assert_eq!(decode_utf16_external(&[0xFF, 0xFE, 0x41]), None, "odd length");
    }

    #[test]
    fn only_identical_utf16_text_is_redundant() {
        let reps = vec![
            rep(0, UTI_PLAIN_TEXT, b"same"),
            rep(0, UTI_UTF16_PLAIN_TEXT, &utf16("same", &[0xFF, 0xFE], false)),
            rep(1, UTI_PLAIN_TEXT, b"first"),
            rep(1, UTI_UTF16_PLAIN_TEXT, &utf16("different", &[0xFF, 0xFE], false)),
            rep(2, UTI_UTF16_PLAIN_TEXT, &utf16("only UTF-16", &[0xFF, 0xFE], false)),
        ];
        let kept = without_redundant_utf16(reps.clone());
        assert_eq!(kept.len(), 4);
        assert!(!kept.contains(&reps[1]), "the repeat is dropped");
        assert!(kept.contains(&reps[3]), "different text is kept");
        assert!(kept.contains(&reps[4]), "UTF-16 without UTF-8 is the only text, so it is kept");
    }
}
