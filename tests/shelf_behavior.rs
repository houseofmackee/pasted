//! Shelf behavior that needs real AppKit views (so the main thread, without
//! the test harness): smart pinboards and jumping from a search to an
//! item's place. Nothing is shown on screen.
//!
//!   cargo test --test shelf_behavior

use objc2_app_kit::NSApplication;
use objc2_foundation::MainThreadMarker;
use pasted::model::{NewItem, Representation, SourceApp, UTI_PLAIN_TEXT};
use pasted::store::Store;
use pasted::ui::shelf::Shelf;

fn item(text: &str, ts: i64) -> NewItem {
    let reps = vec![Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: text.as_bytes().to_vec() }];
    let c = pasted::clipboard::classify::classify(&reps).unwrap();
    NewItem {
        kind: c.kind,
        title: None,
        text: c.text,
        meta: c.meta,
        representations: reps,
        source_app: Some(SourceApp { bundle_id: "com.apple.Safari".into(), name: Some("Safari".into()) }),
        device: None,
        created_at: ts,
        timestamp: ts,
        external_id: None,
        in_history: true,
    }
}

fn check(ok: bool, what: &str, failures: &mut Vec<String>) {
    if !ok {
        failures.push(what.to_string());
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let _app = NSApplication::sharedApplication(mtm);
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_with(&dir.path().join("t.db"), dir.path().join("blobs"), None).unwrap();
    let now = pasted::model::now_ms();

    // 400 older notes push the first items past the shelf's first pages.
    let oldest = store.insert(&item("the very first note", now - 1_000_000), true).unwrap().id();
    store
        .batch(|s| {
            for n in 0..400 {
                s.insert(&item(&format!("filler note {n}"), now - 900_000 + n), true)?;
            }
            Ok(())
        })
        .unwrap();
    let rust = store.insert(&item("https://www.rust-lang.org/learn", now - 3_000), true).unwrap().id();
    let github = store.insert(&item("https://github.com/houseofmackee/pasted", now - 2_000), true).unwrap().id();
    let mut pinned_only = item("work snippet", now - 1_000);
    pinned_only.in_history = false;
    let snippet = store.insert(&pinned_only, false).unwrap().id();
    let work = store.create_pinboard("Work", None, None).unwrap();
    store.pin(snippet, work, None).unwrap();
    let links = store.create_smart_pinboard("Links", "type:link", None).unwrap();

    let target = pasted::app::Target::new(mtm);
    let mut shelf = Shelf::new(&target, mtm);
    let mut failures = Vec::new();
    let ids = |s: &Shelf| s.items.iter().map(|i| i.id).collect::<Vec<_>>();

    shelf.reload_tabs(&store);
    shelf.select_board(Some(links), &store);
    check(ids(&shelf) == [github, rust], "smart pinboard shows only links, newest first", &mut failures);
    check(shelf.in_smart_board(), "a smart pinboard is open", &mut failures);
    let placeholder = shelf.search.placeholderString().map(|p| p.to_string()).unwrap_or_default();
    check(
        placeholder.contains("Links") && placeholder.contains("type:link"),
        "placeholder shows the saved search",
        &mut failures,
    );

    shelf.set_query("rust", &store);
    check(ids(&shelf) == [rust], "typing narrows the smart pinboard", &mut failures);
    check(shelf.is_searching(), "typed text counts as a search", &mut failures);

    // Jump from the search to the item's place in history.
    check(shelf.jump_to(rust, None, &store), "jump to history succeeds", &mut failures);
    check(shelf.board.is_none() && !shelf.is_searching(), "jump leaves the search for history", &mut failures);
    check(shelf.selected_item().map(|i| i.id) == Some(rust), "the item is selected", &mut failures);
    check(shelf.search_text().is_empty(), "the search field is cleared", &mut failures);

    // Deep in history: pages load until the item is found.
    shelf.set_query("very first", &store);
    check(shelf.jump_to(oldest, None, &store), "jump to an old item succeeds", &mut failures);
    check(shelf.selected_item().map(|i| i.id) == Some(oldest), "the old item is selected", &mut failures);
    check(shelf.items.len() > 400, "earlier pages were loaded on the way", &mut failures);

    // Only on a pinboard: not in history, found in the pinboard.
    check(!shelf.jump_to(snippet, None, &store), "a pinned-only item is not in history", &mut failures);
    check(shelf.jump_to(snippet, Some(work), &store), "jump to the pinboard succeeds", &mut failures);
    check(
        shelf.board == Some(work) && shelf.selected_item().map(|i| i.id) == Some(snippet),
        "selected in its pinboard",
        &mut failures,
    );

    if failures.is_empty() {
        println!("shelf_behavior: all checks passed");
    } else {
        eprintln!("shelf_behavior failures:\n  {}", failures.join("\n  "));
        std::process::exit(1);
    }
}
