use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::clipboard::{classify::classify, pasteboard};
use crate::import::paste_app::{self, ImportOptions, PasteInstall};
use crate::model::*;
use crate::search::Query;
use crate::settings::Settings;
use crate::store::Store;
use crate::transform::Transform;

#[derive(Parser)]
#[command(name = "pasted", version, about = "A fast, private clipboard manager for macOS")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the menu bar app (the default).
    Run,
    /// Show the shelf of the running app.
    Open,
    /// Show the Settings window of the running app.
    Settings {
        /// Tab to show: general, shortcuts, privacy or data.
        #[arg(long)]
        tab: Option<String>,
    },
    /// List recent history, or a pinboard's items.
    List {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[arg(short, long)]
        board: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Search history. Supports type:, app:, after:, before: and board: filters.
    Search {
        query: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Print an item's text to stdout.
    Show { id: i64 },
    /// Put an item back on the clipboard.
    Copy {
        id: i64,
        /// Copy only the plain text.
        #[arg(long)]
        plain: bool,
        /// Copy with a cleanup applied, for example `--as snake` (see `pasted transform --list`).
        #[arg(long = "as", value_name = "TRANSFORM")]
        transform: Option<String>,
    },
    /// Expand snippet placeholders in stdin and print the result.
    Expand {
        /// Values for {input:Name} placeholders, as Name=value.
        #[arg(long = "set", value_name = "NAME=VALUE")]
        values: Vec<String>,
    },
    /// List rules, or test which rules match some text.
    Rules {
        #[command(subcommand)]
        command: Option<RulesCommand>,
    },
    /// Apply a cleanup to stdin and print the result.
    Transform {
        /// Transform name, for example `clean-links` or `pretty-json`.
        name: Option<String>,
        /// List available transforms.
        #[arg(long)]
        list: bool,
    },
    /// Save text from stdin (or arguments) as a new item.
    Add { text: Vec<String> },
    /// Delete an item.
    Delete { id: i64 },
    /// Set or clear an item's title.
    Rename { id: i64, title: Option<String> },
    /// Add an item to a pinboard (created if missing).
    Pin { id: i64, board: String },
    /// Remove an item from a pinboard.
    Unpin { id: i64, board: String },
    /// Manage pinboards.
    #[command(subcommand)]
    Board(BoardCommand),
    /// Export history: .zip (full backup), .json (no images or files) or .csv.
    Export {
        /// Destination file; the extension picks the format unless --format is given.
        path: Option<PathBuf>,
        #[arg(long, value_name = "zip|json|csv")]
        format: Option<String>,
        /// Only items on this pinboard.
        #[arg(long)]
        board: Option<String>,
        /// Only items matching a search, for example "type:link after:2026-09-01".
        #[arg(long)]
        query: Option<String>,
    },
    /// Import from another clipboard manager, or restore a backup.
    #[command(subcommand)]
    Import(ImportCommand),
    /// Delete history older than the retention setting and unused blob files.
    Prune,
    /// Fetch and print a link preview (debugging aid; nothing is stored).
    Preview { url: String },
    /// Encryption at rest: status, enable or disable (quit the app first).
    Encryption {
        #[command(subcommand)]
        command: Option<EncryptionCommand>,
    },
    /// Show statistics.
    Stats {
        /// Open the Statistics window of the running app instead.
        #[arg(long)]
        show: bool,
        /// Days to summarize.
        #[arg(long, default_value_t = 30)]
        days: i64,
    },
    /// Print data file locations.
    Paths,
}

#[derive(Subcommand)]
pub enum EncryptionCommand {
    Status,
    Enable,
    Disable,
}

#[derive(Subcommand)]
pub enum RulesCommand {
    /// Show which rules would match a copy of TEXT.
    Test {
        text: String,
        /// Source app bundle identifier or name.
        #[arg(long)]
        app: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum BoardCommand {
    List,
    Create { name: String },
    Rename { name: String, new_name: String },
    Delete { name: String },
}

#[derive(Subcommand)]
pub enum ImportCommand {
    /// Import history, pinboards and settings from Paste (pasteapp.io).
    Paste {
        /// Report what would be imported without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Paste data directory (default: auto-detect).
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        no_settings: bool,
        #[arg(long)]
        no_history: bool,
    },
    /// Restore a Pasted backup archive (.zip) made by `pasted export`.
    Archive { path: PathBuf },
}

fn print_items(items: &[Item], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(items)?);
        return Ok(());
    }
    for item in items {
        let app = item.app_name.as_deref().unwrap_or("");
        println!(
            "{:>6}  {:<9} {:<10} {:<14} {}",
            item.id,
            item.kind.as_str(),
            relative_time(item.timestamp),
            truncate_chars(app, 14),
            item.summary(80)
        );
    }
    Ok(())
}

const CLI_BUNDLE_ID: &str = "io.pasted.cli";
const CLI_APP_NAME: &str = "Command Line";

fn parse_transform(name: &str) -> Result<Transform> {
    Transform::parse(name).with_context(|| format!("unknown transform {name:?}; see `pasted transform --list`"))
}

fn board_id(store: &Store, name: &str, create: bool) -> Result<i64> {
    match store.pinboard_by_name(name)? {
        Some(id) => Ok(id),
        None if create => store.create_pinboard(name, None, None),
        None => bail!("no pinboard named {name:?}"),
    }
}

fn require(store: &Store, id: i64) -> Result<Item> {
    store.get(id)?.with_context(|| format!("no item with id {id}"))
}

pub fn run(command: Command) -> Result<()> {
    let mut store = Store::open_default()?;
    match command {
        Command::Run => unreachable!("handled in main"),
        Command::Open | Command::Settings { .. } => {
            let (name, object) = match &command {
                Command::Settings { tab } => (crate::app::SETTINGS_NOTIFICATION, tab.clone()),
                _ => (crate::app::OPEN_NOTIFICATION, None),
            };
            let name = objc2_foundation::NSString::from_str(name);
            let object = object.map(|o| objc2_foundation::NSString::from_str(&o));
            // SAFETY: a notification whose object is a string, as distributed notifications require.
            unsafe {
                objc2_foundation::NSDistributedNotificationCenter::defaultCenter()
                    .postNotificationName_object_userInfo_deliverImmediately(&name, object.as_deref(), None, true)
            };
        }
        Command::List { limit, board, json } => {
            let q = Query { board_name: board, ..Query::default() };
            print_items(&store.search(&q, limit, 0)?, json)?;
        }
        Command::Search { query, limit, json } => {
            print_items(&store.search(&Query::parse(&query.join(" ")), limit, 0)?, json)?;
        }
        Command::Show { id } => {
            let item = require(&store, id)?;
            println!("{}", item.text);
        }
        Command::Copy { id, plain, transform } => {
            let item = require(&store, id)?;
            let pb = pasteboard::general();
            if let Some(name) = transform {
                let t = parse_transform(&name)?;
                let out = t.apply(&item.text).with_context(|| format!("{} does not apply to item {id}", t.name()))?;
                let mut reps =
                    vec![Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: out.text.into_bytes() }];
                if let Some(html) = out.html {
                    reps.push(Representation { index: 0, uti: UTI_HTML.into(), data: html.into_bytes() });
                }
                pasteboard::write(&pb, &reps);
            } else if plain {
                pasteboard::write_text(&pb, &item.text);
            } else {
                pasteboard::write(&pb, &store.representations(id)?);
            }
            store.mark_used(id)?;
        }
        Command::Add { text } => {
            let text = if text.is_empty() {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            } else {
                text.join(" ")
            };
            if text.is_empty() {
                bail!("nothing to add");
            }
            let reps = vec![Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: text.into_bytes() }];
            let c = classify(&reps).context("could not classify input")?;
            let now = now_ms();
            let item = NewItem {
                kind: c.kind,
                title: None,
                text: c.text,
                meta: c.meta,
                representations: reps,
                source_app: Some(SourceApp { bundle_id: CLI_BUNDLE_ID.into(), name: Some(CLI_APP_NAME.into()) }),
                device: None,
                created_at: now,
                timestamp: now,
                external_id: None,
                in_history: true,
            };
            let id = store.insert(&item, true)?.id();
            let settings = Settings::load(&crate::paths::settings_path()?)?;
            let (rules, _) = crate::rules::Rules::new(&settings.rules);
            let outcome = rules.evaluate(item.kind, &item.text, Some(CLI_BUNDLE_ID), Some(CLI_APP_NAME));
            store.apply_rules(id, &outcome, now)?;
            println!("{id}");
        }
        Command::Expand { values } => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            let mut ctx = crate::snippet::Context::now();
            ctx.clipboard = pasteboard::general()
                .stringForType(&objc2_foundation::NSString::from_str(UTI_PLAIN_TEXT))
                .map(|s| s.to_string());
            for v in values {
                let (k, val) = v.split_once('=').with_context(|| format!("expected NAME=VALUE, got {v:?}"))?;
                ctx.inputs.insert(k.trim().to_string(), val.to_string());
            }
            for label in crate::snippet::inputs(&input) {
                if !ctx.inputs.contains_key(&label) {
                    eprintln!("note: no value for {{input:{label}}}; use --set \"{label}=...\"");
                }
            }
            print!("{}", crate::snippet::expand(&input, &ctx).text);
        }
        Command::Encryption { command } => {
            let (db, blobs) = (crate::paths::db_path()?, crate::paths::blobs_dir()?);
            match command.unwrap_or(EncryptionCommand::Status) {
                EncryptionCommand::Status => {
                    let on = crate::encryption::database_is_encrypted(&db);
                    println!("Encryption: {}", if on { "on" } else { "off" });
                    if on {
                        let key = crate::encryption::keychain_load()?.is_some();
                        println!("Key in Keychain: {}", if key { "yes" } else { "MISSING" });
                    }
                }
                cmd => {
                    // The app keeps the database open; switching needs it closed.
                    drop(store);
                    let lock = std::fs::File::create(crate::paths::data_dir()?.join("pasted.lock"))?;
                    if lock.try_lock().is_err() {
                        bail!("Pasted is running. Quit it first, or use Settings > Privacy > Encryption.");
                    }
                    let s = match cmd {
                        EncryptionCommand::Enable => crate::encryption::enable(&db, &blobs)?,
                        _ => crate::encryption::disable(&db, &blobs)?,
                    };
                    println!(
                        "Encryption is now {} ({} stored files converted).",
                        if crate::encryption::database_is_encrypted(&db) { "on" } else { "off" },
                        s.blobs
                    );
                    return Ok(());
                }
            }
        }
        Command::Rules { command } => {
            let settings = Settings::load(&crate::paths::settings_path()?)?;
            match command {
                None => {
                    if settings.rules.is_empty() {
                        println!("No rules. Add [[rules]] to the settings file or use Settings > Rules.");
                    }
                    for (i, r) in settings.rules.iter().enumerate() {
                        let status = match (r.enabled, r.validate()) {
                            (false, _) => "off".to_string(),
                            (true, Ok(())) => "on".to_string(),
                            (true, Err(e)) => format!("error: {e}"),
                        };
                        let name = if r.name.is_empty() { format!("Rule {}", i + 1) } else { r.name.clone() };
                        println!("{name} [{status}]\n    {}", r.summary());
                    }
                }
                Some(RulesCommand::Test { text, app }) => {
                    let (rules, errors) = crate::rules::Rules::new(&settings.rules);
                    for e in errors {
                        eprintln!("skipped {e}");
                    }
                    let reps =
                        vec![Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: text.clone().into_bytes() }];
                    let kind = classify(&reps).map(|c| c.kind).unwrap_or(ItemKind::Text);
                    let o = rules.evaluate(kind, &text, app.as_deref(), app.as_deref());
                    if o.matched.is_empty() {
                        println!("No rules match ({}).", kind.as_str());
                    } else {
                        println!("Matches ({}): {}", kind.as_str(), o.matched.join("; "));
                        if let Some(ms) = o.expire_after_ms {
                            let note = if o.pin_to.is_empty() { "" } else { " (ignored: pinned items never expire)" };
                            println!("Expires after {}{note}", crate::rules::format_duration(ms));
                        }
                        if !o.pin_to.is_empty() {
                            println!("Pinned to {}", o.pin_to.join(", "));
                        }
                    }
                }
            }
        }
        Command::Transform { name, list } => {
            if list || name.is_none() {
                for t in Transform::ALL {
                    println!("{:<14} {}", t.name(), t.label());
                }
                return Ok(());
            }
            let t = parse_transform(name.as_deref().unwrap_or_default())?;
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            let out = t.apply(&input).with_context(|| format!("{} does not apply to this input", t.name()))?;
            print!("{}", out.html.unwrap_or(out.text));
        }
        Command::Delete { id } => {
            if !store.delete(id)? {
                bail!("no item with id {id}");
            }
        }
        Command::Rename { id, title } => {
            require(&store, id)?;
            store.set_title(id, title.as_deref())?;
        }
        Command::Pin { id, board } => {
            require(&store, id)?;
            let board = board_id(&store, &board, true)?;
            store.pin(id, board, None)?;
        }
        Command::Unpin { id, board } => {
            let board = board_id(&store, &board, false)?;
            store.unpin(id, board)?;
        }
        Command::Board(cmd) => match cmd {
            BoardCommand::List => {
                for b in store.pinboards()? {
                    println!("{:>4}  #{:06X}  {:<24} {} items", b.id, b.color & 0xFFFFFF, b.name, b.item_count);
                }
            }
            BoardCommand::Create { name } => {
                println!("{}", store.create_pinboard(&name, None, None)?);
            }
            BoardCommand::Rename { name, new_name } => {
                let id = board_id(&store, &name, false)?;
                store.rename_pinboard(id, &new_name)?;
            }
            BoardCommand::Delete { name } => {
                let id = board_id(&store, &name, false)?;
                store.delete_pinboard(id)?;
            }
        },
        Command::Import(ImportCommand::Archive { path }) => {
            let summary = crate::export::restore(&mut store, &path)?;
            println!("{}", summary.describe());
        }
        Command::Export { path, format, board, query } => {
            let format = match (&format, &path) {
                (Some(f), _) => crate::export::Format::parse(f).with_context(|| format!("unknown format {f:?}"))?,
                (None, Some(p)) => crate::export::Format::from_path(p).unwrap_or(crate::export::Format::Zip),
                (None, None) => crate::export::Format::Zip,
            };
            let mut path = path.unwrap_or_else(|| crate::export::default_file_name(format));
            if crate::export::Format::from_path(&path) != Some(format) {
                path.set_extension(format.extension());
            }
            let filter = match (board, query) {
                (None, None) => None,
                (board, query) => {
                    let mut q = Query::parse(query.as_deref().unwrap_or(""));
                    if board.is_some() {
                        q.board_name = board;
                    }
                    Some(q)
                }
            };
            let summary = crate::export::export(&store, &path, format, filter.as_ref())?;
            println!("{}", summary.describe(&path));
        }
        Command::Import(ImportCommand::Paste { dry_run, path, no_settings, no_history }) => {
            let install = match path {
                Some(p) => PasteInstall::from_dir(&p)?,
                None => paste_app::detect()
                    .into_iter()
                    .next()
                    .context("no Paste installation found; pass --path to its Application Support directory")?,
            };
            eprintln!("Importing from {} ({})", install.data_dir.display(), install.bundle_id);
            if paste_app::is_running(&install) {
                eprintln!("Note: Paste is running. Quit it for the most complete import.");
            }
            let settings_path = crate::paths::settings_path()?;
            let mut settings = Settings::load(&settings_path)?;
            let opts = ImportOptions { dry_run, history: !no_history, settings: !no_settings };
            let report = paste_app::import(&install, &mut store, &mut settings, &opts)?;
            if !dry_run && !no_settings {
                settings.save(&settings_path)?;
            }
            if dry_run {
                println!("Dry run, nothing was changed.");
            }
            println!("{}", report.summary());
        }
        Command::Prune => {
            let settings = Settings::load(&crate::paths::settings_path()?)?;
            let items = store.prune(settings.general.history_retention, now_ms())?;
            let blobs = store.collect_garbage()?;
            println!("Removed {items} items and {blobs} blob files");
        }
        Command::Preview { url } => {
            let parsed = url::Url::parse(&url)?;
            if !crate::link_preview::is_fetchable(&parsed) {
                bail!("only public http(s) URLs are fetched");
            }
            let p = crate::link_preview::fetch(&url)?;
            println!("title:       {}", p.title.as_deref().unwrap_or("(none)"));
            println!("description: {}", p.description.as_deref().unwrap_or("(none)"));
            println!(
                "image:       {}",
                p.image.as_ref().map(|i| format!("{} bytes", i.len())).unwrap_or("(none)".into())
            );
            println!(
                "icon:        {}",
                p.icon.as_ref().map(|i| format!("{} bytes", i.len())).unwrap_or("(none)".into())
            );
        }
        Command::Stats { show: true, .. } => {
            let name = objc2_foundation::NSString::from_str(crate::app::STATS_NOTIFICATION);
            // SAFETY: plain notification without object or user info.
            unsafe {
                objc2_foundation::NSDistributedNotificationCenter::defaultCenter()
                    .postNotificationName_object_userInfo_deliverImmediately(&name, None, None, true)
            };
        }
        Command::Stats { days, .. } => {
            let r = store.report(days, now_ms())?;
            println!("Added in the last {days} days: {} ({} in the {days} days before)", r.added, r.added_previous);
            println!("Pasted with Pasted (all time): {}", r.pastes);
            let busiest = r.per_day.iter().max_by_key(|(_, n)| *n).filter(|(_, n)| *n > 0);
            if let Some((d, n)) = busiest {
                println!("Busiest day: {} ({n} items)", d.format("%a %b %-d"));
            }
            let s = store.stats()?;
            println!("Items: {} ({} in history)", s.items, s.history_items);
            println!("Pinboards: {}", s.pinboards);
            println!("Size: {:.1} MB", s.bytes as f64 / 1_048_576.0);
            println!("By type:");
            for (k, n) in &s.by_kind {
                println!("  {k:<10} {n}");
            }
            println!("Top apps:");
            for (a, n) in &s.top_apps {
                println!("  {a:<24} {n}");
            }
        }
        Command::Paths => {
            println!("data:     {}", crate::paths::data_dir()?.display());
            println!("database: {}", crate::paths::db_path()?.display());
            println!("settings: {}", crate::paths::settings_path()?.display());
        }
    }
    Ok(())
}
