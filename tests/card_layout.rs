//! Snapshot test for shelf card layout: builds a real card for each kind of
//! item and records every subview's class, frame and text. AppKit views
//! need the main thread, so this target runs without the test harness.
//!
//!   cargo test --test card_layout                       compare
//!   UPDATE_SNAPSHOTS=1 cargo test --test card_layout    rewrite after an intended change
//!
//! Text measurement follows the system font, so a macOS update can move
//! things by a point or two; review the diff, then update.

use std::path::Path;

use base64::Engine;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSTextField, NSView};
use objc2_foundation::MainThreadMarker;
use pasted::link_preview::LinkPreview;
use pasted::model::{
    ItemKind, ItemMeta, NewItem, Representation, SourceApp, UTI_FILE_URL, UTI_HTML, UTI_PLAIN_TEXT, UTI_PNG,
};
use pasted::store::Store;
use pasted::ui::card::{self, CARD_H, CARD_W, CardContent};
use pasted::ui::icons::IconCache;

const SNAPSHOT: &str = "tests/snapshots/card_layout.txt";

struct Case {
    name: &'static str,
    kind: ItemKind,
    text: &'static str,
    title: Option<&'static str>,
    reps: Vec<Representation>,
    meta: ItemMeta,
    link: Option<LinkPreview>,
    sensitive: bool,
    close_match: bool,
}

fn plain(text: &str) -> Vec<Representation> {
    vec![Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: text.as_bytes().to_vec() }]
}

fn case(name: &'static str, kind: ItemKind, text: &'static str) -> Case {
    Case {
        name,
        kind,
        text,
        title: None,
        reps: plain(text),
        meta: ItemMeta::default(),
        link: None,
        sensitive: false,
        close_match: false,
    }
}

fn cases() -> Vec<Case> {
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")
        .unwrap();
    let long = "The quarterly roadmap review moved to Thursday. Bring the updated estimates for search, \
                sync and the statistics window, plus open questions about distribution and notarization.";
    vec![
        case("short text", ItemKind::Text, "Hello"),
        case("long text", ItemKind::Text, long),
        Case { title: Some("Meeting notes"), ..case("titled text", ItemKind::Text, long) },
        Case { close_match: true, ..case("close match", ItemKind::Text, "Quarterly roadmap review") },
        Case {
            meta: ItemMeta { language: Some("rust".into()), ..Default::default() },
            ..case("code", ItemKind::Code, "fn main() {\n    let mut total = 0;\n    println!(\"{total}\");\n}")
        },
        Case {
            meta: ItemMeta { url: Some("https://github.com/rust-lang/rust".into()), ..Default::default() },
            ..case("link", ItemKind::Link, "https://github.com/rust-lang/rust")
        },
        Case {
            meta: ItemMeta { url: Some("https://www.rust-lang.org/".into()), ..Default::default() },
            link: Some(LinkPreview {
                url: "https://www.rust-lang.org/".into(),
                title: Some("Rust Programming Language".into()),
                description: Some("A language empowering everyone to build reliable and efficient software.".into()),
                image: None,
                icon: None,
            }),
            ..case("link with preview", ItemKind::Link, "https://www.rust-lang.org/")
        },
        Case {
            meta: ItemMeta { color: Some("#3B2DB5".into()), ..Default::default() },
            ..case("color", ItemKind::Color, "#3B2DB5")
        },
        Case {
            reps: vec![
                Representation { index: 0, uti: UTI_FILE_URL.into(), data: b"file:///tmp/Report.pdf".to_vec() },
                Representation { index: 1, uti: UTI_FILE_URL.into(), data: b"file:///tmp/Notes.txt".to_vec() },
            ],
            meta: ItemMeta { files: vec!["/tmp/Report.pdf".into(), "/tmp/Notes.txt".into()], ..Default::default() },
            ..case("files", ItemKind::File, "/tmp/Report.pdf\n/tmp/Notes.txt")
        },
        Case {
            reps: vec![Representation { index: 0, uti: UTI_PNG.into(), data: png }],
            meta: ItemMeta { width: Some(1), height: Some(1), ..Default::default() },
            ..case("image", ItemKind::Image, "")
        },
        Case {
            reps: vec![
                Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: b"Release notes: faster search".to_vec() },
                Representation {
                    index: 0,
                    uti: UTI_HTML.into(),
                    data: b"<h2>Release notes</h2><p><b>Faster</b> search</p>".to_vec(),
                },
            ],
            ..case("rich text", ItemKind::RichText, "Release notes: faster search")
        },
        Case { sensitive: true, ..case("sensitive", ItemKind::Text, "sk_live_0123456789abcdef") },
    ]
}

fn text_of(view: &NSView) -> Option<String> {
    let field = view.downcast_ref::<NSTextField>()?;
    let s: String = field.stringValue().to_string().replace('\n', "\\n").chars().take(48).collect();
    Some(s)
}

/// One line per view: depth, class, frame, then text for labels.
fn dump(view: &NSView, depth: usize, out: &mut Vec<String>, problems: &mut Vec<String>, name: &str) {
    for child in view.subviews().iter() {
        let f = child.frame();
        let class = child.class().name().to_string_lossy().into_owned();
        let mut line = format!(
            "{}{class} {:.1},{:.1} {:.1}x{:.1}",
            "  ".repeat(depth + 1),
            f.origin.x,
            f.origin.y,
            f.size.width,
            f.size.height
        );
        if child.isHidden() {
            line.push_str(" hidden");
        }
        if let Some(text) = text_of(&child) {
            line.push_str(&format!(" {text:?}"));
            if f.size.width <= 0.0 || f.size.height <= 0.0 {
                problems.push(format!("{name}: zero-size text {text:?}"));
            }
        }
        // Everything stays inside its parent (the card is the outermost parent).
        let parent = view.frame().size;
        let eps = 0.5;
        if f.origin.x < -eps
            || f.origin.y < -eps
            || f.origin.x + f.size.width > parent.width + eps
            || f.origin.y + f.size.height > parent.height + eps
        {
            problems.push(format!("{name}: {line} spills outside its {}x{} parent", parent.width, parent.height));
        }
        out.push(line);
        dump(&child, depth + 1, out, problems, name);
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let _app = NSApplication::sharedApplication(mtm);
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_with(&dir.path().join("t.db"), dir.path().join("blobs"), None).unwrap();
    let mut icons = IconCache::default();
    let now = pasted::model::now_ms();

    let mut out = vec![format!("# Card layout snapshot ({CARD_W}x{CARD_H}); regenerate with UPDATE_SNAPSHOTS=1")];
    let mut problems = Vec::new();
    for (i, c) in cases().into_iter().enumerate() {
        let new = NewItem {
            kind: c.kind,
            title: c.title.map(str::to_string),
            text: c.text.into(),
            meta: c.meta,
            representations: c.reps,
            // Finder is on every Mac, so the header icon is always found.
            source_app: Some(SourceApp { bundle_id: "com.apple.finder".into(), name: Some("Finder".into()) }),
            device: None,
            created_at: now,
            timestamp: now,
            external_id: None,
            in_history: true,
        };
        let id = store.insert(&new, false).unwrap().id();
        let mut item = store.get(id).unwrap().unwrap();
        item.sensitive = c.sensitive;
        let image = (item.kind == ItemKind::Image)
            .then(|| store.representation(id, UTI_PNG).ok().flatten())
            .flatten()
            .and_then(|d: Vec<u8>| card::image_from(&d));
        let rich = pasted::ui::rich_text::render(&store, &item, pasted::ui::rich_text::Target::Card, mtm);
        let content = CardContent {
            item: &item,
            index: i,
            image,
            link: c.link,
            rich,
            close_match: c.close_match,
            shortcut_number: Some(i % 9 + 1),
        };
        let view: Retained<card::CardView> = card::build(content, &mut icons, mtm);
        view.setFrame(objc2_foundation::NSRect::new(
            objc2_foundation::NSPoint::new(0.0, 0.0),
            objc2_foundation::NSSize::new(CARD_W, CARD_H),
        ));
        out.push(String::new());
        out.push(format!("[{}]", c.name));
        let start = out.len();
        dump(&view, 0, &mut out, &mut problems, c.name);
        if c.sensitive && out[start..].iter().any(|l| l.contains(c.text)) {
            problems.push(format!("{}: the hidden text appears on the card", c.name));
        }
    }
    let actual = out.join("\n") + "\n";

    if !problems.is_empty() {
        eprintln!("card layout problems:\n  {}", problems.join("\n  "));
        std::process::exit(1);
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        println!("card_layout: wrote {SNAPSHOT}");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    if expected == actual {
        println!("card_layout: {} cards match {SNAPSHOT}", cases().len());
        return;
    }
    eprintln!("card layout changed (run with UPDATE_SNAPSHOTS=1 if intended):");
    let (e, a): (Vec<_>, Vec<_>) = (expected.lines().collect(), actual.lines().collect());
    let mut section = "";
    for i in 0..e.len().max(a.len()) {
        let (el, al) = (e.get(i).copied().unwrap_or(""), a.get(i).copied().unwrap_or(""));
        if al.starts_with('[') {
            section = al;
        }
        if el != al {
            eprintln!("  {section}\n  - {el}\n  + {al}");
        }
    }
    std::process::exit(1);
}
