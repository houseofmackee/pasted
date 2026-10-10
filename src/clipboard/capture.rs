//! Builds a storable item from the current pasteboard contents.

use objc2_app_kit::NSPasteboard;

use super::classify::classify;
use super::pasteboard::{self, ReadError};
use super::privacy::{IgnoreReason, PrivacyFilter, Verdict};
use crate::model::{NewItem, SourceApp, now_ms};

pub enum Capture {
    Item(Box<NewItem>),
    /// Concealed or secret-looking content kept briefly (opt-in); expires
    /// after the given milliseconds.
    Sensitive(Box<NewItem>, IgnoreReason, i64),
    Ignored(IgnoreReason),
    TooLarge(usize),
    Empty,
}

pub fn capture(pb: &NSPasteboard, filter: &PrivacyFilter, max_bytes: usize, device: Option<&str>) -> Capture {
    let source_app: Option<SourceApp> = pasteboard::frontmost_app();
    let types = pasteboard::all_types(pb);
    // Checked before reading any data, so skipped content is never loaded.
    let mut sensitive =
        match filter.verdict(filter.check_types(&types, source_app.as_ref().map(|a| a.bundle_id.as_str()))) {
            Verdict::Save => None,
            Verdict::Ignore(reason) => return Capture::Ignored(reason),
            Verdict::KeepBriefly(reason, ms) => Some((reason, ms)),
        };
    let reps = match pasteboard::read(pb, max_bytes) {
        Ok(reps) => reps,
        Err(ReadError::TooLarge(n)) => return Capture::TooLarge(n),
        Err(ReadError::Empty) => return Capture::Empty,
    };
    let Some(c) = classify(&reps) else { return Capture::Empty };
    match filter.decide_text(&c.text) {
        Verdict::Save => {}
        Verdict::Ignore(reason) => return Capture::Ignored(reason),
        Verdict::KeepBriefly(reason, ms) => sensitive = sensitive.or(Some((reason, ms))),
    }
    let now = now_ms();
    let item = Box::new(NewItem {
        kind: c.kind,
        title: None,
        text: c.text,
        meta: c.meta,
        representations: reps,
        source_app,
        device: device.map(str::to_string),
        created_at: now,
        timestamp: now,
        external_id: None,
        in_history: true,
    });
    match sensitive {
        Some((reason, ms)) => Capture::Sensitive(item, reason, ms),
        None => Capture::Item(item),
    }
}
