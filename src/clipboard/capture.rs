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

/// Removes tracking parameters from a copied link, so the clipboard itself
/// holds the clean link and a plain Cmd-V pastes it too. Only items that are
/// a single link are changed; their content becomes the clean URL as text
/// and as a URL, keeping the link's name. Returns whether anything changed.
pub fn strip_link_tracking(item: &mut NewItem) -> bool {
    use crate::model::{ItemKind, Representation, UTI_PLAIN_TEXT, UTI_URL, UTI_URL_NAME};
    if item.kind != ItemKind::Link {
        return false;
    }
    let Some(url) = item.meta.url.clone() else { return false };
    let clean = crate::transform::clean_url(&url);
    if clean == url {
        return false;
    }
    let mut reps = vec![
        Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: clean.clone().into_bytes() },
        Representation { index: 0, uti: UTI_URL.into(), data: clean.clone().into_bytes() },
    ];
    reps.extend(item.representations.iter().filter(|r| r.index == 0 && r.uti == UTI_URL_NAME).cloned());
    item.representations = reps;
    item.text = clean.clone();
    item.meta.url = Some(clean);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemKind, Representation, UTI_HTML, UTI_PLAIN_TEXT, UTI_URL, UTI_URL_NAME};

    fn item(reps: Vec<Representation>) -> NewItem {
        let c = classify(&reps).unwrap();
        NewItem {
            kind: c.kind,
            title: None,
            text: c.text,
            meta: c.meta,
            representations: reps,
            source_app: None,
            device: None,
            created_at: 0,
            timestamp: 0,
            external_id: None,
            in_history: true,
        }
    }

    fn rep(uti: &str, data: &str) -> Representation {
        Representation { index: 0, uti: uti.into(), data: data.as_bytes().to_vec() }
    }

    #[test]
    fn copied_links_lose_tracking_parameters() {
        let raw = "https://www.tomshardware.com/tech-industry/a-story?utm_medium=referral&utm_source=pushly&utm_campaign=All%20Push%20Subscribers";
        let mut it = item(vec![
            rep(UTI_PLAIN_TEXT, raw),
            rep(UTI_URL, raw),
            rep(UTI_URL_NAME, "A story"),
            rep(UTI_HTML, &format!("<a href=\"{raw}\">A story</a>")),
        ]);
        assert_eq!(it.kind, ItemKind::Link);
        assert!(strip_link_tracking(&mut it));
        let clean = "https://www.tomshardware.com/tech-industry/a-story";
        assert_eq!(it.text, clean);
        assert_eq!(it.meta.url.as_deref(), Some(clean));
        let utis: Vec<&str> = it.representations.iter().map(|r| r.uti.as_str()).collect();
        assert_eq!(utis, [UTI_PLAIN_TEXT, UTI_URL, UTI_URL_NAME]);
        assert!(it.representations.iter().all(|r| !String::from_utf8_lossy(&r.data).contains("utm_")));
    }

    #[test]
    fn other_items_are_left_alone() {
        let mut clean_link = item(vec![rep(UTI_PLAIN_TEXT, "https://example.com/?id=7")]);
        assert!(!strip_link_tracking(&mut clean_link));
        assert_eq!(clean_link.text, "https://example.com/?id=7");
        // Text that mentions a link is not rewritten on copy (only when pasted).
        let text = "see https://example.com/?utm_source=x";
        let mut prose = item(vec![rep(UTI_PLAIN_TEXT, text)]);
        assert!(!strip_link_tracking(&mut prose));
        assert_eq!(prose.text, text);
    }
}
