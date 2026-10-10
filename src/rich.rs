//! A small, safe HTML to styled-text converter for rich text clips.
//!
//! AppKit's own HTML importer runs WebKit synchronously on the main thread,
//! which is slow and can load remote resources. Clipboard HTML only needs a
//! handful of inline styles and block structure, so this handles those and
//! ignores everything else (scripts, styles, images, embeds).

use std::sync::LazyLock;

use regex::Regex;

use crate::clipboard::classify::parse_color;
use crate::link_preview::decode_entities;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
    /// 1 to 6 for h1 to h6, 0 for body text.
    pub heading: u8,
    /// sRGB color from the source, if any.
    pub color: Option<[f64; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->|<[^>]*>|[^<]+").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)^<\s*(/?)\s*([a-zA-Z0-9]+)(.*?)/?\s*>$").unwrap());
static STYLE_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)\b(style|color)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#).unwrap());

const SKIP: &[&str] = &["script", "style", "head", "title", "template", "noscript", "svg", "iframe", "object"];
const BLOCK: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "blockquote",
    "ul",
    "ol",
    "table",
    "tr",
    "pre",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "dl",
    "dt",
    "dd",
    "figure",
    "figcaption",
    "hr",
    "main",
    "aside",
    "nav",
];

fn named_color(name: &str) -> Option<[f64; 3]> {
    Some(match name {
        "black" => [0.0, 0.0, 0.0],
        "white" => [1.0, 1.0, 1.0],
        "red" => [1.0, 0.0, 0.0],
        "green" => [0.0, 0.5, 0.0],
        "blue" => [0.0, 0.0, 1.0],
        "gray" | "grey" => [0.5, 0.5, 0.5],
        "orange" => [1.0, 0.65, 0.0],
        "purple" => [0.5, 0.0, 0.5],
        _ => return None,
    })
}

fn color_value(v: &str) -> Option<[f64; 3]> {
    let v = v.trim().to_ascii_lowercase();
    parse_color(&v).map(|[r, g, b, _]| [r, g, b]).or_else(|| named_color(&v))
}

/// Applies the parts of a `style` attribute (and `<font color>`) we support.
fn apply_css(style: &mut Style, attrs: &str) {
    for c in STYLE_ATTR.captures_iter(attrs) {
        let value = c.get(2).or(c.get(3)).or(c.get(4)).map(|m| m.as_str()).unwrap_or("");
        if c[1].eq_ignore_ascii_case("color") {
            if let Some(color) = color_value(value) {
                style.color = Some(color);
            }
            continue;
        }
        for decl in value.split(';') {
            let Some((prop, val)) = decl.split_once(':') else { continue };
            let (prop, val) = (prop.trim().to_ascii_lowercase(), val.trim().to_ascii_lowercase());
            match prop.as_str() {
                "font-weight" => {
                    style.bold = val == "bold" || val == "bolder" || val.parse::<u32>().is_ok_and(|w| w >= 600)
                }
                "font-style" => style.italic = val.contains("italic") || val.contains("oblique"),
                "text-decoration" | "text-decoration-line" => {
                    style.underline |= val.contains("underline");
                    style.strike |= val.contains("line-through");
                }
                "color" => {
                    if let Some(color) = color_value(&val) {
                        style.color = Some(color);
                    }
                }
                "font-family" if val.contains("mono") || val.contains("courier") || val.contains("menlo") => {
                    style.code = true
                }
                _ => {}
            }
        }
    }
}

struct Builder {
    runs: Vec<Run>,
    chars: usize,
    max: usize,
}

impl Builder {
    fn trailing_newlines(&self) -> usize {
        let mut n = 0;
        for run in self.runs.iter().rev() {
            for c in run.text.chars().rev() {
                if c == '\n' {
                    n += 1;
                } else {
                    return n;
                }
            }
        }
        if self.runs.is_empty() { usize::MAX } else { n }
    }

    fn at_line_start(&self) -> bool {
        self.trailing_newlines() > 0
    }

    fn push(&mut self, text: &str, style: &Style) {
        if text.is_empty() || self.chars >= self.max {
            return;
        }
        if text.starts_with('\n') {
            // Spaces before a line break are invisible noise.
            while let Some(last) = self.runs.last_mut() {
                let kept = last.text.trim_end_matches(' ').len();
                self.chars -= last.text.len() - kept;
                last.text.truncate(kept);
                if !last.text.is_empty() {
                    break;
                }
                self.runs.pop();
            }
        }
        let room = self.max - self.chars;
        let text: String = text.chars().take(room).collect();
        self.chars += text.chars().count();
        match self.runs.last_mut() {
            Some(last) if last.style == *style => last.text.push_str(&text),
            _ => self.runs.push(Run { text, style: style.clone() }),
        }
    }

    /// Ensures the output ends with at least `n` newlines (but not at the start).
    fn newlines(&mut self, n: usize) {
        let have = self.trailing_newlines();
        if have == usize::MAX {
            return;
        }
        for _ in have..n {
            self.push("\n", &Style::default());
        }
    }
}

/// Converts clipboard HTML to styled runs, keeping at most `max_chars` characters.
pub fn html_to_runs(html: &str, max_chars: usize) -> Vec<Run> {
    let mut b = Builder { runs: Vec::new(), chars: 0, max: max_chars };
    // Open elements and the style in effect inside each.
    let mut stack: Vec<(String, Style)> = Vec::new();
    let mut skip_depth: Option<(String, usize)> = None;
    let mut pre = 0usize;
    let mut lists: Vec<Option<usize>> = Vec::new();

    for token in TOKEN.find_iter(html) {
        if b.chars >= max_chars {
            break;
        }
        let t = token.as_str();
        if t.starts_with("<!--") {
            continue;
        }
        if let Some(c) = TAG.captures(t) {
            let closing = !c[1].is_empty();
            let name = c[2].to_ascii_lowercase();
            let attrs = c.get(3).map(|m| m.as_str()).unwrap_or("");

            if let Some((skip_name, depth)) = &mut skip_depth {
                if *skip_name == name {
                    if closing {
                        *depth -= 1;
                    } else {
                        *depth += 1;
                    }
                    if *depth == 0 {
                        skip_depth = None;
                    }
                }
                continue;
            }
            if !closing && SKIP.contains(&name.as_str()) {
                skip_depth = Some((name, 1));
                continue;
            }

            if closing {
                if let Some(pos) = stack.iter().rposition(|(n, _)| *n == name) {
                    stack.truncate(pos);
                }
                match name.as_str() {
                    "pre" => pre = pre.saturating_sub(1),
                    "ul" | "ol" => {
                        lists.pop();
                    }
                    _ => {}
                }
                if name.starts_with('h') && name.len() == 2 || name == "p" {
                    b.newlines(2);
                } else if BLOCK.contains(&name.as_str()) {
                    b.newlines(1);
                }
                continue;
            }

            let mut style = stack.last().map(|(_, s)| s.clone()).unwrap_or_default();
            match name.as_str() {
                "br" => {
                    b.push("\n", &style);
                    continue;
                }
                "hr" => {
                    b.newlines(1);
                    continue;
                }
                "img" | "input" | "meta" | "link" | "wbr" | "col" | "source" => continue,
                "td" | "th" => {
                    if !b.at_line_start() {
                        b.push("\t", &style);
                    }
                }
                "li" => {
                    b.newlines(1);
                    let depth = lists.len().saturating_sub(1);
                    let marker = match lists.last_mut() {
                        Some(Some(n)) => {
                            *n += 1;
                            format!("{}{n}. ", "    ".repeat(depth))
                        }
                        _ => format!("{}\u{2022} ", "    ".repeat(depth)),
                    };
                    b.push(&marker, &style);
                }
                "ul" => {
                    b.newlines(1);
                    lists.push(None);
                }
                "ol" => {
                    b.newlines(1);
                    lists.push(Some(0));
                }
                "p" => b.newlines(2),
                n if BLOCK.contains(&n) => b.newlines(1),
                _ => {}
            }
            match name.as_str() {
                "b" | "strong" | "th" => style.bold = true,
                "i" | "em" | "cite" | "var" => style.italic = true,
                "u" | "ins" => style.underline = true,
                "s" | "strike" | "del" => style.strike = true,
                "code" | "kbd" | "samp" | "tt" => style.code = true,
                "pre" => {
                    style.code = true;
                    pre += 1;
                }
                "a" if attrs.to_ascii_lowercase().contains("href") => style.link = true,
                h if h.len() == 2 && h.starts_with('h') => {
                    if let Some(level) = h[1..].parse::<u8>().ok().filter(|l| (1..=6).contains(l)) {
                        style.heading = level;
                        style.bold = true;
                        b.newlines(2);
                    }
                }
                _ => {}
            }
            apply_css(&mut style, attrs);
            stack.push((name, style));
            continue;
        }
        if skip_depth.is_some() || t.starts_with('<') {
            continue;
        }
        let style = stack.last().map(|(_, s)| s.clone()).unwrap_or_default();
        let text = decode_entities(t).replace('\u{a0}', " ");
        if pre > 0 {
            b.push(&text, &style);
            continue;
        }
        let mut collapsed = String::with_capacity(text.len());
        let mut last_space = false;
        for ch in text.chars() {
            if ch.is_whitespace() {
                if !last_space {
                    collapsed.push(' ');
                }
                last_space = true;
            } else {
                collapsed.push(ch);
                last_space = false;
            }
        }
        let ends_space = b.runs.last().is_some_and(|r| r.text.ends_with([' ', '\n', '\t']));
        if b.at_line_start() || ends_space {
            collapsed = collapsed.trim_start().to_string();
        }
        b.push(&collapsed, &style);
    }

    // Trim trailing whitespace and drop empty runs.
    while let Some(last) = b.runs.last_mut() {
        let trimmed = last.text.trim_end().len();
        last.text.truncate(trimmed);
        if last.text.is_empty() {
            b.runs.pop();
        } else {
            break;
        }
    }
    b.runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(runs: &[Run]) -> String {
        runs.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn inline_styles() {
        let runs = html_to_runs("<p>Hello <b>bold</b> and <i>it</i> <a href='x'>link</a></p>", 1000);
        assert_eq!(plain(&runs), "Hello bold and it link");
        assert!(runs.iter().any(|r| r.text == "bold" && r.style.bold));
        assert!(runs.iter().any(|r| r.text == "it" && r.style.italic));
        assert!(runs.iter().any(|r| r.text == "link" && r.style.link));
    }

    #[test]
    fn blocks_lists_and_headings() {
        let html = "<meta charset='utf-8'><h1>Title</h1><p>One</p><p>Two<br>Three</p><ul><li>a</li><li>b</li></ul><ol><li>x</li><li>y</li></ol>";
        let runs = html_to_runs(html, 1000);
        assert_eq!(plain(&runs), "Title\n\nOne\n\nTwo\nThree\n\n\u{2022} a\n\u{2022} b\n1. x\n2. y");
        assert_eq!(runs[0].style.heading, 1);
        assert!(runs[0].style.bold);
    }

    #[test]
    fn css_and_colors() {
        let html = r##"<span style="font-weight: 700; color: rgb(255, 0, 0)">red</span><font color="#00ff00">green</font><span style="font-family: Menlo">mono</span>"##;
        let runs = html_to_runs(html, 1000);
        assert_eq!(runs[0].style.color, Some([1.0, 0.0, 0.0]));
        assert!(runs[0].style.bold);
        assert_eq!(runs[1].style.color, Some([0.0, 1.0, 0.0]));
        assert!(runs[2].style.code);
    }

    #[test]
    fn skips_scripts_styles_and_comments() {
        let html = "<style>p{color:red}</style><script>alert(1)</script><!-- note --><p>safe &amp; sound</p>";
        assert_eq!(plain(&html_to_runs(html, 1000)), "safe & sound");
    }

    #[test]
    fn whitespace_and_pre() {
        let html = "<div>  lots   of\n\n space </div><pre>  keep\n    this</pre>";
        assert_eq!(plain(&html_to_runs(html, 1000)), "lots of space\n  keep\n    this");
    }

    #[test]
    fn truncates() {
        let runs = html_to_runs("<p>abcdefghij</p>", 4);
        assert_eq!(plain(&runs), "abcd");
    }
}
