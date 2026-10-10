//! Storage and search benchmarks.
//!
//!   cargo bench --bench store
//!
//! - capture: inserting one copy into a 10,000 item history (what every
//!   clipboard change costs), and importing 10,000 items in one batch.
//! - search: the shelf's query (150 results, fuzzy fill) on 100,000 items.
//!   The budget is 20 ms per query; slower queries are reported as OVER.
//!
//! The 100,000 item database is built once and kept in
//! target/bench-data (delete it to rebuild).

use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use pasted::clipboard::classify::classify;
use pasted::model::{NewItem, Representation, SourceApp, UTI_PLAIN_TEXT};
use pasted::search::Query;
use pasted::store::Store;

const SEARCH_ITEMS: usize = 100_000;
const BUDGET: Duration = Duration::from_millis(20);
/// The shelf's page size.
const PAGE: usize = 150;

const WORDS: &[&str] = &[
    "roadmap",
    "quarterly",
    "review",
    "meeting",
    "notes",
    "search",
    "release",
    "invoice",
    "customer",
    "design",
    "budget",
    "deploy",
    "staging",
    "feedback",
    "summary",
    "draft",
    "contract",
    "schedule",
    "analytics",
    "report",
    "the",
    "and",
    "with",
    "for",
    "from",
    "about",
    "after",
    "before",
    "team",
    "project",
    "update",
    "issue",
    "fix",
    "login",
    "account",
    "password",
    "reset",
    "email",
    "address",
    "phone",
    "travel",
    "flight",
    "hotel",
    "dinner",
];
const APPS: &[(&str, &str)] = &[
    ("com.google.Chrome", "Google Chrome"),
    ("com.tinyspeck.slackmacgap", "Slack"),
    ("com.microsoft.VSCode", "Code"),
    ("com.apple.mail", "Mail"),
    ("dev.warp.Warp-Stable", "Warp"),
];

/// Small deterministic generator, so every run sees the same corpus.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.next() as usize % items.len()]
    }
}

/// A mix of sentences, links and code, like a real history.
fn item(rng: &mut Rng, n: usize, now: i64) -> NewItem {
    let text = match n % 10 {
        0..=5 => {
            let len = 4 + rng.next() as usize % 40;
            let mut words: Vec<&str> = (0..len).map(|_| *rng.pick(WORDS)).collect();
            words.push(if n.is_multiple_of(7) { "INV-" } else { "ref" });
            format!("{} {n}", words.join(" "))
        }
        6..=7 => format!("https://example{}.com/{}/{}?id={n}", rng.next() % 500, rng.pick(WORDS), rng.pick(WORDS)),
        _ => format!("fn {}_{n}() {{\n    let mut total = 0;\n    println!(\"{{total}}\");\n}}", rng.pick(WORDS)),
    };
    let reps = vec![Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: text.into_bytes() }];
    let c = classify(&reps).expect("text classifies");
    let (bundle, name) = *rng.pick(APPS);
    let ts = now - (n as i64) * 60_000;
    NewItem {
        kind: c.kind,
        title: None,
        text: c.text,
        meta: c.meta,
        representations: reps,
        source_app: Some(SourceApp { bundle_id: bundle.into(), name: Some(name.into()) }),
        device: Some("Bench Mac".into()),
        created_at: ts,
        timestamp: ts,
        external_id: None,
        in_history: true,
    }
}

fn open(dir: &Path) -> Store {
    std::fs::create_dir_all(dir).unwrap();
    Store::open_with(&dir.join("pasted.db"), dir.join("blobs"), None).unwrap()
}

fn fill(store: &mut Store, count: usize, seed: u64) {
    let now = pasted::model::now_ms();
    let mut rng = Rng(seed);
    store
        .batch(|s| {
            for n in 0..count {
                s.insert(&item(&mut rng, n, now), true)?;
            }
            Ok(())
        })
        .unwrap();
}

fn capture(c: &mut Criterion) {
    let mut group = c.benchmark_group("capture");
    group.sample_size(10);

    let dir = tempfile::tempdir().unwrap();
    let mut store = open(dir.path());
    fill(&mut store, 10_000, 1);
    let now = pasted::model::now_ms();
    let mut rng = Rng(99);
    let mut n = 1_000_000;
    group.bench_function("insert one into 10k history", |b| {
        b.iter(|| {
            n += 1;
            store.insert(&item(&mut rng, n, now), true).unwrap()
        })
    });

    group.bench_function("import 10k in one batch", |b| {
        b.iter_batched(
            || tempfile::tempdir().unwrap(),
            |dir| {
                let mut store = open(dir.path());
                fill(&mut store, 10_000, 2);
                dir
            },
            BatchSize::PerIteration,
        )
    });
    group.finish();
}

/// The cached 100k database, built on first use.
fn search_store() -> Store {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench-data/search-100k");
    let ready = dir.join("ready");
    if !ready.exists() {
        let _ = std::fs::remove_dir_all(&dir);
        let started = Instant::now();
        let mut store = open(&dir);
        fill(&mut store, SEARCH_ITEMS, 3);
        std::fs::write(&ready, b"").unwrap();
        eprintln!("built {SEARCH_ITEMS} item database in {:.1} s", started.elapsed().as_secs_f64());
    }
    let store = open(&dir);
    assert_eq!(store.stats().unwrap().items as usize, SEARCH_ITEMS, "stale bench database: delete {}", dir.display());
    store
}

fn search(c: &mut Criterion) {
    let store = search_store();
    let queries = [
        ("open shelf (no query)", ""),
        ("filter only", "app:Slack"),
        ("common word", "roadmap"),
        ("two words", "quarterly review"),
        ("rare match", "INV- 4242"),
        ("short (LIKE)", "fi"),
        ("type filter", "type:link example42"),
        ("app filter", "app:Slack budget"),
        ("no match, fuzzy fill", "quartely roadmep"),
    ];
    let mut group = c.benchmark_group("search 100k");
    group.sample_size(20);
    let mut over = Vec::new();
    for (name, text) in queries {
        let q = Query::parse(text);
        // Budget check on the median of a few runs, independent of criterion's report.
        let mut times: Vec<Duration> = (0..9)
            .map(|_| {
                let t = Instant::now();
                std::hint::black_box(store.search_with_fuzzy(&q, PAGE, 0, true).unwrap());
                t.elapsed()
            })
            .collect();
        times.sort();
        let median = times[times.len() / 2];
        let status = if median <= BUDGET { "ok" } else { "OVER" };
        eprintln!(
            "budget {status:>4}: {name:<22} {:>7.2} ms (median of 9, budget {} ms)",
            median.as_secs_f64() * 1000.0,
            BUDGET.as_millis()
        );
        if median > BUDGET {
            over.push(name);
        }
        group.bench_function(name, |b| b.iter(|| store.search_with_fuzzy(&q, PAGE, 0, true).unwrap()));
    }
    group.finish();
    if !over.is_empty() {
        eprintln!("search over the {} ms budget: {}", BUDGET.as_millis(), over.join(", "));
    }
}

criterion_group!(benches, capture, search);
criterion_main!(benches);
