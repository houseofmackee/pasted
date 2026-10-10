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
    /// Import from another clipboard manager.
    #[command(subcommand)]
    Import(ImportCommand),
    /// Delete history older than the retention setting and unused blob files.
    Prune,
    /// Fetch and print a link preview (debugging aid; nothing is stored).
    Preview { url: String },
    /// Show storage statistics.
    Stats,
    /// Print data file locations.
    Paths,
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
            let id = store
                .insert(
                    &NewItem {
                        kind: c.kind,
                        title: None,
                        text: c.text,
                        meta: c.meta,
                        representations: reps,
                        source_app: Some(SourceApp {
                            bundle_id: "io.pasted.cli".into(),
                            name: Some("Terminal".into()),
                        }),
                        device: None,
                        created_at: now,
                        timestamp: now,
                        external_id: None,
                        in_history: true,
                    },
                    true,
                )?
                .id();
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
        Command::Stats => {
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
