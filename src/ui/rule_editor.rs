//! Modal editor for one rule.

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSButton, NSComboBox, NSControlStateValueOff, NSControlStateValueOn,
    NSPopUpButton, NSTextAlignment, NSTextField, NSView,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSString};

use super::{activate_app, label, ns, rect};
use crate::model::ItemKind;
use crate::rules::{Rule, parse_duration};

const EXPIRY_CHOICES: &[(&str, Option<&str>)] = &[
    ("Never", None),
    ("5 minutes", Some("5m")),
    ("15 minutes", Some("15m")),
    ("1 hour", Some("1h")),
    ("8 hours", Some("8h")),
    ("1 day", Some("1d")),
    ("1 week", Some("1w")),
];

struct Form {
    name: Retained<NSTextField>,
    app: Retained<NSTextField>,
    kinds: Vec<(ItemKind, Retained<NSButton>)>,
    pattern: Retained<NSTextField>,
    expiry: Retained<NSPopUpButton>,
    expiry_values: Vec<Option<String>>,
    pin_to: Retained<NSComboBox>,
    enabled: Retained<NSButton>,
}

fn field(placeholder: &str, value: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let f = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(0.0, 0.0, 290.0, 24.0));
    f.setPlaceholderString(Some(&ns(placeholder)));
    f.setStringValue(&ns(value));
    f
}

fn checkbox(title: &str, on: bool, mtm: MainThreadMarker) -> Retained<NSButton> {
    // SAFETY: no target or action; the checkbox just toggles.
    let b = unsafe { NSButton::checkboxWithTitle_target_action(&ns(title), None, None, mtm) };
    b.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    b
}

fn build(rule: &Rule, boards: &[String], mtm: MainThreadMarker) -> (Retained<NSView>, Form) {
    let (w, h) = (430.0, 300.0);
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, w, h));
    let x = 130.0;
    let mut y = h;
    let mut row = |title: &str, height: f64, control: &NSView| {
        y -= height + 8.0;
        let l = label(title, 13.0, false, mtm);
        l.setAlignment(NSTextAlignment::Right);
        l.setFrame(rect(0.0, y + height - 20.0, x - 8.0, 18.0));
        view.addSubview(&l);
        let mut f = control.frame();
        f.origin.x = x;
        f.origin.y = y;
        f.size.height = height;
        control.setFrame(f);
        view.addSubview(control);
    };

    let name = field("Optional", &rule.name, mtm);
    row("Name:", 24.0, &name);
    let app = field("Any app, or Terminal, com.apple.Safari, \u{2026}", rule.app.as_deref().unwrap_or(""), mtm);
    row("Copied from:", 24.0, &app);

    let kinds_view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 300.0, 44.0));
    let mut kinds = Vec::new();
    for (i, kind) in [
        ItemKind::Text,
        ItemKind::RichText,
        ItemKind::Code,
        ItemKind::Link,
        ItemKind::Image,
        ItemKind::File,
        ItemKind::Color,
    ]
    .into_iter()
    .enumerate()
    {
        let on = rule.kind.iter().any(|k| ItemKind::parse(k) == Some(kind));
        let b = checkbox(kind.label(), on, mtm);
        let (col, line) = (i % 4, i / 4);
        b.setFrame(rect(col as f64 * 78.0, 24.0 - line as f64 * 22.0, 78.0, 20.0));
        kinds_view.addSubview(&b);
        kinds.push((kind, b));
    }
    row("Types:", 44.0, &kinds_view);
    let hint = label("None checked means any type.", 11.0, false, mtm);
    hint.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
    row("", 14.0, &hint);

    let pattern = field("Regular expression, empty for any text", rule.matches.as_deref().unwrap_or(""), mtm);
    row("Text matches:", 24.0, &pattern);

    let expiry = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(0.0, 0.0, 160.0, 26.0), false);
    let mut expiry_values: Vec<Option<String>> = EXPIRY_CHOICES.iter().map(|(_, v)| v.map(str::to_string)).collect();
    let mut titles: Vec<String> = EXPIRY_CHOICES.iter().map(|(t, _)| t.to_string()).collect();
    let current = rule.expire_after.clone();
    let mut selected = 0;
    if let Some(cur) = &current {
        match EXPIRY_CHOICES.iter().position(|(_, v)| v.is_some_and(|v| parse_duration(v) == parse_duration(cur))) {
            Some(i) => selected = i,
            None => {
                titles.push(format!("Custom ({cur})"));
                expiry_values.push(Some(cur.clone()));
                selected = titles.len() - 1;
            }
        }
    }
    let ns_titles: Vec<Retained<NSString>> = titles.iter().map(|t| ns(t)).collect();
    expiry.addItemsWithTitles(&NSArray::from_retained_slice(&ns_titles));
    expiry.selectItemAtIndex(selected as isize);
    row("Remove after:", 26.0, &expiry);

    let pin_to = NSComboBox::initWithFrame(NSComboBox::alloc(mtm), rect(0.0, 0.0, 200.0, 26.0));
    for b in boards {
        // SAFETY: combo box items are plain strings.
        unsafe { pin_to.addItemWithObjectValue(&ns(b)) };
    }
    pin_to.setPlaceholderString(Some(&ns("No pinboard")));
    pin_to.setStringValue(&ns(rule.pin_to.as_deref().unwrap_or("")));
    row("Pin to:", 26.0, &pin_to);

    let enabled = checkbox("Rule is on", rule.enabled, mtm);
    row("", 20.0, &enabled);

    (view, Form { name, app, kinds, pattern, expiry, expiry_values, pin_to, enabled })
}

fn read(form: &Form) -> Rule {
    let text = |f: &NSTextField| {
        let s = f.stringValue().to_string();
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    };
    Rule {
        name: text(&form.name).unwrap_or_default(),
        enabled: form.enabled.state() == NSControlStateValueOn,
        app: text(&form.app),
        kind: form
            .kinds
            .iter()
            .filter(|(_, b)| b.state() == NSControlStateValueOn)
            .map(|(k, _)| k.as_str().to_string())
            .collect(),
        matches: text(&form.pattern),
        expire_after: form.expiry_values.get(form.expiry.indexOfSelectedItem().max(0) as usize).cloned().flatten(),
        pin_to: text(&form.pin_to),
    }
}

/// Shows the editor until the rule is valid or the user cancels.
pub fn edit_rule(initial: &Rule, title: &str, boards: &[String], mtm: MainThreadMarker) -> Option<Rule> {
    let mut rule = initial.clone();
    let mut problem: Option<String> = None;
    loop {
        let alert = NSAlert::new(mtm);
        // SAFETY: setting a valid image.
        unsafe { alert.setIcon(super::app_icon().as_deref()) };
        alert.setMessageText(&ns(title));
        alert.setInformativeText(&ns(problem.as_deref().unwrap_or(
            "New copies matching every condition are removed after the chosen time or pinned. Pinned items never expire.",
        )));
        alert.addButtonWithTitle(&ns("Save"));
        alert.addButtonWithTitle(&ns("Cancel"));
        let (view, form) = build(&rule, boards, mtm);
        alert.setAccessoryView(Some(&view));
        alert.window().setInitialFirstResponder(Some(&form.name));
        activate_app(mtm);
        if alert.runModal() != NSAlertFirstButtonReturn {
            return None;
        }
        rule = read(&form);
        match rule.validate() {
            Ok(()) => return Some(rule),
            Err(e) => problem = Some(format!("Please fix: {e}.")),
        }
    }
}
