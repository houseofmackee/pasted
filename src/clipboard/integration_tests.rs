//! Integration tests against real pasteboards: capture, storage and write
//! back. Each test uses its own uniquely named pasteboard, so the general
//! clipboard (and whatever the user copied) is never touched.

use std::ops::Deref;

use base64::Engine;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;

use super::capture::{Capture, capture};
use super::pasteboard::{self, BUNDLE_ID, SOURCE_TYPE};
use super::privacy::{IgnoreReason, PrivacyFilter};
use crate::model::{
    ItemKind, NewItem, Representation, UTI_FILE_URL, UTI_HTML, UTI_PLAIN_TEXT, UTI_PNG, UTI_RTF, UTI_UTF16_PLAIN_TEXT,
};
use crate::settings::Privacy;
use crate::store::tests::temp_store;

const MAX: usize = 64 * 1024 * 1024;

/// A private pasteboard, released when dropped.
struct Board(Retained<NSPasteboard>);

impl Board {
    fn new() -> Board {
        Board(NSPasteboard::pasteboardWithUniqueName())
    }
}

impl Deref for Board {
    type Target = NSPasteboard;
    fn deref(&self) -> &NSPasteboard {
        &self.0
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        // SAFETY: `releaseGlobally` takes no arguments and returns nothing.
        let _: () = unsafe { msg_send![&*self.0, releaseGlobally] };
    }
}

fn rep(index: u32, uti: &str, data: &[u8]) -> Representation {
    Representation { index, uti: uti.into(), data: data.to_vec() }
}

fn filter() -> PrivacyFilter {
    PrivacyFilter::new(&Privacy::default())
}

fn captured(board: &Board, filter: &PrivacyFilter) -> Box<NewItem> {
    match capture(board, filter, MAX, Some("Test Mac")) {
        Capture::Item(item) => item,
        _ => panic!("expected an item"),
    }
}

/// (index, uti, data) sorted, for comparing representation sets.
fn key(reps: &[Representation]) -> Vec<(u32, String, Vec<u8>)> {
    let mut v: Vec<_> = reps.iter().map(|r| (r.index, r.uti.clone(), r.data.clone())).collect();
    v.sort();
    v
}

fn rich_reps() -> Vec<Representation> {
    vec![
        rep(0, UTI_PLAIN_TEXT, b"Release notes"),
        rep(0, UTI_HTML, b"<b>Release</b> notes"),
        rep(0, UTI_RTF, br"{\rtf1\ansi {\b Release} notes}"),
    ]
}

#[test]
fn plain_text_round_trip_marks_the_source() {
    let board = Board::new();
    let before = board.changeCount();
    let change = pasteboard::write_text(&board, "hello pasteboard");
    assert!(change > before, "writing bumps the change count");

    let types = pasteboard::all_types(&board);
    assert!(types.contains(&UTI_PLAIN_TEXT.to_string()));
    assert!(types.contains(&SOURCE_TYPE.to_string()), "other clipboard tools can see who wrote it");
    let marker = board.stringForType(&NSString::from_str(SOURCE_TYPE)).map(|s| s.to_string());
    assert_eq!(marker.as_deref(), Some(BUNDLE_ID));

    let reps = pasteboard::read(&board, MAX).ok().unwrap();
    assert_eq!(key(&reps), key(&[rep(0, UTI_PLAIN_TEXT, b"hello pasteboard")]), "the marker is not stored");
    let item = captured(&board, &filter());
    assert_eq!(item.kind, ItemKind::Text);
    assert_eq!(item.text, "hello pasteboard");
    assert_eq!(item.device.as_deref(), Some("Test Mac"));
}

#[test]
fn rich_text_keeps_every_representation() {
    let board = Board::new();
    pasteboard::write(&board, &rich_reps());
    let item = captured(&board, &filter());
    assert_eq!(item.kind, ItemKind::RichText);
    assert_eq!(item.text, "Release notes");
    assert_eq!(key(&item.representations), key(&rich_reps()));
}

#[test]
fn utf16_copy_added_by_macos_is_not_stored() {
    let board = Board::new();
    pasteboard::write(&board, &rich_reps());
    // macOS offers a UTF-16 version of the text next to the formats we wrote.
    let types = pasteboard::all_types(&board);
    assert!(types.contains(&UTI_UTF16_PLAIN_TEXT.to_string()), "{types:?}");
    let item = captured(&board, &filter());
    assert!(item.representations.iter().all(|r| r.uti != UTI_UTF16_PLAIN_TEXT));

    // Pasting it back still offers UTF-16 to apps that ask for it.
    let (_dir, mut store) = temp_store();
    let id = store.insert(&item, true).unwrap().id();
    let target = Board::new();
    pasteboard::write(&target, &store.representations(id).unwrap());
    let utf16 = target.dataForType(&NSString::from_str(UTI_UTF16_PLAIN_TEXT)).map(|d| d.to_vec());
    let text = utf16.as_deref().and_then(crate::model::decode_utf16_external);
    assert_eq!(text.as_deref(), Some("Release notes"));
}

#[test]
fn multiple_pasteboard_items_keep_their_order() {
    let board = Board::new();
    let reps = [rep(0, UTI_FILE_URL, b"file:///tmp/a.txt"), rep(1, UTI_FILE_URL, b"file:///tmp/b%20c.txt")];
    pasteboard::write(&board, &reps);
    assert_eq!(board.pasteboardItems().map(|i| i.len()), Some(2));
    let item = captured(&board, &filter());
    assert_eq!(item.kind, ItemKind::File);
    assert_eq!(item.meta.files, ["/tmp/a.txt", "/tmp/b c.txt"]);
    assert_eq!(key(&item.representations), key(&reps));
}

#[test]
fn images_are_classified_with_their_size() {
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")
        .unwrap();
    let board = Board::new();
    pasteboard::write(&board, &[rep(0, UTI_PNG, &png)]);
    let item = captured(&board, &filter());
    assert_eq!(item.kind, ItemKind::Image);
    assert_eq!((item.meta.width, item.meta.height), (Some(1), Some(1)));
}

#[test]
fn concealed_content_is_skipped_or_kept_briefly() {
    let board = Board::new();
    pasteboard::write(
        &board,
        &[rep(0, UTI_PLAIN_TEXT, b"correct horse battery staple"), rep(0, "org.nspasteboard.ConcealedType", b"")],
    );
    assert!(matches!(capture(&board, &filter(), MAX, None), Capture::Ignored(IgnoreReason::Concealed(_))));
    let brief = PrivacyFilter::new(&Privacy { sensitive_expiry: "5m".into(), ..Privacy::default() });
    match capture(&board, &brief, MAX, None) {
        Capture::Sensitive(item, IgnoreReason::Concealed(_), ms) => {
            assert_eq!(ms, 5 * 60 * 1000);
            assert_eq!(item.text, "correct horse battery staple");
        }
        _ => panic!("expected a sensitive item"),
    }
}

#[test]
fn ignore_patterns_apply_to_captured_text() {
    let board = Board::new();
    pasteboard::write_text(&board, "ticket INTERNAL-42");
    let f = PrivacyFilter::new(&Privacy { ignore_patterns: vec!["INTERNAL-\\d+".into()], ..Privacy::default() });
    assert!(matches!(capture(&board, &f, MAX, None), Capture::Ignored(IgnoreReason::Pattern(_))));
}

#[test]
fn size_limit_and_empty_pasteboards() {
    let board = Board::new();
    pasteboard::write_text(&board, &"x".repeat(2000));
    assert!(matches!(capture(&board, &filter(), 1000, None), Capture::TooLarge(n) if n > 1000));
    board.clearContents();
    assert!(matches!(capture(&board, &filter(), MAX, None), Capture::Empty));
}

#[test]
fn stored_items_write_back_exactly() {
    let (_dir, mut store) = temp_store();
    let source = Board::new();
    pasteboard::write(&source, &rich_reps());
    let item = captured(&source, &filter());
    let id = store.insert(&item, true).unwrap().id();

    // What a paste does: put the stored representations on a pasteboard.
    let target = Board::new();
    pasteboard::write(&target, &store.representations(id).unwrap());
    let reread = pasteboard::read(&target, MAX).ok().unwrap();
    assert_eq!(key(&reread), key(&rich_reps()));

    // Copying the same content again moves the item instead of duplicating it.
    let again = captured(&target, &filter());
    assert_eq!(store.insert(&again, true).unwrap().id(), id);
    assert!(matches!(store.insert(&again, true).unwrap(), crate::store::Inserted::Existing(_)));
    assert_eq!(store.get(id).unwrap().unwrap().text, "Release notes");
}
