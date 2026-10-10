//! Text cleanups that can be applied when pasting.

use std::sync::LazyLock;

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use regex::Regex;
use url::Url;

/// What to put on the pasteboard: plain text, plus HTML for formatted output.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub text: String,
    pub html: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Trim,
    SingleLine,
    Upper,
    Lower,
    Title,
    Sentence,
    Camel,
    Snake,
    Kebab,
    CleanLinks,
    PrettyJson,
    MinifyJson,
    SortLines,
    UniqueLines,
    UrlEncode,
    UrlDecode,
    Markdown,
}

impl Transform {
    pub const ALL: [Transform; 17] = [
        Transform::Trim,
        Transform::SingleLine,
        Transform::Upper,
        Transform::Lower,
        Transform::Title,
        Transform::Sentence,
        Transform::Camel,
        Transform::Snake,
        Transform::Kebab,
        Transform::CleanLinks,
        Transform::PrettyJson,
        Transform::MinifyJson,
        Transform::SortLines,
        Transform::UniqueLines,
        Transform::UrlEncode,
        Transform::UrlDecode,
        Transform::Markdown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Transform::Trim => "Trimmed",
            Transform::SingleLine => "Single Line",
            Transform::Upper => "UPPERCASE",
            Transform::Lower => "lowercase",
            Transform::Title => "Title Case",
            Transform::Sentence => "Sentence case",
            Transform::Camel => "camelCase",
            Transform::Snake => "snake_case",
            Transform::Kebab => "kebab-case",
            Transform::CleanLinks => "Links Without Tracking",
            Transform::PrettyJson => "Pretty JSON",
            Transform::MinifyJson => "Minified JSON",
            Transform::SortLines => "Sorted Lines",
            Transform::UniqueLines => "Without Duplicate Lines",
            Transform::UrlEncode => "URL-Encoded",
            Transform::UrlDecode => "URL-Decoded",
            Transform::Markdown => "Formatted Markdown",
        }
    }

    /// Name used by the command line, for example `snake`.
    pub fn name(self) -> &'static str {
        match self {
            Transform::Trim => "trim",
            Transform::SingleLine => "single-line",
            Transform::Upper => "upper",
            Transform::Lower => "lower",
            Transform::Title => "title",
            Transform::Sentence => "sentence",
            Transform::Camel => "camel",
            Transform::Snake => "snake",
            Transform::Kebab => "kebab",
            Transform::CleanLinks => "clean-links",
            Transform::PrettyJson => "pretty-json",
            Transform::MinifyJson => "minify-json",
            Transform::SortLines => "sort-lines",
            Transform::UniqueLines => "unique-lines",
            Transform::UrlEncode => "url-encode",
            Transform::UrlDecode => "url-decode",
            Transform::Markdown => "markdown",
        }
    }

    pub fn parse(name: &str) -> Option<Transform> {
        Transform::ALL.into_iter().find(|t| t.name().eq_ignore_ascii_case(name))
    }

    pub fn index(self) -> usize {
        Transform::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    /// Applies the transform. `None` when it does not apply (for example
    /// JSON formatting of text that is not JSON).
    pub fn apply(self, text: &str) -> Option<Output> {
        let plain = |text: String| Some(Output { text, html: None });
        match self {
            Transform::Trim => plain(text.trim().to_string()),
            Transform::SingleLine => plain(text.split_whitespace().collect::<Vec<_>>().join(" ")),
            Transform::Upper => plain(text.to_uppercase()),
            Transform::Lower => plain(text.to_lowercase()),
            Transform::Title => plain(title_case(text)),
            Transform::Sentence => plain(sentence_case(text)),
            Transform::Camel => plain(camel_case(text)),
            Transform::Snake => plain(words(text).join("_")),
            Transform::Kebab => plain(words(text).join("-")),
            Transform::CleanLinks => plain(clean_links(text)),
            Transform::PrettyJson => {
                let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
                plain(serde_json::to_string_pretty(&v).ok()?)
            }
            Transform::MinifyJson => {
                let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
                plain(serde_json::to_string(&v).ok()?)
            }
            Transform::SortLines => {
                let mut lines: Vec<&str> = text.lines().collect();
                lines.sort_by_key(|l| l.to_lowercase());
                plain(lines.join("\n"))
            }
            Transform::UniqueLines => {
                let mut seen = std::collections::HashSet::new();
                plain(text.lines().filter(|l| seen.insert(*l)).collect::<Vec<_>>().join("\n"))
            }
            Transform::UrlEncode => plain(utf8_percent_encode(text, NON_ALPHANUMERIC).to_string()),
            Transform::UrlDecode => {
                if !text.contains('%') {
                    return None;
                }
                plain(percent_decode_str(text).decode_utf8().ok()?.into_owned())
            }
            Transform::Markdown => {
                let mut html = String::new();
                let parser = pulldown_cmark::Parser::new_ext(text, pulldown_cmark::Options::all());
                pulldown_cmark::html::push_html(&mut html, parser);
                Some(Output { text: text.to_string(), html: Some(html) })
            }
        }
    }

    /// Transforms that would change `text`, in menu order.
    pub fn applicable(text: &str) -> Vec<(Transform, Output)> {
        if text.is_empty() || text.len() > 2_000_000 {
            return Vec::new();
        }
        Transform::ALL
            .into_iter()
            .filter(|t| *t != Transform::Markdown || looks_like_markdown(text))
            .filter_map(|t| t.apply(text).map(|o| (t, o)))
            .filter(|(_, o)| o.html.is_some() || o.text != text)
            .collect()
    }
}

static MARKDOWN_HINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^#{1,6}\s|^\s*[-*+]\s|^\s*\d+\.\s|\*\*[^*\n]+\*\*|`[^`\n]+`|\[[^\]\n]+\]\([^)\n]+\)|^>\s").unwrap()
});

pub fn looks_like_markdown(text: &str) -> bool {
    MARKDOWN_HINT.is_match(text)
}

/// Splits text into lowercase words, also at camelCase boundaries.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;
    for ch in text.chars() {
        if !ch.is_alphanumeric() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        prev_lower = ch.is_lowercase() || ch.is_numeric();
        current.extend(ch.to_lowercase());
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn camel_case(text: &str) -> String {
    let w = words(text);
    w.iter().enumerate().map(|(i, w)| if i == 0 { w.clone() } else { capitalize(w) }).collect()
}

fn title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_word_start = true;
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '\'' {
            if at_word_start {
                out.extend(ch.to_uppercase());
            } else {
                out.extend(ch.to_lowercase());
            }
            at_word_start = false;
        } else {
            out.push(ch);
            at_word_start = true;
        }
    }
    out
}

fn sentence_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut sentence_start = true;
    for ch in text.chars() {
        if ch.is_alphabetic() {
            if sentence_start {
                out.extend(ch.to_uppercase());
            } else {
                out.extend(ch.to_lowercase());
            }
            sentence_start = false;
        } else {
            out.push(ch);
            if matches!(ch, '.' | '!' | '?' | '\n') {
                sentence_start = true;
            }
        }
    }
    out
}

static URL_IN_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"']+"#).unwrap());

/// Query parameters that only exist to track clicks.
fn is_tracking_param(key: &str, host: &str) -> bool {
    let k = key.to_ascii_lowercase();
    if k.starts_with("utm_") || k.starts_with("pk_") || k.starts_with("hsa_") {
        return true;
    }
    let host_specific = matches!(k.as_str(), "si" | "feature" | "pp")
        && ["youtube.com", "youtu.be", "spotify.com"].iter().any(|h| host == *h || host.ends_with(&format!(".{h}")));
    host_specific
        || matches!(
            k.as_str(),
            "fbclid"
                | "gclid"
                | "gclsrc"
                | "dclid"
                | "gbraid"
                | "wbraid"
                | "msclkid"
                | "yclid"
                | "twclid"
                | "ttclid"
                | "li_fat_id"
                | "igshid"
                | "igsh"
                | "mc_cid"
                | "mc_eid"
                | "_hsenc"
                | "_hsmi"
                | "mkt_tok"
                | "vero_id"
                | "oly_anon_id"
                | "oly_enc_id"
                | "wickedid"
                | "ref_src"
                | "ref_url"
                | "s_cid"
                | "_ga"
                | "_gl"
        )
}

pub fn clean_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else { return raw.to_string() };
    if url.query().is_none() {
        return raw.to_string();
    }
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let kept: Vec<(String, String)> =
        url.query_pairs().filter(|(k, _)| !is_tracking_param(k, &host)).map(|(k, v)| (k.into(), v.into())).collect();
    if kept.len() == url.query_pairs().count() {
        return raw.to_string();
    }
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
    url.to_string()
}

/// Removes tracking parameters from every link in the text.
pub fn clean_links(text: &str) -> String {
    URL_IN_TEXT
        .replace_all(text, |c: &regex::Captures| {
            // Keep trailing punctuation that belongs to the sentence, not the link.
            let m = &c[0];
            let trimmed = m.trim_end_matches(['.', ',', ')', ';', ':', '!', '?']);
            format!("{}{}", clean_url(trimmed), &m[trimmed.len()..])
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(tr: Transform, s: &str) -> String {
        tr.apply(s).unwrap().text
    }

    #[test]
    fn cases() {
        assert_eq!(t(Transform::Upper, "Hello world"), "HELLO WORLD");
        assert_eq!(t(Transform::Title, "the QUICK brown-fox's tail"), "The Quick Brown-Fox's Tail");
        assert_eq!(t(Transform::Sentence, "HELLO THERE. how ARE you?\nfine"), "Hello there. How are you?\nFine");
        assert_eq!(t(Transform::Camel, "Hello big_world-42"), "helloBigWorld42");
        assert_eq!(t(Transform::Snake, "helloBigWorld HTTPServer"), "hello_big_world_httpserver");
        assert_eq!(t(Transform::Kebab, "Some Title: Here!"), "some-title-here");
    }

    #[test]
    fn whitespace_and_lines() {
        assert_eq!(t(Transform::Trim, "  a b \n"), "a b");
        assert_eq!(t(Transform::SingleLine, "one\n  two\tthree "), "one two three");
        assert_eq!(t(Transform::SortLines, "b\nA\nc"), "A\nb\nc");
        assert_eq!(t(Transform::UniqueLines, "a\nb\na\nc\nb"), "a\nb\nc");
    }

    #[test]
    fn json() {
        assert_eq!(
            t(Transform::PrettyJson, r#"{"b":1,"a":[1,2]}"#),
            "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(t(Transform::MinifyJson, "{\n \"b\": 1, \"a\": true\n}"), r#"{"b":1,"a":true}"#);
        assert!(Transform::PrettyJson.apply("not json").is_none());
    }

    #[test]
    fn links() {
        assert_eq!(
            clean_url("https://example.com/p?id=7&utm_source=x&utm_medium=y&fbclid=abc#top"),
            "https://example.com/p?id=7#top"
        );
        assert_eq!(clean_url("https://example.com/?utm_campaign=z"), "https://example.com/");
        assert_eq!(clean_url("https://youtu.be/abc?si=TRACK&t=42"), "https://youtu.be/abc?t=42");
        assert_eq!(clean_url("https://example.com/?si=keep"), "https://example.com/?si=keep");
        assert_eq!(clean_url("https://example.com/a b"), "https://example.com/a b");
        assert_eq!(
            clean_links("See https://a.com/x?gclid=1. And (https://b.com/?q=1&utm_term=2)."),
            "See https://a.com/x. And (https://b.com/?q=1)."
        );
    }

    #[test]
    fn url_encoding() {
        assert_eq!(t(Transform::UrlEncode, "a b/ç"), "a%20b%2F%C3%A7");
        assert_eq!(t(Transform::UrlDecode, "a%20b%2F%C3%A7"), "a b/ç");
        assert!(Transform::UrlDecode.apply("plain").is_none());
    }

    #[test]
    fn markdown() {
        let o = Transform::Markdown.apply("# Title\n\nSome **bold** text").unwrap();
        assert_eq!(o.text, "# Title\n\nSome **bold** text");
        assert!(o.html.unwrap().contains("<strong>bold</strong>"));
    }

    #[test]
    fn applicable_only_lists_changes() {
        let names: Vec<_> = Transform::applicable("hello").into_iter().map(|(t, _)| t).collect();
        assert!(names.contains(&Transform::Upper) && names.contains(&Transform::Title));
        assert!(!names.contains(&Transform::Lower) && !names.contains(&Transform::Trim));
        assert!(!names.contains(&Transform::PrettyJson) && !names.contains(&Transform::Markdown));
        let md: Vec<_> = Transform::applicable("- item\n- **two**").into_iter().map(|(t, _)| t).collect();
        assert!(md.contains(&Transform::Markdown));
    }

    #[test]
    fn names_round_trip() {
        for t in Transform::ALL {
            assert_eq!(Transform::parse(t.name()), Some(t));
            assert_eq!(Transform::ALL[t.index()], t);
        }
    }
}
