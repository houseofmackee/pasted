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
}

impl Item {
    /// Single line summary for lists and card headers.
    pub fn summary(&self, max_chars: usize) -> String {
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
