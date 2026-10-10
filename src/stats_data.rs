//! The numbers behind the Statistics charts as plain tables: shown in the
//! window's Table view, copied with Copy Data and printed by
//! `pasted stats --tsv`. Values are exact (no "1.2K") so they paste cleanly
//! into a spreadsheet.

use chrono::TimeZone;

use crate::model::ItemKind;
use crate::store::Report;

pub struct Table {
    pub title: String,
    pub headers: Vec<&'static str>,
    pub rows: Vec<Vec<String>>,
    /// Right-aligned columns in the text layout.
    pub numeric: Vec<bool>,
}

pub fn device_label(name: &str) -> String {
    if name.is_empty() { "Unknown device".into() } else { name.to_string() }
}

pub fn hour_label(h: usize) -> String {
    match h {
        0 => "12 AM".to_string(),
        12 => "12 PM".to_string(),
        h if h < 12 => format!("{h} AM"),
        h => format!("{} PM", h - 12),
    }
}

fn share(n: i64, total: i64) -> String {
    let pct = n as f64 / total.max(1) as f64 * 100.0;
    if pct > 0.0 && pct < 0.5 { "<1%".to_string() } else { format!("{pct:.0}%") }
}

fn local_time(ms: i64) -> String {
    chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// Keeps a cell on one line and out of the way of the TSV separators.
fn cell(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One line describing what the tables cover.
pub fn heading(r: &Report) -> String {
    let device = match &r.device {
        Some(d) => device_label(d),
        None => "all devices".into(),
    };
    format!("Pasted statistics, last {} days, {device}", r.days)
}

pub fn tables(r: &Report) -> Vec<Table> {
    let days = r.days;
    let mut out = Vec::new();
    out.push(Table {
        title: "Summary".into(),
        headers: vec!["Measure", "Value"],
        rows: vec![
            vec![format!("Added in the last {days} days"), r.added.to_string()],
            vec![format!("Added in the {days} days before"), r.added_previous.to_string()],
            vec!["Pasted with Pasted (all time)".into(), r.pastes.to_string()],
            vec!["In history".into(), r.history_items.to_string()],
            vec!["Pinboards".into(), r.pinboards.to_string()],
            vec!["Storage (MB)".into(), format!("{:.1}", r.bytes as f64 / 1_048_576.0)],
        ],
        numeric: vec![false, true],
    });
    out.push(Table {
        title: "Items added per day".into(),
        headers: vec!["Date", "Day", "Items"],
        rows: r
            .per_day
            .iter()
            .map(|(d, n)| vec![d.format("%Y-%m-%d").to_string(), d.format("%a").to_string(), n.to_string()])
            .collect(),
        numeric: vec![false, false, true],
    });
    out.push(Table {
        title: format!("Time of day, last {days} days"),
        headers: vec!["Hour", "Items"],
        rows: (0..24)
            .map(|h| vec![format!("{} to {}", hour_label(h), hour_label((h + 1) % 24)), r.per_hour[h].to_string()])
            .collect(),
        numeric: vec![false, true],
    });
    let total: i64 = r.by_kind.iter().map(|(_, n)| n).sum();
    out.push(Table {
        title: format!("Types, last {days} days"),
        headers: vec!["Type", "Items", "Share"],
        rows: r
            .by_kind
            .iter()
            .map(|(k, n)| {
                let label = ItemKind::parse(k).map(|k| k.label().to_string()).unwrap_or_else(|| k.clone());
                vec![label, n.to_string(), share(*n, total)]
            })
            .collect(),
        numeric: vec![false, true, true],
    });
    // Every app, unlike the chart, which folds the tail into "Other apps".
    out.push(Table {
        title: format!("Copied from, last {days} days"),
        headers: vec!["App", "Bundle ID", "Items", "Share"],
        rows: r
            .top_apps
            .iter()
            .map(|(bundle, name, n)| vec![cell(name), bundle.clone(), n.to_string(), share(*n, r.added)])
            .collect(),
        numeric: vec![false, false, true, true],
    });
    out.push(Table {
        title: "Most pasted, all time".into(),
        headers: vec!["Item", "Times pasted"],
        // `summary` never reveals sensitive items.
        rows: r.most_pasted.iter().map(|i| vec![cell(&i.summary(80)), i.use_count.to_string()]).collect(),
        numeric: vec![false, true],
    });
    let total: i64 = r.devices.iter().map(|d| d.items).sum();
    out.push(Table {
        title: format!("Devices, last {days} days (all devices)"),
        headers: vec!["Device", "Items", "Share", "Top app", "Last copy"],
        rows: r
            .devices
            .iter()
            .map(|d| {
                vec![
                    cell(&device_label(&d.name)),
                    d.items.to_string(),
                    share(d.items, total),
                    d.top_app.as_deref().map(cell).unwrap_or_default(),
                    local_time(d.last_copy),
                ]
            })
            .collect(),
        numeric: vec![false, true, true, false, false],
    });
    out
}

/// Tab-separated values: a title line, a header row and the rows of each
/// table, with a blank line between tables.
pub fn to_tsv(heading: &str, tables: &[Table]) -> String {
    let mut out = format!("{heading}\n");
    for t in tables {
        out.push('\n');
        out.push_str(&t.title);
        out.push('\n');
        out.push_str(&t.headers.join("\t"));
        out.push('\n');
        for row in &t.rows {
            out.push_str(&row.join("\t"));
            out.push('\n');
        }
    }
    out
}

/// Aligned columns for reading on screen.
pub fn to_text(heading: &str, tables: &[Table]) -> String {
    let mut out = format!("{heading}\n");
    for t in tables {
        out.push('\n');
        out.push_str(&t.title);
        out.push('\n');
        let mut widths: Vec<usize> = t.headers.iter().map(|h| h.chars().count()).collect();
        for row in &t.rows {
            for (w, c) in widths.iter_mut().zip(row) {
                *w = (*w).max(c.chars().count());
            }
        }
        let line = |cells: &[String]| {
            let parts: Vec<String> = cells
                .iter()
                .zip(&widths)
                .zip(&t.numeric)
                .map(|((c, w), num)| if *num { format!("{c:>w$}") } else { format!("{c:<w$}") })
                .collect();
            format!("  {}\n", parts.join("   ").trim_end())
        };
        out.push_str(&line(&t.headers.iter().map(|h| h.to_string()).collect::<Vec<_>>()));
        if t.rows.is_empty() {
            out.push_str("  (none)\n");
        }
        for row in &t.rows {
            out.push_str(&line(row));
        }
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// HTML tables, so pasting into Notes, Pages or a document keeps the layout.
pub fn to_html(heading: &str, tables: &[Table]) -> String {
    let mut out = format!("<meta charset=\"utf-8\"><h3>{}</h3>", escape(heading));
    for t in tables {
        out.push_str(&format!("<h4>{}</h4><table><tr>", escape(&t.title)));
        for h in &t.headers {
            out.push_str(&format!("<th>{}</th>", escape(h)));
        }
        out.push_str("</tr>");
        for row in &t.rows {
            out.push_str("<tr>");
            for (c, num) in row.iter().zip(&t.numeric) {
                let align = if *num { " align=\"right\"" } else { "" };
                out.push_str(&format!("<td{align}>{}</td>", escape(c)));
            }
            out.push_str("</tr>");
        }
        out.push_str("</table>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Item, ItemMeta};
    use crate::store::DeviceStats;

    fn report() -> Report {
        let day = |d| chrono::NaiveDate::from_ymd_opt(2026, 10, d).unwrap();
        let mut per_hour = [0; 24];
        per_hour[9] = 4;
        let secret = Item {
            id: 1,
            kind: ItemKind::Text,
            title: None,
            text: "hunter2".into(),
            meta: ItemMeta::default(),
            app_bundle_id: None,
            app_name: None,
            device: None,
            created_at: 0,
            timestamp: 0,
            use_count: 3,
            size_bytes: 7,
            expires_at: None,
            sensitive: true,
        };
        let note = Item { id: 2, text: "line one\nline\ttwo".into(), sensitive: false, use_count: 2, ..secret.clone() };
        Report {
            days: 7,
            device: None,
            per_day: vec![(day(9), 1), (day(10), 3)],
            per_hour,
            by_kind: vec![("link".into(), 3), ("text".into(), 1)],
            top_apps: vec![("com.google.Chrome".into(), "Google Chrome".into(), 4)],
            added: 4,
            added_previous: 2,
            most_pasted: vec![secret, note],
            pastes: 5,
            history_items: 10,
            pinboards: 1,
            bytes: 1_048_576,
            devices: vec![DeviceStats { name: String::new(), items: 4, last_copy: 0, top_app: None }],
        }
    }

    #[test]
    fn tsv_has_exact_values_and_no_stray_separators() {
        let r = report();
        let tsv = to_tsv(&heading(&r), &tables(&r));
        assert!(tsv.starts_with("Pasted statistics, last 7 days, all devices\n"));
        assert!(tsv.contains("Date\tDay\tItems\n2026-10-09\tFri\t1\n2026-10-10\tSat\t3\n"));
        assert!(tsv.contains("9 AM to 10 AM\t4\n"));
        assert!(tsv.contains("Link\t3\t75%\n"));
        assert!(tsv.contains("Google Chrome\tcom.google.Chrome\t4\t100%\n"));
        assert!(tsv.contains("Storage (MB)\t1.0\n"));
        assert!(tsv.contains("line one line two\t2\n"), "cells stay on one line");
        assert!(tsv.contains("Unknown device\t4\t100%\t\t"));
        assert!(!tsv.contains("hunter2"), "sensitive items are never written out");
    }

    #[test]
    fn text_aligns_numbers_and_html_escapes() {
        let r = report();
        let t = tables(&r);
        let text = to_text(&heading(&r), &t);
        assert!(text.contains("  Type   Items   Share\n  Link       3     75%\n  Text       1     25%\n"), "{text}");
        let html = to_html("a < b", &t);
        assert!(html.contains("<h3>a &lt; b</h3>"));
        assert!(html.contains("<td align=\"right\">3</td>"));
    }
}
