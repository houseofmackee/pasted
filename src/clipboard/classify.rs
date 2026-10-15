//! Turns raw pasteboard representations into a typed item with searchable text.

use std::sync::LazyLock;

use regex::Regex;

use crate::model::*;

pub struct Classified {
    pub kind: ItemKind,
    pub text: String,
    pub meta: ItemMeta,
}

static COLOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)^(
            \#(?:[0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})
          | (?:rgb|rgba|hsl|hsla)\(\s*[0-9.%]+\s*,?\s*[0-9.%]+\s*,?\s*[0-9.%]+\s*(?:[,/]\s*[0-9.%]+\s*)?\)
        )$",
    )
    .unwrap()
});

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:https?://|ftp://|mailto:|www\.)[^\s]+$").unwrap());

/// Text of the first pasteboard item, preferring UTF-8.
pub fn plain_text(reps: &[Representation]) -> Option<String> {
    let first = reps.iter().map(|r| r.index).min()?;
    let find = |uti: &str| reps.iter().find(|r| r.index == first && r.uti == uti);
    if let Some(r) = find(UTI_PLAIN_TEXT) {
        return Some(String::from_utf8_lossy(&r.data).into_owned());
    }
    if let Some(r) = find(UTI_UTF16_PLAIN_TEXT) {
        let units: Vec<u16> = r.data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let s = String::from_utf16_lossy(&units);
        return Some(s.trim_start_matches('\u{feff}').to_string());
    }
    None
}

fn has(reps: &[Representation], uti: &str) -> bool {
    reps.iter().any(|r| r.uti == uti)
}

fn get<'a>(reps: &'a [Representation], uti: &str) -> Option<&'a Representation> {
    reps.iter().find(|r| r.uti == uti)
}

pub fn file_urls(reps: &[Representation]) -> Vec<String> {
    reps.iter()
        .filter(|r| r.uti == UTI_FILE_URL)
        .map(|r| {
            let url = String::from_utf8_lossy(&r.data).trim_end_matches('\0').to_string();
            file_url_to_path(&url).unwrap_or(url)
        })
        .collect()
}

pub fn file_url_to_path(url: &str) -> Option<String> {
    if url.starts_with("file:///.file/id=")
        && let Some(path) = resolve_file_reference(url)
    {
        return Some(path);
    }
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    Some(percent_decode(rest))
}

/// The real path of a file reference URL (`file:///.file/id=...`, which
/// Finder and some apps copy), while the file still exists.
fn resolve_file_reference(url: &str) -> Option<String> {
    use objc2_foundation::{NSString, NSURL};
    let url = NSURL::URLWithString(&NSString::from_str(url))?;
    Some(url.filePathURL()?.path()?.to_string())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| ())
                .and_then(|h| u8::from_str_radix(h, 16).map_err(|_| ()))
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Width and height from a PNG or JPEG header, without decoding the image.
pub fn image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() >= 24 && data.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes(data[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(data[20..24].try_into().ok()?);
        return Some((w, h));
    }
    if data.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 9 < data.len() {
            if data[i] != 0xff {
                return None;
            }
            let marker = data[i + 1];
            let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
                let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

pub fn is_color(text: &str) -> bool {
    COLOR_RE.is_match(text.trim())
}

/// Parses `#rgb[a]`, `#rrggbb[aa]`, `rgb[a](..)` and `hsl[a](..)` into sRGB components in 0..=1.
pub fn parse_color(text: &str) -> Option<[f64; 4]> {
    let t = text.trim().to_ascii_lowercase();
    if let Some(hex) = t.strip_prefix('#') {
        let digits: Vec<u8> = hex.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
        let (rgb, a) = match digits.len() {
            3 | 4 => ([digits[0] * 17, digits[1] * 17, digits[2] * 17], digits.get(3).map(|d| d * 17).unwrap_or(255)),
            6 | 8 => {
                let byte = |i: usize| digits[i] * 16 + digits[i + 1];
                ([byte(0), byte(2), byte(4)], if digits.len() == 8 { byte(6) } else { 255 })
            }
            _ => return None,
        };
        return Some([rgb[0] as f64 / 255.0, rgb[1] as f64 / 255.0, rgb[2] as f64 / 255.0, a as f64 / 255.0]);
    }
    let open = t.find('(')?;
    let func = &t[..open];
    let args: Vec<&str> = t[open + 1..t.rfind(')')?]
        .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect();
    if args.len() < 3 {
        return None;
    }
    let num = |s: &str, scale: f64| -> Option<f64> {
        match s.strip_suffix('%') {
            Some(p) => Some(p.parse::<f64>().ok()? / 100.0),
            None => Some(s.parse::<f64>().ok()? / scale),
        }
    };
    let alpha = args.get(3).map(|a| num(a, 1.0)).unwrap_or(Some(1.0))?;
    match func {
        "rgb" | "rgba" => Some([num(args[0], 255.0)?, num(args[1], 255.0)?, num(args[2], 255.0)?, alpha]),
        "hsl" | "hsla" => {
            let h = args[0].trim_end_matches("deg").parse::<f64>().ok()? / 360.0;
            let s = num(args[1], 100.0)?;
            let l = num(args[2], 100.0)?;
            let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
            let p = 2.0 * l - q;
            let hue = |mut t: f64| {
                t = t.rem_euclid(1.0);
                if t < 1.0 / 6.0 {
                    p + (q - p) * 6.0 * t
                } else if t < 0.5 {
                    q
                } else if t < 2.0 / 3.0 {
                    p + (q - p) * (2.0 / 3.0 - t) * 6.0
                } else {
                    p
                }
            };
            Some([hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0), alpha])
        }
        _ => None,
    }
    .map(|c| c.map(|v| v.clamp(0.0, 1.0)))
}

pub fn is_url(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty() && t.len() < 4096 && URL_RE.is_match(t)
}

/// Cheap heuristic: several signals typical of source code.
pub fn detect_code(text: &str) -> Option<&'static str> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return None;
    }
    let has = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
    let language = if has(&["fn ", "let mut ", "impl ", "pub struct ", "use std::"]) {
        Some("rust")
    } else if has(&["def ", "import ", "self.", "elif "]) && text.contains(':') {
        Some("python")
    } else if has(&["function ", "const ", "=> {", "console.log", "export "]) {
        Some("javascript")
    } else if has(&["func ", "package main", ":= "]) {
        Some("go")
    } else if has(&["#!/bin/", "fi\n", "done\n", "echo $"]) {
        Some("shell")
    } else if has(&["SELECT ", "INSERT INTO", "CREATE TABLE", "WHERE "]) {
        Some("sql")
    } else if text.trim_start().starts_with('{') && serde_json::from_str::<serde_json::Value>(text).is_ok() {
        Some("json")
    } else {
        None
    };
    if language.is_some() {
        return language;
    }
    let symbolic = lines
        .iter()
        .filter(|l| {
            let t = l.trim_end();
            t.ends_with(';') || t.ends_with('{') || t.ends_with('}') || t.starts_with("    ")
        })
        .count();
    (symbolic * 2 > lines.len() && lines.len() >= 3).then_some("code")
}

pub fn classify(reps: &[Representation]) -> Option<Classified> {
    if reps.is_empty() {
        return None;
    }
    let mut meta = ItemMeta::default();

    let image = IMAGE_UTIS.iter().find_map(|u| get(reps, u));
    let files = file_urls(reps);
    if !files.is_empty() {
        let text = files.join("\n");
        meta.files = files;
        // Screenshot tools and browsers copy an image as its file plus the
        // image data: show it as an image, keep the file for pasting.
        if let Some(image) = image {
            if let Some((w, h)) = image_dimensions(&image.data) {
                meta.width = Some(w);
                meta.height = Some(h);
            }
            return Some(Classified { kind: ItemKind::Image, text, meta });
        }
        return Some(Classified { kind: ItemKind::File, text, meta });
    }

    let text = plain_text(reps);

    match text {
        Some(text) if !text.trim().is_empty() => {
            let trimmed = text.trim();
            if has(reps, UTI_URL) || is_url(trimmed) {
                let url = get(reps, UTI_URL)
                    .map(|r| String::from_utf8_lossy(&r.data).trim_end_matches('\0').to_string())
                    .unwrap_or_else(|| trimmed.to_string());
                if !url.starts_with("file://") && is_url(&url) {
                    meta.url = Some(url);
                    meta.url_name = get(reps, UTI_URL_NAME)
                        .map(|r| String::from_utf8_lossy(&r.data).trim_end_matches('\0').to_string());
                    return Some(Classified { kind: ItemKind::Link, text, meta });
                }
            }
            if is_color(trimmed) {
                meta.color = Some(trimmed.to_string());
                return Some(Classified { kind: ItemKind::Color, text, meta });
            }
            if let Some(lang) = detect_code(&text) {
                meta.language = Some(lang.to_string());
                return Some(Classified { kind: ItemKind::Code, text, meta });
            }
            let rich = has(reps, UTI_RTF) || has(reps, UTI_RTFD) || has(reps, UTI_HTML);
            let kind = if rich { ItemKind::RichText } else { ItemKind::Text };
            Some(Classified { kind, text, meta })
        }
        _ => {
            let image = image?;
            if let Some((w, h)) = image_dimensions(&image.data) {
                meta.width = Some(w);
                meta.height = Some(h);
            }
            Some(Classified { kind: ItemKind::Image, text: String::new(), meta })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(uti: &str, data: &[u8]) -> Representation {
        Representation { index: 0, uti: uti.into(), data: data.to_vec() }
    }

    #[test]
    fn plain_text_is_text() {
        let c = classify(&[rep(UTI_PLAIN_TEXT, b"hello world")]).unwrap();
        assert_eq!(c.kind, ItemKind::Text);
        assert_eq!(c.text, "hello world");
    }

    #[test]
    fn html_makes_rich_text() {
        let c = classify(&[rep(UTI_PLAIN_TEXT, b"hi"), rep(UTI_HTML, b"<b>hi</b>")]).unwrap();
        assert_eq!(c.kind, ItemKind::RichText);
    }

    #[test]
    fn url_detection() {
        let c = classify(&[rep(UTI_PLAIN_TEXT, b"  https://example.com/a?b=c ")]).unwrap();
        assert_eq!(c.kind, ItemKind::Link);
        assert_eq!(c.meta.url.as_deref(), Some("https://example.com/a?b=c"));
        let c = classify(&[rep(UTI_PLAIN_TEXT, b"see https://example.com")]).unwrap();
        assert_eq!(c.kind, ItemKind::Text);
    }

    #[test]
    fn colors() {
        for s in ["#fff", "#A1B2C3", "#a1b2c3ff", "rgb(1, 2, 3)", "rgba(1,2,3,0.5)", "hsl(120 50% 50%)"] {
            assert!(is_color(s), "{s}");
        }
        for s in ["#ggg", "#12345", "fff", "rgb()"] {
            assert!(!is_color(s), "{s}");
        }
    }

    #[test]
    fn color_parsing() {
        assert_eq!(parse_color("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color("#0f08"), Some([0.0, 1.0, 0.0, 136.0 / 255.0]));
        assert_eq!(parse_color("rgba(0, 0, 255, 0.5)"), Some([0.0, 0.0, 1.0, 0.5]));
        let hsl = parse_color("hsl(120 100% 50%)").unwrap();
        assert!((hsl[1] - 1.0).abs() < 1e-9 && hsl[0].abs() < 1e-9);
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn code_detection() {
        assert_eq!(detect_code("fn main() {\n    println!(\"hi\");\n}"), Some("rust"));
        assert_eq!(detect_code("{\n  \"a\": 1\n}"), Some("json"));
        assert_eq!(detect_code("Dear Bob,\nThanks for the note.\nCheers"), None);
        assert_eq!(detect_code("one line;"), None);
    }

    #[test]
    fn files() {
        let c = classify(&[
            Representation { index: 0, uti: UTI_FILE_URL.into(), data: b"file:///Users/me/My%20File.txt".to_vec() },
            Representation { index: 1, uti: UTI_FILE_URL.into(), data: b"file:///tmp/b.png".to_vec() },
            rep(UTI_PLAIN_TEXT, b"My File.txt"),
        ])
        .unwrap();
        assert_eq!(c.kind, ItemKind::File);
        assert_eq!(c.meta.files, vec!["/Users/me/My File.txt", "/tmp/b.png"]);
    }

    #[test]
    fn image_file_is_an_image() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&800u32.to_be_bytes());
        png.extend_from_slice(&600u32.to_be_bytes());
        let c = classify(&[
            Representation { index: 0, uti: UTI_FILE_URL.into(), data: b"file:///tmp/Shot%201.png".to_vec() },
            rep(UTI_PNG, &png),
        ])
        .unwrap();
        assert_eq!(c.kind, ItemKind::Image);
        assert_eq!((c.meta.width, c.meta.height), (Some(800), Some(600)));
        assert_eq!(c.meta.files, vec!["/tmp/Shot 1.png"]);
        assert_eq!(c.text, "/tmp/Shot 1.png");
    }

    #[test]
    fn file_reference_urls_resolve() {
        use objc2_foundation::{NSString, NSURL};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("note.txt");
        std::fs::write(&path, "hi").unwrap();
        let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().unwrap()));
        let reference = url.fileReferenceURL().unwrap().absoluteString().unwrap().to_string();
        assert!(reference.starts_with("file:///.file/id="), "{reference}");
        assert_eq!(file_url_to_path(&reference).as_deref(), path.to_str());
    }

    #[test]
    fn image_only() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        let c = classify(&[rep(UTI_PNG, &png)]).unwrap();
        assert_eq!(c.kind, ItemKind::Image);
        assert_eq!((c.meta.width, c.meta.height), (Some(640), Some(480)));
    }

    #[test]
    fn utf16_text() {
        let data: Vec<u8> = "\u{feff}héllo".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(plain_text(&[rep(UTI_UTF16_PLAIN_TEXT, &data)]).unwrap(), "héllo");
    }
}
