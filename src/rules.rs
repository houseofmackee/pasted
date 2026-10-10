//! Rules applied to each new clip: expire it after a while, or pin it to a
//! pinboard. All conditions of a rule must match; empty conditions match
//! everything.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::model::ItemKind;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub name: String,
    pub enabled: bool,
    /// Bundle identifier or app name of the source app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Item kinds, for example `["text", "code"]`. Empty matches all.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kind: Vec<String>,
    /// Regular expression the item's text must match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
    /// Remove from history after this long: `30s`, `15m`, `1h`, `2d`, `1w`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_after: Option<String>,
    /// Pinboard name; created if it does not exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_to: Option<String>,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            name: String::new(),
            enabled: true,
            app: None,
            kind: Vec::new(),
            matches: None,
            expire_after: None,
            pin_to: None,
        }
    }
}

/// Parses `30s`, `15m`, `1h`, `2d` or `1w` into milliseconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim().to_ascii_lowercase();
    let unit_at = s.find(|c: char| !c.is_ascii_digit())?;
    let n: i64 = s[..unit_at].parse().ok().filter(|n| *n > 0)?;
    let unit = match s[unit_at..].trim() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1_000,
        "m" | "min" | "mins" | "minute" | "minutes" => 60_000,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000,
        "d" | "day" | "days" => 86_400_000,
        "w" | "wk" | "week" | "weeks" => 7 * 86_400_000,
        _ => return None,
    };
    n.checked_mul(unit)
}

/// Short human form of a duration, for example "45m" or "2d".
pub fn format_duration(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

impl Rule {
    /// One line description, used in lists.
    pub fn summary(&self) -> String {
        let mut when = Vec::new();
        if let Some(app) = &self.app {
            when.push(format!("from {app}"));
        }
        if !self.kind.is_empty() {
            when.push(self.kind.join("/"));
        }
        if let Some(p) = &self.matches {
            when.push(format!("matching /{p}/"));
        }
        let mut then = Vec::new();
        if let Some(e) = &self.expire_after {
            then.push(format!("expire after {e}"));
        }
        if let Some(b) = &self.pin_to {
            then.push(format!("pin to {b}"));
        }
        let when = if when.is_empty() { "every copy".to_string() } else { when.join(", ") };
        let then = if then.is_empty() { "do nothing".to_string() } else { then.join(" and ") };
        format!("{when}: {then}")
    }

    /// Problems that make the rule unusable.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(p) = &self.matches {
            Regex::new(p).map_err(|e| format!("invalid pattern: {}", e.to_string().lines().last().unwrap_or("")))?;
        }
        if let Some(e) = &self.expire_after
            && parse_duration(e).is_none()
        {
            return Err(format!("invalid duration {e:?} (use 30s, 15m, 1h, 2d or 1w)"));
        }
        if let Some(k) = self.kind.iter().find(|k| ItemKind::parse(k).is_none()) {
            return Err(format!("unknown kind {k:?}"));
        }
        if self.expire_after.is_none() && self.pin_to.as_deref().is_none_or(|p| p.trim().is_empty()) {
            return Err("the rule has no action".into());
        }
        Ok(())
    }
}

struct Compiled {
    rule: Rule,
    pattern: Option<Regex>,
    kinds: Vec<ItemKind>,
    expire_ms: Option<i64>,
}

/// Valid, enabled rules ready to evaluate.
pub struct Rules {
    compiled: Vec<Compiled>,
}

/// What the matching rules ask for.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Shortest expiry among matching rules.
    pub expire_after_ms: Option<i64>,
    pub pin_to: Vec<String>,
    pub matched: Vec<String>,
}

impl Rules {
    /// Compiles rules; invalid ones are skipped and reported.
    pub fn new(rules: &[Rule]) -> (Rules, Vec<String>) {
        let mut errors = Vec::new();
        let mut compiled = Vec::new();
        for (i, rule) in rules.iter().enumerate() {
            if !rule.enabled {
                continue;
            }
            if let Err(e) = rule.validate() {
                let name = if rule.name.is_empty() { format!("rule {}", i + 1) } else { rule.name.clone() };
                errors.push(format!("{name}: {e}"));
                continue;
            }
            compiled.push(Compiled {
                pattern: rule.matches.as_deref().and_then(|p| Regex::new(p).ok()),
                kinds: rule.kind.iter().filter_map(|k| ItemKind::parse(k)).collect(),
                expire_ms: rule.expire_after.as_deref().and_then(parse_duration),
                rule: rule.clone(),
            });
        }
        (Rules { compiled }, errors)
    }

    pub fn is_empty(&self) -> bool {
        self.compiled.is_empty()
    }

    pub fn evaluate(&self, kind: ItemKind, text: &str, app_bundle: Option<&str>, app_name: Option<&str>) -> Outcome {
        let mut out = Outcome::default();
        for c in &self.compiled {
            if let Some(app) = &c.rule.app {
                let hit = [app_bundle, app_name].iter().flatten().any(|a| a.eq_ignore_ascii_case(app.trim()));
                if !hit {
                    continue;
                }
            }
            if !c.kinds.is_empty() && !c.kinds.contains(&kind) {
                continue;
            }
            if let Some(p) = &c.pattern
                && !p.is_match(text)
            {
                continue;
            }
            if let Some(ms) = c.expire_ms {
                out.expire_after_ms = Some(out.expire_after_ms.map_or(ms, |cur| cur.min(ms)));
            }
            if let Some(board) = c.rule.pin_to.as_deref().map(str::trim).filter(|b| !b.is_empty())
                && !out.pin_to.iter().any(|p| p.eq_ignore_ascii_case(board))
            {
                out.pin_to.push(board.to_string());
            }
            out.matched.push(if c.rule.name.is_empty() { c.rule.summary() } else { c.rule.name.clone() });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(f: impl FnOnce(&mut Rule)) -> Rule {
        let mut r = Rule::default();
        f(&mut r);
        r
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s"), Some(30_000));
        assert_eq!(parse_duration(" 15 min "), Some(900_000));
        assert_eq!(parse_duration("1h"), Some(3_600_000));
        assert_eq!(parse_duration("2d"), Some(172_800_000));
        assert_eq!(parse_duration("1w"), Some(604_800_000));
        for bad in ["", "h", "0h", "-1h", "1y", "abc"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
        assert_eq!(format_duration(45 * 60_000), "45m");
        assert_eq!(format_duration(3 * 86_400_000), "3d");
    }

    #[test]
    fn matching_and_combining() {
        let (rules, errors) = Rules::new(&[
            rule(|r| {
                r.app = Some("Terminal".into());
                r.expire_after = Some("1h".into());
            }),
            rule(|r| {
                r.matches = Some(r"(?i)^jira-\d+".into());
                r.pin_to = Some("Tickets".into());
            }),
            rule(|r| {
                r.kind = vec!["code".into()];
                r.expire_after = Some("10m".into());
            }),
            rule(|r| {
                r.enabled = false;
                r.pin_to = Some("Never".into());
            }),
        ]);
        assert!(errors.is_empty());
        let o = rules.evaluate(ItemKind::Code, "JIRA-12 fix", Some("com.apple.Terminal"), Some("Terminal"));
        assert_eq!(o.expire_after_ms, Some(600_000));
        assert_eq!(o.pin_to, ["Tickets"]);
        assert_eq!(o.matched.len(), 3);
        let o = rules.evaluate(ItemKind::Text, "hello", Some("com.apple.Safari"), Some("Safari"));
        assert_eq!(o, Outcome::default());
        let o = rules.evaluate(ItemKind::Text, "x", Some("com.apple.Terminal"), Some("terminal"));
        assert_eq!(o.expire_after_ms, Some(3_600_000));
    }

    #[test]
    fn invalid_rules_are_reported_and_skipped() {
        let (rules, errors) = Rules::new(&[
            rule(|r| {
                r.name = "bad regex".into();
                r.matches = Some("(".into());
                r.pin_to = Some("X".into());
            }),
            rule(|r| r.expire_after = Some("soon".into())),
            rule(|r| {
                r.kind = vec!["video".into()];
                r.pin_to = Some("X".into());
            }),
            rule(|r| r.app = Some("Terminal".into())),
        ]);
        assert!(rules.is_empty());
        assert_eq!(errors.len(), 4);
        assert!(errors[0].starts_with("bad regex: invalid pattern"));
        assert!(errors[3].contains("no action"));
    }

    #[test]
    fn toml_round_trip() {
        let toml_text = r#"
            [[rules]]
            name = "Terminal is temporary"
            app = "com.apple.Terminal"
            expire_after = "1h"

            [[rules]]
            matches = "^https://github.com/"
            kind = ["link"]
            pin_to = "GitHub"
        "#;
        #[derive(Deserialize, Serialize)]
        struct W {
            rules: Vec<Rule>,
        }
        let w: W = toml::from_str(toml_text).unwrap();
        assert_eq!(w.rules.len(), 2);
        assert!(w.rules[0].enabled);
        assert_eq!(w.rules[1].summary(), "link, matching /^https://github.com//: pin to GitHub");
        let again: W = toml::from_str(&toml::to_string(&w).unwrap()).unwrap();
        assert_eq!(again.rules, w.rules);
    }
}
