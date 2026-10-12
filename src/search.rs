//! Search query syntax: free text plus filters.
//!
//! `type:link app:safari device:macbook after:2025-01-01 before:2025-02-01 board:work some words`
//! Filters may be negated with a leading `-`, for example `-type:image`.

use chrono::{Local, NaiveDate, TimeZone};

use crate::model::ItemKind;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub text: String,
    pub kinds: Vec<ItemKind>,
    pub exclude_kinds: Vec<ItemKind>,
    pub apps: Vec<String>,
    /// Part of a device name, lowercased; "unknown" means no device recorded.
    pub devices: Vec<String>,
    pub after_ms: Option<i64>,
    pub before_ms: Option<i64>,
    /// Pinboard name from `board:`; resolved by the store.
    pub board_name: Option<String>,
    /// Pinboard id set by the UI; `None` means clipboard history.
    pub board_id: Option<i64>,
}

fn day_start_ms(s: &str) -> Option<i64> {
    let date = match s {
        "today" => Local::now().date_naive(),
        "yesterday" => Local::now().date_naive().pred_opt()?,
        _ => NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?,
    };
    let dt = date.and_hms_opt(0, 0, 0)?;
    Some(Local.from_local_datetime(&dt).earliest()?.timestamp_millis())
}

impl Query {
    pub fn parse(input: &str) -> Query {
        let mut q = Query::default();
        let mut words = Vec::new();
        for token in input.split_whitespace() {
            let (negated, body) = match token.strip_prefix('-') {
                Some(rest) if rest.contains(':') => (true, rest),
                _ => (false, token),
            };
            let Some((key, value)) = body.split_once(':') else {
                words.push(token);
                continue;
            };
            if value.is_empty() {
                words.push(token);
                continue;
            }
            match key.to_ascii_lowercase().as_str() {
                "type" | "kind" | "is" => match ItemKind::parse(value) {
                    Some(k) if negated => q.exclude_kinds.push(k),
                    Some(k) => q.kinds.push(k),
                    None => words.push(token),
                },
                "app" | "from" => q.apps.push(value.to_lowercase()),
                "device" | "mac" => q.devices.push(value.to_lowercase()),
                "after" | "since" => match day_start_ms(value) {
                    Some(ms) => q.after_ms = Some(ms),
                    None => words.push(token),
                },
                "before" | "until" => match day_start_ms(value) {
                    Some(ms) => q.before_ms = Some(ms),
                    None => words.push(token),
                },
                "board" | "pinboard" => q.board_name = Some(value.to_string()),
                _ => words.push(token),
            }
        }
        q.text = words.join(" ");
        q
    }

    #[cfg(test)]
    pub fn text(text: &str) -> Query {
        Query { text: text.to_string(), ..Query::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_filters() {
        let q = Query::parse("type:link -type:image app:Safari hello  world board:Work");
        assert_eq!(q.text, "hello world");
        assert_eq!(q.kinds, vec![ItemKind::Link]);
        assert_eq!(q.exclude_kinds, vec![ItemKind::Image]);
        assert_eq!(q.apps, vec!["safari"]);
        assert_eq!(q.board_name.as_deref(), Some("Work"));
        let q = Query::parse("device:MacBook mac:unknown notes");
        assert_eq!(q.devices, vec!["macbook", "unknown"]);
        assert_eq!(q.text, "notes");
    }

    #[test]
    fn keeps_unknown_tokens_as_text() {
        let q = Query::parse("http://x.com type:nope -dash after:soon");
        assert_eq!(q.text, "http://x.com type:nope -dash after:soon");
        assert!(q.kinds.is_empty());
        assert!(q.after_ms.is_none());
    }

    #[test]
    fn dates() {
        let q = Query::parse("after:2025-01-01 before:2025-02-01");
        assert!(q.after_ms.unwrap() < q.before_ms.unwrap());
        assert!(Query::parse("after:today").after_ms.is_some());
    }
}
