//! Builds a storable item from the current pasteboard contents.

use objc2_app_kit::NSPasteboard;

use super::classify::classify;
use super::pasteboard::{self, ReadError};
use super::privacy::{IgnoreReason, PrivacyFilter};
use crate::model::{NewItem, SourceApp, now_ms};

pub enum Capture {
    Item(Box<NewItem>),
    Ignored(IgnoreReason),
    TooLarge(usize),
    Empty,
}

pub fn capture(pb: &NSPasteboard, filter: &PrivacyFilter, max_bytes: usize, device: Option<&str>) -> Capture {
    let source_app: Option<SourceApp> = pasteboard::frontmost_app();
    let types = pasteboard::all_types(pb);
    if let Some(reason) = filter.check_types(&types, source_app.as_ref().map(|a| a.bundle_id.as_str())) {
        return Capture::Ignored(reason);
    }
    let reps = match pasteboard::read(pb, max_bytes) {
        Ok(reps) => reps,
        Err(ReadError::TooLarge(n)) => return Capture::TooLarge(n),
        Err(ReadError::Empty) => return Capture::Empty,
    };
    let Some(c) = classify(&reps) else { return Capture::Empty };
    if let Some(reason) = filter.check_text(&c.text) {
        return Capture::Ignored(reason);
    }
    let now = now_ms();
    Capture::Item(Box::new(NewItem {
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
    }))
}
