//! Placeholders in pinboard snippets, expanded when pasting.
//!
//! Only known names are placeholders, so other text in braces (code, JSON,
//! templates for other tools) is left alone. A backslash keeps a placeholder
//! literal: `\{date}` pastes as `{date}`.
//!
//! | Placeholder | Result |
//! | --- | --- |
//! | `{date}` `{time}` `{datetime}` | current date and time, optional strftime format after `:` |
//! | `{weekday}` `{month}` `{year}` | parts of the date |
//! | `{date+7d}` `{weekday-1w}` | date offsets in days (d) or weeks (w) |
//! | `{clipboard}` | text currently on the clipboard |
//! | `{uuid}` | a random UUID |
//! | `{cursor}` | where the cursor is left after pasting |
//! | `{input:Name}` | asked for when pasting |

use std::collections::HashMap;
use std::sync::LazyLock;

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Duration, Local};
use regex::Regex;

static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\\\{|\{(date|time|datetime|weekday|month|year|clipboard|cursor|uuid|input)([+-]\d{1,4}[dw])?(?::([^{}\n]{1,100}))?\}",
    )
    .unwrap()
});

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Text(String),
    Placeholder { name: String, offset_days: i64, arg: Option<String> },
    Cursor,
}

fn tokenize(template: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut last = 0;
    for c in PLACEHOLDER.captures_iter(template) {
        let m = c.get(0).unwrap();
        if m.start() > last {
            out.push(Token::Text(template[last..m.start()].to_string()));
        }
        last = m.end();
        if m.as_str() == "\\{" {
            out.push(Token::Text("{".into()));
            continue;
        }
        let name = c[1].to_string();
        let offset_days = c.get(2).map_or(0, |o| {
            let s = o.as_str();
            let n: i64 = s[..s.len() - 1].parse().unwrap_or(0);
            if s.ends_with('w') { n * 7 } else { n }
        });
        let arg = c.get(3).map(|a| a.as_str().to_string());
        // Offsets only make sense for dates.
        if offset_days != 0 && !matches!(name.as_str(), "date" | "datetime" | "weekday" | "month" | "year") {
            out.push(Token::Text(m.as_str().to_string()));
            continue;
        }
        out.push(if name == "cursor" { Token::Cursor } else { Token::Placeholder { name, offset_days, arg } });
    }
    if last < template.len() {
        out.push(Token::Text(template[last..].to_string()));
    }
    out
}

pub fn has_placeholders(text: &str) -> bool {
    tokenize(text).iter().any(|t| !matches!(t, Token::Text(_)))
}

/// Labels of `{input:...}` placeholders, unique, in order of appearance.
pub fn inputs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tokenize(text) {
        if let Token::Placeholder { name, arg, .. } = t
            && name == "input"
        {
            let label = arg.unwrap_or_else(|| "Text".into()).trim().to_string();
            if !out.contains(&label) {
                out.push(label);
            }
        }
    }
    out
}

pub struct Context {
    pub now: DateTime<Local>,
    pub clipboard: Option<String>,
    pub inputs: HashMap<String, String>,
}

impl Context {
    pub fn now() -> Context {
        Context { now: Local::now(), clipboard: None, inputs: HashMap::new() }
    }
}

#[derive(Debug, PartialEq)]
pub struct Expanded {
    pub text: String,
    /// Characters between `{cursor}` and the end, if it was used.
    pub cursor_from_end: Option<usize>,
}

/// Formats with a user-supplied strftime string, falling back to `default`
/// when the format is invalid (chrono would otherwise panic).
fn format_date(when: DateTime<Local>, format: Option<&str>, default: &str) -> String {
    let fmt = format.filter(|f| !StrftimeItems::new(f).any(|i| matches!(i, Item::Error))).unwrap_or(default);
    when.format(fmt).to_string()
}

fn value(name: &str, offset_days: i64, arg: Option<&str>, ctx: &Context) -> String {
    let when = ctx.now + Duration::days(offset_days);
    match name {
        "date" => format_date(when, arg, "%Y-%m-%d"),
        "time" => format_date(when, arg, "%H:%M"),
        "datetime" => format_date(when, arg, "%Y-%m-%d %H:%M"),
        "weekday" => when.format("%A").to_string(),
        "month" => when.format("%B").to_string(),
        "year" => when.format("%Y").to_string(),
        "clipboard" => ctx.clipboard.clone().unwrap_or_default(),
        "uuid" => uuid::Uuid::new_v4().to_string(),
        "input" => {
            let label = arg.unwrap_or("Text").trim();
            ctx.inputs.get(label).cloned().unwrap_or_default()
        }
        _ => String::new(),
    }
}

pub fn expand(template: &str, ctx: &Context) -> Expanded {
    let mut text = String::with_capacity(template.len());
    let mut cursor_at: Option<usize> = None;
    for t in tokenize(template) {
        match t {
            Token::Text(s) => text.push_str(&s),
            Token::Cursor => {
                cursor_at.get_or_insert(text.chars().count());
            }
            Token::Placeholder { name, offset_days, arg } => {
                text.push_str(&value(&name, offset_days, arg.as_deref(), ctx))
            }
        }
    }
    let total = text.chars().count();
    Expanded { cursor_from_end: cursor_at.map(|at| total - at), text }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\n', "<br>")
}

/// Expands placeholders inside HTML. Values are escaped; `{cursor}` is dropped.
pub fn expand_html(html: &str, ctx: &Context) -> String {
    let mut out = String::with_capacity(html.len());
    for t in tokenize(html) {
        match t {
            Token::Text(s) => out.push_str(&s),
            Token::Cursor => {}
            Token::Placeholder { name, offset_days, arg } => {
                out.push_str(&escape_html(&value(&name, offset_days, arg.as_deref(), ctx)))
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn ctx() -> Context {
        Context {
            now: Local.with_ymd_and_hms(2026, 10, 9, 14, 5, 0).unwrap(),
            clipboard: Some("CLIP".into()),
            inputs: HashMap::from([("Name".to_string(), "Ada".to_string())]),
        }
    }

    #[test]
    fn dates_and_offsets() {
        let e = expand("{date} {time} {datetime} {weekday} {month} {year}", &ctx());
        assert_eq!(e.text, "2026-10-09 14:05 2026-10-09 14:05 Friday October 2026");
        assert_eq!(expand("{date+3d} {date-1w} {weekday+1d}", &ctx()).text, "2026-10-12 2026-10-02 Saturday");
        assert_eq!(expand("{date:%d/%m/%Y} {time:%I%p}", &ctx()).text, "09/10/2026 02PM");
        // Invalid formats fall back instead of panicking.
        assert_eq!(expand("{date:%Q}", &ctx()).text, "2026-10-09");
    }

    #[test]
    fn clipboard_inputs_and_cursor() {
        let e = expand("Hi {input:Name}, re: {clipboard}{cursor}!\nBye {input:Name}", &ctx());
        assert_eq!(e.text, "Hi Ada, re: CLIP!\nBye Ada");
        assert_eq!(e.cursor_from_end, Some("!\nBye Ada".chars().count()));
        assert_eq!(inputs("{input:Name} {input: Name } {input:Email} {input}"), ["Name", "Email", "Text"]);
    }

    #[test]
    fn unknown_and_escaped_braces_stay() {
        let code = "fn f() { format!(\"{name} {}\", x) } {date+3h} \\{date}";
        let e = expand(code, &ctx());
        assert_eq!(e.text, "fn f() { format!(\"{name} {}\", x) } {date+3h} {date}");
        assert!(!has_placeholders("{name} {} {\"json\": 1}"));
        assert!(has_placeholders("Due {date+7d}"));
        assert!(!has_placeholders("\\{date}"));
    }

    #[test]
    fn uuid_is_random() {
        let a = expand("{uuid}", &ctx()).text;
        assert_eq!(a.len(), 36);
        assert_ne!(a, expand("{uuid}", &ctx()).text);
    }

    #[test]
    fn html_values_are_escaped() {
        let mut c = ctx();
        c.clipboard = Some("<b>&</b>".into());
        assert_eq!(expand_html("<p>{clipboard}{cursor} {year}</p>", &c), "<p>&lt;b&gt;&amp;&lt;/b&gt; 2026</p>");
    }
}
