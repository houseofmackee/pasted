//! SQLite storage for items, representations, pinboards and search.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter, types::Value};
use sha2::{Digest, Sha256};

use crate::model::*;
use crate::search::Query;
use crate::settings::Retention;

/// Representations larger than this live in the blob directory.
const INLINE_LIMIT: usize = 32 * 1024;
/// Only the first part of very long text is indexed for search.
const FTS_TEXT_LIMIT: usize = 200_000;

const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE apps (
    id INTEGER PRIMARY KEY,
    bundle_id TEXT NOT NULL UNIQUE,
    name TEXT
);
CREATE TABLE items (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    title TEXT,
    text TEXT NOT NULL DEFAULT '',
    meta TEXT NOT NULL DEFAULT '{}',
    app_id INTEGER REFERENCES apps(id),
    device TEXT,
    created_at INTEGER NOT NULL,
    timestamp INTEGER NOT NULL,
    use_count INTEGER NOT NULL DEFAULT 0,
    last_used_at INTEGER,
    checksum TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    in_history INTEGER NOT NULL DEFAULT 1,
    external_id TEXT UNIQUE
);
CREATE INDEX items_timestamp ON items(timestamp DESC);
CREATE INDEX items_checksum ON items(checksum);
CREATE TABLE representations (
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    uti TEXT NOT NULL,
    data BLOB,
    blob TEXT,
    size INTEGER NOT NULL,
    PRIMARY KEY (item_id, idx, uti)
);
CREATE INDEX representations_blob ON representations(blob) WHERE blob IS NOT NULL;
CREATE TABLE pinboards (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    color INTEGER NOT NULL,
    sort INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    external_id TEXT UNIQUE
);
CREATE TABLE pinboard_items (
    pinboard_id INTEGER NOT NULL REFERENCES pinboards(id) ON DELETE CASCADE,
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    sort INTEGER NOT NULL,
    PRIMARY KEY (pinboard_id, item_id)
);
CREATE INDEX pinboard_items_item ON pinboard_items(item_id);
CREATE TABLE ocr (
    item_id INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
    text TEXT NOT NULL
);
CREATE VIRTUAL TABLE items_fts USING fts5(title, text, ocr, app, tokenize = 'trigram remove_diacritics 1');
CREATE TRIGGER items_fts_delete AFTER DELETE ON items BEGIN
    DELETE FROM items_fts WHERE rowid = old.id;
END;
CREATE TABLE import_runs (
    id INTEGER PRIMARY KEY,
    source TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    imported INTEGER NOT NULL DEFAULT 0,
    skipped INTEGER NOT NULL DEFAULT 0,
    failed INTEGER NOT NULL DEFAULT 0
);
"#,
    r#"
CREATE TABLE link_previews (
    url TEXT PRIMARY KEY,
    title TEXT,
    description TEXT,
    image BLOB,
    icon BLOB,
    status TEXT NOT NULL,
    fetched_at INTEGER NOT NULL
);
CREATE INDEX items_link_url ON items(json_extract(meta, '$.url')) WHERE kind = 'link';
"#,
    r#"
ALTER TABLE items ADD COLUMN expires_at INTEGER;
CREATE INDEX items_expires_at ON items(expires_at) WHERE expires_at IS NOT NULL;
"#,
    r#"
ALTER TABLE items ADD COLUMN sensitive INTEGER NOT NULL DEFAULT 0;
"#,
];

/// Failed link previews are retried after this long.
const PREVIEW_RETRY_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Pinboard colors (ARGB), similar to Paste's palette.
pub const PINBOARD_COLORS: &[u32] = &[
    0xFFFF3B30, 0xFFFF9500, 0xFFFFCC00, 0xFF34C759, 0xFF00C7BE, 0xFF007AFF, 0xFF5856D6, 0xFFAF52DE, 0xFFFF2D55,
    0xFF8E8E93,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inserted {
    New(i64),
    /// Same content was already in history; it was moved to the top.
    Existing(i64),
}

impl Inserted {
    pub fn id(self) -> i64 {
        match self {
            Inserted::New(id) | Inserted::Existing(id) => id,
        }
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Stats {
    pub items: i64,
    pub history_items: i64,
    pub pinboards: i64,
    pub bytes: i64,
    pub by_kind: Vec<(String, i64)>,
    pub top_apps: Vec<(String, i64)>,
}

/// Numbers for the statistics window, over the last `days` days.
#[derive(Debug, Default)]
pub struct Report {
    pub days: i64,
    /// The device the numbers are limited to, if any ("" for unknown).
    pub device: Option<String>,
    /// One entry per local day, oldest first, including empty days.
    pub per_day: Vec<(chrono::NaiveDate, i64)>,
    /// Items added per local hour of day.
    pub per_hour: [i64; 24],
    pub by_kind: Vec<(String, i64)>,
    /// Bundle id, display name, items added.
    pub top_apps: Vec<(String, String, i64)>,
    pub added: i64,
    /// Items added in the `days` before the range, for comparison.
    pub added_previous: i64,
    /// All time.
    pub most_pasted: Vec<Item>,
    pub pastes: i64,
    pub history_items: i64,
    pub pinboards: i64,
    pub bytes: i64,
    /// Items per device in the range, most first (never device-filtered).
    pub devices: Vec<DeviceStats>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceStats {
    /// "" when the device is unknown.
    pub name: String,
    pub items: i64,
    pub last_copy: i64,
    pub top_app: Option<String>,
}

/// Everything about an item except its representations, for export.
pub struct ExportRow {
    pub id: i64,
    pub kind: String,
    pub title: Option<String>,
    pub text: String,
    pub meta: String,
    pub url: Option<String>,
    pub app_bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub device: Option<String>,
    pub created_at: i64,
    pub timestamp: i64,
    pub use_count: i64,
    pub last_used_at: Option<i64>,
    pub in_history: bool,
    pub ocr: Option<String>,
    pub sensitive: bool,
}

pub struct Store {
    conn: Connection,
    blobs: PathBuf,
    /// Set when history is encrypted at rest.
    keys: Option<crate::encryption::Keys>,
}

pub fn checksum(reps: &[Representation]) -> String {
    let mut sorted: Vec<&Representation> = reps.iter().collect();
    sorted.sort_by(|a, b| (a.index, &a.uti).cmp(&(b.index, &b.uti)));
    let mut h = Sha256::new();
    for r in sorted {
        h.update(r.index.to_le_bytes());
        h.update((r.uti.len() as u64).to_le_bytes());
        h.update(r.uti.as_bytes());
        h.update((r.data.len() as u64).to_le_bytes());
        h.update(&r.data);
    }
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('%');
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

const ITEM_COLUMNS: &str = "i.id, i.kind, i.title, i.text, i.meta, a.bundle_id, a.name, i.device, \
     i.created_at, i.timestamp, i.use_count, i.size_bytes, i.expires_at, i.sensitive";

fn item_from_row(row: &Row) -> rusqlite::Result<Item> {
    let kind: String = row.get(1)?;
    let meta: String = row.get(4)?;
    Ok(Item {
        id: row.get(0)?,
        kind: ItemKind::parse(&kind).unwrap_or(ItemKind::Text),
        title: row.get(2)?,
        text: row.get(3)?,
        meta: serde_json::from_str(&meta).unwrap_or_default(),
        app_bundle_id: row.get(5)?,
        app_name: row.get(6)?,
        device: row.get(7)?,
        created_at: row.get(8)?,
        timestamp: row.get(9)?,
        use_count: row.get(10)?,
        size_bytes: row.get(11)?,
        expires_at: row.get(12)?,
        sensitive: row.get(13)?,
    })
}

impl Store {
    /// Opens the user's history, fetching the key from the Keychain when the
    /// database is encrypted.
    pub fn open_default() -> Result<Store> {
        let db = crate::paths::db_path()?;
        let keys =
            if crate::encryption::database_is_encrypted(&db) {
                Some(crate::encryption::keychain_load()?.context(
                    "history is encrypted, but its key is missing from the Keychain (it may have been deleted)",
                )?)
            } else {
                None
            };
        Store::open_with(&db, crate::paths::blobs_dir()?, keys)
    }

    #[cfg(test)]
    pub fn open(db: &Path, blobs: PathBuf) -> Result<Store> {
        Store::open_with(db, blobs, None)
    }

    pub fn open_with(db: &Path, blobs: PathBuf, keys: Option<crate::encryption::Keys>) -> Result<Store> {
        let conn = Connection::open(db).with_context(|| format!("opening {}", db.display()))?;
        if let Some(k) = &keys {
            k.apply(&conn)?;
        }
        // The first real query fails here if the key is wrong or missing.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))
            .with_context(|| format!("{} could not be read (encrypted with a different key?)", db.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // secure_delete overwrites deleted content, so expired or deleted clips
        // do not linger in free pages of the database file.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;",
        )?;
        std::fs::create_dir_all(&blobs)?;
        let mut store = Store { conn, blobs, keys };
        store.migrate()?;
        Ok(store)
    }

    /// An empty in-memory store, used while the real one is closed.
    pub fn placeholder() -> Result<Store> {
        let dir = std::env::temp_dir().join("pasted-placeholder-blobs");
        Store::open_with(Path::new(":memory:"), dir, None)
    }

    pub fn is_encrypted(&self) -> bool {
        self.keys.is_some()
    }

    fn read_blob(&self, name: &str) -> Result<Vec<u8>> {
        let raw = std::fs::read(self.blob_path(name)).with_context(|| format!("reading blob {name}"))?;
        if crate::encryption::is_sealed(&raw) {
            self.keys.as_ref().context("an encrypted file was found but no key is loaded")?.open(&raw)
        } else {
            Ok(raw)
        }
    }

    fn migrate(&mut self) -> Result<()> {
        let version: usize = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > MIGRATIONS.len() {
            bail!("database was created by a newer version of Pasted (schema {version})");
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql).with_context(|| format!("migration {}", i + 1))?;
            tx.pragma_update(None, "user_version", i + 1)?;
            tx.commit()?;
        }
        Ok(())
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.blobs.join(&hash[..2]).join(hash)
    }

    fn write_blob(&self, data: &[u8]) -> Result<String> {
        let name = match &self.keys {
            Some(k) => k.blob_name(data),
            None => hex(&Sha256::digest(data)),
        };
        let path = self.blob_path(&name);
        if !path.exists() {
            let content = match &self.keys {
                Some(k) => k.seal(data)?,
                None => data.to_vec(),
            };
            std::fs::create_dir_all(path.parent().unwrap())?;
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, content)?;
            std::fs::rename(&tmp, &path)?;
        }
        Ok(name)
    }

    fn upsert_app(&self, app: &SourceApp) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO apps (bundle_id, name) VALUES (?1, ?2)
             ON CONFLICT(bundle_id) DO UPDATE SET name = coalesce(excluded.name, apps.name)",
            params![app.bundle_id, app.name],
        )?;
        Ok(self.conn.query_row("SELECT id FROM apps WHERE bundle_id = ?1", [&app.bundle_id], |r| r.get(0))?)
    }

    fn reindex(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM items_fts WHERE rowid = ?1", [id])?;
        self.conn.execute(
            "INSERT INTO items_fts (rowid, title, text, ocr, app)
             SELECT i.id, coalesce(i.title, ''), substr(i.text, 1, ?2),
                    coalesce(o.text, '') || ' ' || coalesce(lp.title, '') || ' ' || coalesce(lp.description, ''),
                    coalesce(a.name, '')
             FROM items i
             LEFT JOIN ocr o ON o.item_id = i.id
             LEFT JOIN apps a ON a.id = i.app_id
             LEFT JOIN link_previews lp ON i.kind = 'link' AND lp.url = json_extract(i.meta, '$.url')
             WHERE i.id = ?1",
            params![id, FTS_TEXT_LIMIT as i64],
        )?;
        Ok(())
    }

    /// Stores a new item. With `dedupe`, identical content already in history
    /// is moved to the top instead of being stored twice.
    pub fn insert(&mut self, item: &NewItem, dedupe: bool) -> Result<Inserted> {
        let sum = checksum(&item.representations);
        if dedupe {
            let existing: Option<i64> = self
                .conn
                .query_row(
                    "SELECT id FROM items WHERE checksum = ?1 AND in_history = 1 ORDER BY timestamp DESC LIMIT 1",
                    [&sum],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = existing {
                self.conn.execute(
                    "UPDATE items SET timestamp = max(timestamp, ?2) WHERE id = ?1",
                    params![id, item.timestamp],
                )?;
                return Ok(Inserted::Existing(id));
            }
        }

        let mut blobs = Vec::with_capacity(item.representations.len());
        for r in &item.representations {
            blobs.push(if r.data.len() > INLINE_LIMIT { Some(self.write_blob(&r.data)?) } else { None });
        }
        let app_id = item.source_app.as_ref().map(|a| self.upsert_app(a)).transpose()?;
        let size: usize = item.representations.iter().map(|r| r.data.len()).sum();

        let tx = self.conn.savepoint()?;
        tx.execute(
            "INSERT INTO items (kind, title, text, meta, app_id, device, created_at, timestamp,
                                checksum, size_bytes, in_history, external_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                item.kind.as_str(),
                item.title,
                item.text,
                serde_json::to_string(&item.meta)?,
                app_id,
                item.device,
                item.created_at,
                item.timestamp,
                sum,
                size as i64,
                item.in_history,
                item.external_id,
            ],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO representations (item_id, idx, uti, data, blob, size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (r, blob) in item.representations.iter().zip(&blobs) {
                let inline = if blob.is_none() { Some(&r.data[..]) } else { None };
                stmt.execute(params![id, r.index, r.uti, inline, blob, r.data.len() as i64])?;
            }
        }
        tx.commit()?;
        self.reindex(id)?;
        Ok(Inserted::New(id))
    }

    pub fn get(&self, id: i64) -> Result<Option<Item>> {
        let sql = format!("SELECT {ITEM_COLUMNS} FROM items i LEFT JOIN apps a ON a.id = i.app_id WHERE i.id = ?1");
        Ok(self.conn.query_row(&sql, [id], item_from_row).optional()?)
    }

    pub fn representations(&self, id: i64) -> Result<Vec<Representation>> {
        let mut stmt = self
            .conn
            .prepare("SELECT idx, uti, data, blob FROM representations WHERE item_id = ?1 ORDER BY idx, rowid")?;
        let rows = stmt.query_map([id], |r| {
            Ok((
                r.get::<_, u32>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (index, uti, data, blob) = row?;
            let data = match (data, blob) {
                (Some(d), _) => d,
                (None, Some(hash)) => self.read_blob(&hash).with_context(|| format!("item {id}"))?,
                (None, None) => Vec::new(),
            };
            out.push(Representation { index, uti, data });
        }
        Ok(out)
    }

    /// Data for one type of the first pasteboard item, if present.
    pub fn representation(&self, id: i64, uti: &str) -> Result<Option<Vec<u8>>> {
        let row: Option<(Option<Vec<u8>>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT data, blob FROM representations WHERE item_id = ?1 AND uti = ?2 ORDER BY idx LIMIT 1",
                params![id, uti],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(match row {
            Some((Some(d), _)) => Some(d),
            Some((None, Some(hash))) => Some(self.read_blob(&hash)?),
            _ => None,
        })
    }

    /// Exact search: substring matches (trigram FTS, or LIKE for short
    /// words) plus filters, newest first (pinboard order on a board).
    pub fn search(&self, q: &Query, limit: usize, offset: usize) -> Result<Vec<Item>> {
        let Some((sql, args)) = self.search_sql(q, ITEM_COLUMNS, limit, offset)? else { return Ok(Vec::new()) };
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), item_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn search_ids(&self, q: &Query) -> Result<HashSet<i64>> {
        let Some((sql, args)) = self.search_sql(q, "i.id", i64::MAX as usize, 0)? else { return Ok(HashSet::new()) };
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Exact results first; when they run out (and `fuzzy` is on), close
    /// matches fill the rest of the page. Returns the items, whether more
    /// pages may follow, and how many items at the end are fuzzy matches.
    pub fn search_with_fuzzy(
        &self,
        q: &Query,
        limit: usize,
        offset: usize,
        fuzzy: bool,
    ) -> Result<(Vec<Item>, bool, usize)> {
        let mut items = self.search(q, limit, offset)?;
        if items.len() == limit || !fuzzy || q.text.trim().is_empty() {
            let more = items.len() == limit;
            return Ok((items, more, 0));
        }
        let exact = self.search_ids(q)?;
        let close = self.fuzzy_search(q, &exact, limit - items.len())?;
        let n = close.len();
        items.extend(close);
        Ok((items, false, n))
    }

    /// Items that fuzzily match `q.text` among the most recent candidates
    /// that pass `q`'s filters, best match first, skipping `exclude`.
    pub fn fuzzy_search(&self, q: &Query, exclude: &HashSet<i64>, limit: usize) -> Result<Vec<Item>> {
        const CANDIDATES: usize = 5_000;
        let Some(mut matcher) = crate::fuzzy::FuzzyQuery::new(&q.text) else { return Ok(Vec::new()) };
        let filters_only = Query { text: String::new(), ..q.clone() };
        let mut scored: Vec<(u32, Item)> = Vec::new();
        for item in self.search(&filters_only, CANDIDATES, 0)? {
            if exclude.contains(&item.id) || item.sensitive {
                continue;
            }
            let haystack = [
                item.title.as_deref().unwrap_or(""),
                item.meta.url_name.as_deref().unwrap_or(""),
                item.app_name.as_deref().unwrap_or(""),
                &item.text,
            ]
            .join(" ");
            if let Some(score) = matcher.score(&haystack) {
                scored.push((score, item));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.timestamp.cmp(&a.1.timestamp)));
        Ok(scored.into_iter().take(limit).map(|(_, i)| i).collect())
    }

    /// Builds the exact-search SQL. `None` when a named pinboard does not exist.
    fn search_sql(
        &self,
        q: &Query,
        columns: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Option<(String, Vec<Value>)>> {
        let mut clauses: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();

        let board = match (q.board_id, &q.board_name) {
            (Some(id), _) => Some(id),
            (None, Some(name)) => match self.pinboard_by_name(name)? {
                Some(id) => Some(id),
                None => return Ok(None),
            },
            _ => None,
        };
        match board {
            Some(id) => {
                clauses.push("i.id IN (SELECT item_id FROM pinboard_items WHERE pinboard_id = ?)".into());
                args.push(id.into());
            }
            None => clauses.push("i.in_history = 1".into()),
        }
        // Expired items are hidden immediately, before the sweep deletes them.
        clauses.push("(i.expires_at IS NULL OR i.expires_at > ?)".into());
        args.push(now_ms().into());
        if !q.kinds.is_empty() {
            clauses.push(format!("i.kind IN ({})", vec!["?"; q.kinds.len()].join(",")));
            args.extend(q.kinds.iter().map(|k| Value::from(k.as_str().to_string())));
        }
        if !q.exclude_kinds.is_empty() {
            clauses.push(format!("i.kind NOT IN ({})", vec!["?"; q.exclude_kinds.len()].join(",")));
            args.extend(q.exclude_kinds.iter().map(|k| Value::from(k.as_str().to_string())));
        }
        if !q.apps.is_empty() {
            let ors: Vec<&str> =
                q.apps.iter().map(|_| "(a.name LIKE ? ESCAPE '\\' OR a.bundle_id LIKE ? ESCAPE '\\')").collect();
            clauses.push(format!("({})", ors.join(" OR ")));
            for app in &q.apps {
                args.push(escape_like(app).into());
                args.push(escape_like(app).into());
            }
        }
        if let Some(ms) = q.after_ms {
            clauses.push("i.timestamp >= ?".into());
            args.push(ms.into());
        }
        if let Some(ms) = q.before_ms {
            clauses.push("i.timestamp < ?".into());
            args.push(ms.into());
        }

        // Trigram FTS needs at least 3 characters per term; shorter terms use LIKE.
        let mut fts_terms = Vec::new();
        for word in q.text.split_whitespace() {
            if word.chars().count() >= 3 {
                fts_terms.push(format!("\"{}\"", word.replace('"', "\"\"")));
            } else {
                clauses.push("(i.text LIKE ? ESCAPE '\\' OR coalesce(i.title, '') LIKE ? ESCAPE '\\')".into());
                args.push(escape_like(word).into());
                args.push(escape_like(word).into());
            }
        }
        if !fts_terms.is_empty() {
            clauses.push("i.id IN (SELECT rowid FROM items_fts WHERE items_fts MATCH ?)".into());
            args.push(fts_terms.join(" AND ").into());
        }

        let order = match board {
            Some(id) => {
                args.push(id.into());
                "(SELECT sort FROM pinboard_items WHERE pinboard_id = ? AND item_id = i.id), i.timestamp DESC"
            }
            None => "i.timestamp DESC",
        };
        args.push((limit.min(i64::MAX as usize) as i64).into());
        args.push((offset as i64).into());
        let sql = format!(
            "SELECT {columns} FROM items i LEFT JOIN apps a ON a.id = i.app_id
             WHERE {} ORDER BY {order} LIMIT ? OFFSET ?",
            clauses.join(" AND ")
        );
        Ok(Some((sql, args)))
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM items WHERE id = ?1", [id])? > 0)
    }

    /// Removes an item from history. Items still on a pinboard are kept there.
    pub fn remove_from_history(&self, id: i64) -> Result<()> {
        let pinned: bool =
            self.conn
                .query_row("SELECT EXISTS(SELECT 1 FROM pinboard_items WHERE item_id = ?1)", [id], |r| r.get(0))?;
        if pinned {
            self.conn.execute("UPDATE items SET in_history = 0 WHERE id = ?1", [id])?;
        } else {
            self.delete(id)?;
        }
        Ok(())
    }

    /// Deletes all history. Items that are also on a pinboard stay there.
    pub fn clear_history(&self) -> Result<usize> {
        let n = self
            .conn
            .execute("DELETE FROM items WHERE in_history = 1 AND id NOT IN (SELECT item_id FROM pinboard_items)", [])?;
        self.conn.execute("UPDATE items SET in_history = 0 WHERE in_history = 1", [])?;
        Ok(n)
    }

    pub fn set_title(&self, id: i64, title: Option<&str>) -> Result<()> {
        let title = title.map(str::trim).filter(|t| !t.is_empty());
        self.conn.execute("UPDATE items SET title = ?2 WHERE id = ?1", params![id, title])?;
        self.reindex(id)
    }

    /// Replaces an item's content with edited plain text.
    pub fn set_text(&mut self, id: i64, text: &str) -> Result<()> {
        let reps = vec![Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: text.as_bytes().to_vec() }];
        let c = crate::clipboard::classify::classify(&reps).expect("text always classifies");
        let tx = self.conn.savepoint()?;
        tx.execute("DELETE FROM representations WHERE item_id = ?1", [id])?;
        tx.execute(
            "INSERT INTO representations (item_id, idx, uti, data, size) VALUES (?1, 0, ?2, ?3, ?4)",
            params![id, UTI_PLAIN_TEXT, text.as_bytes(), text.len() as i64],
        )?;
        tx.execute(
            "UPDATE items SET kind = ?2, text = ?3, meta = ?4, checksum = ?5, size_bytes = ?6 WHERE id = ?1",
            params![id, c.kind.as_str(), c.text, serde_json::to_string(&c.meta)?, checksum(&reps), text.len() as i64],
        )?;
        tx.commit()?;
        self.reindex(id)
    }

    /// Records a paste: bumps the use count and moves the item to the top of history.
    pub fn mark_used(&self, id: i64) -> Result<()> {
        let now = now_ms();
        self.conn.execute(
            "UPDATE items SET use_count = use_count + 1, last_used_at = ?2,
                              timestamp = CASE WHEN in_history = 1 THEN ?2 ELSE timestamp END
             WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    pub fn set_ocr(&self, id: i64, text: &str) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO ocr (item_id, text) VALUES (?1, ?2)", params![id, text])?;
        self.reindex(id)
    }

    /// Image items that have not been through OCR yet, newest first.
    pub fn items_needing_ocr(&self, limit: usize) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM items WHERE kind = 'image' AND id NOT IN (SELECT item_id FROM ocr)
             ORDER BY timestamp DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // Link previews

    /// Link URLs without a preview (or whose last attempt failed long ago), newest first.
    pub fn links_needing_preview(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT url FROM (
                 SELECT json_extract(meta, '$.url') AS url, max(timestamp) AS ts
                 FROM items WHERE kind = 'link' AND sensitive = 0 GROUP BY 1
             )
             WHERE url IS NOT NULL
               AND url NOT IN (SELECT url FROM link_previews WHERE status = 'ok' OR fetched_at > ?1)
             ORDER BY ts DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![now_ms() - PREVIEW_RETRY_MS, limit as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_link_preview(&self, p: &crate::link_preview::LinkPreview, status: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO link_previews (url, title, description, image, icon, status, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![p.url, p.title, p.description, p.image, p.icon, status, now_ms()],
        )?;
        self.reindex_url(&p.url)
    }

    /// Stores a preview from an import unless one already exists.
    pub fn import_link_preview(&self, p: &crate::link_preview::LinkPreview) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO link_previews (url, title, description, image, icon, status, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'ok', ?6)",
            params![p.url, p.title, p.description, p.image, p.icon, now_ms()],
        )?;
        if n > 0 {
            self.reindex_url(&p.url)?;
        }
        Ok(n > 0)
    }

    fn reindex_url(&self, url: &str) -> Result<()> {
        let mut stmt =
            self.conn.prepare("SELECT id FROM items WHERE kind = 'link' AND json_extract(meta, '$.url') = ?1")?;
        let ids: Vec<i64> = stmt.query_map([url], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for id in ids {
            self.reindex(id)?;
        }
        Ok(())
    }

    pub fn link_preview(&self, url: &str) -> Result<Option<crate::link_preview::LinkPreview>> {
        Ok(self
            .conn
            .query_row(
                "SELECT url, title, description, image, icon FROM link_previews WHERE url = ?1 AND status = 'ok'",
                [url],
                |r| {
                    Ok(crate::link_preview::LinkPreview {
                        url: r.get(0)?,
                        title: r.get(1)?,
                        description: r.get(2)?,
                        image: r.get(3)?,
                        icon: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn item_by_external_id(&self, external_id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT id FROM items WHERE external_id = ?1", [external_id], |r| r.get(0))
            .optional()?)
    }

    // Pinboards

    pub fn pinboards(&self) -> Result<Vec<Pinboard>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, p.color, (SELECT count(*) FROM pinboard_items pi WHERE pi.pinboard_id = p.id)
             FROM pinboards p ORDER BY p.sort, p.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Pinboard { id: r.get(0)?, name: r.get(1)?, color: r.get::<_, i64>(2)? as u32, item_count: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn pinboard_by_name(&self, name: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT id FROM pinboards WHERE name = ?1 COLLATE NOCASE ORDER BY sort LIMIT 1", [name], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn pinboard_by_external_id(&self, external_id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT id FROM pinboards WHERE external_id = ?1", [external_id], |r| r.get(0))
            .optional()?)
    }

    pub fn create_pinboard(&self, name: &str, color: Option<u32>, external_id: Option<&str>) -> Result<i64> {
        let count: i64 = self.conn.query_row("SELECT count(*) FROM pinboards", [], |r| r.get(0))?;
        let color = color.unwrap_or(PINBOARD_COLORS[count as usize % PINBOARD_COLORS.len()]);
        self.conn.execute(
            "INSERT INTO pinboards (name, color, sort, created_at, external_id)
             VALUES (?1, ?2, (SELECT coalesce(max(sort), 0) + 1 FROM pinboards), ?3, ?4)",
            params![name.trim(), color as i64, now_ms(), external_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_pinboard(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute("UPDATE pinboards SET name = ?2 WHERE id = ?1", params![id, name.trim()])?;
        Ok(())
    }

    pub fn set_pinboard_color(&self, id: i64, color: u32) -> Result<()> {
        self.conn.execute("UPDATE pinboards SET color = ?2 WHERE id = ?1", params![id, color as i64])?;
        Ok(())
    }

    /// Deletes a pinboard. Items that only lived on it are deleted too.
    pub fn delete_pinboard(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM items WHERE in_history = 0 AND id IN
                (SELECT item_id FROM pinboard_items WHERE pinboard_id = ?1)
             AND id NOT IN (SELECT item_id FROM pinboard_items WHERE pinboard_id != ?1)",
            [id],
        )?;
        self.conn.execute("DELETE FROM pinboards WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn pin(&self, item_id: i64, pinboard_id: i64, sort: Option<i64>) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO pinboard_items (pinboard_id, item_id, sort)
             VALUES (?1, ?2, coalesce(?3, (SELECT coalesce(min(sort), 0) - 1 FROM pinboard_items WHERE pinboard_id = ?1)))",
            params![pinboard_id, item_id, sort],
        )?;
        // Pinned items are kept on purpose; they never expire.
        self.conn.execute("UPDATE items SET expires_at = NULL WHERE id = ?1", [item_id])?;
        Ok(())
    }

    /// Applies what matching rules asked for: pin first (which also keeps the
    /// item), otherwise set the expiry. Returns the pinboards it was pinned to.
    pub fn apply_rules(&self, item_id: i64, outcome: &crate::rules::Outcome, now: i64) -> Result<Vec<String>> {
        for name in &outcome.pin_to {
            let board = match self.pinboard_by_name(name)? {
                Some(id) => id,
                None => self.create_pinboard(name, None, None)?,
            };
            self.pin(item_id, board, None)?;
        }
        if outcome.pin_to.is_empty()
            && let Some(ms) = outcome.expire_after_ms
            && self.item_pinboards(item_id)?.is_empty()
        {
            self.conn.execute("UPDATE items SET expires_at = ?2 WHERE id = ?1", params![item_id, now + ms])?;
        }
        Ok(outcome.pin_to.clone())
    }

    /// Deletes history items whose rule-based expiry has passed.
    pub fn expire_due(&self, now: i64) -> Result<usize> {
        let sensitive: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE sensitive = 1 AND expires_at IS NOT NULL AND expires_at <= ?1)",
            [now],
            |r| r.get(0),
        )?;
        let n = self.conn.execute(
            "DELETE FROM items WHERE expires_at IS NOT NULL AND expires_at <= ?1
             AND id NOT IN (SELECT item_id FROM pinboard_items)",
            [now],
        )?;
        if sensitive {
            // Move the deletions out of the write-ahead log too.
            self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            self.collect_garbage()?;
        }
        Ok(n)
    }

    /// Marks an item sensitive and schedules its removal. A re-copy of the
    /// same content refreshes the deadline.
    pub fn mark_sensitive(&self, item_id: i64, expires_at: i64) -> Result<()> {
        self.conn
            .execute("UPDATE items SET sensitive = 1, expires_at = ?2 WHERE id = ?1", params![item_id, expires_at])?;
        Ok(())
    }

    pub fn unpin(&self, item_id: i64, pinboard_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM pinboard_items WHERE pinboard_id = ?1 AND item_id = ?2",
            params![pinboard_id, item_id],
        )?;
        // An item that is in neither history nor any pinboard is garbage.
        self.conn.execute(
            "DELETE FROM items WHERE id = ?1 AND in_history = 0
             AND NOT EXISTS (SELECT 1 FROM pinboard_items WHERE item_id = ?1)",
            [item_id],
        )?;
        Ok(())
    }

    pub fn item_pinboards(&self, item_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare("SELECT pinboard_id FROM pinboard_items WHERE item_id = ?1")?;
        let rows = stmt.query_map([item_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // Maintenance

    /// Deletes history older than the retention window. Pinned items are kept.
    pub fn prune(&self, retention: Retention, now: i64) -> Result<usize> {
        let mut removed = 0;
        if let Some(age) = retention.max_age_ms() {
            let cutoff = now - age;
            removed += self.conn.execute(
                "DELETE FROM items WHERE in_history = 1 AND timestamp < ?1
                 AND id NOT IN (SELECT item_id FROM pinboard_items)",
                [cutoff],
            )?;
            self.conn.execute("UPDATE items SET in_history = 0 WHERE in_history = 1 AND timestamp < ?1", [cutoff])?;
        }
        Ok(removed)
    }

    /// Deletes blob files no longer referenced by any representation.
    pub fn collect_garbage(&self) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT blob FROM representations WHERE blob IS NOT NULL")?;
        let used: HashSet<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut removed = 0;
        let Ok(dirs) = std::fs::read_dir(&self.blobs) else { return Ok(0) };
        for dir in dirs.flatten() {
            let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                if !used.contains(&name) && std::fs::remove_file(file.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    pub fn stats(&self) -> Result<Stats> {
        let one = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        let pairs = |sql: &str| -> Result<Vec<(String, i64)>> {
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        Ok(Stats {
            items: one("SELECT count(*) FROM items")?,
            history_items: one("SELECT count(*) FROM items WHERE in_history = 1")?,
            pinboards: one("SELECT count(*) FROM pinboards")?,
            bytes: one("SELECT coalesce(sum(size_bytes), 0) FROM items")?,
            by_kind: pairs("SELECT kind, count(*) FROM items GROUP BY kind ORDER BY 2 DESC")?,
            top_apps: pairs(
                "SELECT coalesce(a.name, a.bundle_id), count(*) FROM items i JOIN apps a ON a.id = i.app_id
                 GROUP BY a.id ORDER BY 2 DESC LIMIT 10",
            )?,
        })
    }

    /// Statistics over the last `days` days. With `device`, every number is
    /// limited to items copied on that device ("" means unknown device).
    pub fn report(&self, days: i64, now: i64, device: Option<&str>) -> Result<Report> {
        use chrono::{Local, TimeZone};
        use rusqlite::named_params;
        let days = days.max(1);
        let today = Local
            .timestamp_millis_opt(now)
            .single()
            .map(|d| d.date_naive())
            .unwrap_or_else(|| Local::now().date_naive());
        let first = today - chrono::Duration::days(days - 1);
        let midnight = |d: chrono::NaiveDate| {
            Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).earliest().map(|t| t.timestamp_millis())
        };
        let since = midnight(first).unwrap_or(now - days * 86_400_000);
        let prev_since = midnight(first - chrono::Duration::days(days)).unwrap_or(since - days * 86_400_000);
        // Every query below filters on this; NULL means all devices.
        const DEV: &str = "(:dev IS NULL OR coalesce(i.device, '') = :dev)";

        let mut counts = std::collections::HashMap::new();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT date(i.created_at / 1000, 'unixepoch', 'localtime'), count(*) FROM items i
             WHERE i.created_at >= :since AND {DEV} GROUP BY 1"
        ))?;
        let rows = stmt.query_map(named_params! { ":since": since, ":dev": device }, |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (day, n) = row?;
            if let Ok(d) = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d") {
                counts.insert(d, n);
            }
        }
        let per_day =
            (0..days).map(|i| first + chrono::Duration::days(i)).map(|d| (d, *counts.get(&d).unwrap_or(&0))).collect();

        let mut per_hour = [0i64; 24];
        let mut stmt = self.conn.prepare(&format!(
            "SELECT CAST(strftime('%H', i.created_at / 1000, 'unixepoch', 'localtime') AS INTEGER), count(*)
             FROM items i WHERE i.created_at >= :since AND {DEV} GROUP BY 1"
        ))?;
        let rows = stmt.query_map(named_params! { ":since": since, ":dev": device }, |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (h, n) = row?;
            if (0..24).contains(&h) {
                per_hour[h as usize] = n;
            }
        }

        let mut stmt = self.conn.prepare(&format!(
            "SELECT i.kind, count(*) FROM items i WHERE i.created_at >= :since AND {DEV}
             GROUP BY i.kind ORDER BY 2 DESC"
        ))?;
        let by_kind = stmt
            .query_map(named_params! { ":since": since, ":dev": device }, |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT a.bundle_id, coalesce(a.name, a.bundle_id), count(*) FROM items i JOIN apps a ON a.id = i.app_id
             WHERE i.created_at >= :since AND {DEV} GROUP BY a.id ORDER BY 3 DESC LIMIT 8"
        ))?;
        let top_apps = stmt
            .query_map(named_params! { ":since": since, ":dev": device }, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;

        let count = |sql: &str, from: i64, to: i64| -> Result<i64> {
            Ok(self.conn.query_row(
                &format!("SELECT {sql} FROM items i WHERE i.created_at >= :from AND i.created_at < :to AND {DEV}"),
                named_params! { ":from": from, ":to": to, ":dev": device },
                |r| r.get(0),
            )?)
        };
        let added = count("count(*)", since, i64::MAX)?;
        let added_previous = count("count(*)", prev_since, since)?;
        let pastes = count("coalesce(sum(i.use_count), 0)", i64::MIN, i64::MAX)?;
        let bytes = count("coalesce(sum(i.size_bytes), 0)", i64::MIN, i64::MAX)?;
        let history_items: i64 = self.conn.query_row(
            &format!("SELECT count(*) FROM items i WHERE i.in_history = 1 AND {DEV}"),
            named_params! { ":dev": device },
            |r| r.get(0),
        )?;
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM items i LEFT JOIN apps a ON a.id = i.app_id
             WHERE i.use_count > 0 AND i.sensitive = 0 AND {DEV}
             ORDER BY i.use_count DESC, i.last_used_at DESC LIMIT 8"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let most_pasted =
            stmt.query_map(named_params! { ":dev": device }, item_from_row)?.collect::<rusqlite::Result<_>>()?;

        // The per-device breakdown is never filtered by device.
        let mut stmt = self.conn.prepare(
            "SELECT coalesce(i.device, ''), count(*), max(i.timestamp),
                    (SELECT coalesce(a.name, a.bundle_id) FROM items j JOIN apps a ON a.id = j.app_id
                     WHERE coalesce(j.device, '') = coalesce(i.device, '') AND j.created_at >= :since
                     GROUP BY a.id ORDER BY count(*) DESC LIMIT 1)
             FROM items i WHERE i.created_at >= :since GROUP BY 1 ORDER BY 2 DESC",
        )?;
        let devices = stmt
            .query_map(named_params! { ":since": since }, |r| {
                Ok(DeviceStats { name: r.get(0)?, items: r.get(1)?, last_copy: r.get(2)?, top_app: r.get(3)? })
            })?
            .collect::<rusqlite::Result<_>>()?;

        Ok(Report {
            days,
            device: device.map(str::to_string),
            per_day,
            per_hour,
            by_kind,
            top_apps,
            added,
            added_previous,
            most_pasted,
            pastes,
            history_items,
            pinboards: self.stats()?.pinboards,
            bytes,
            devices,
        })
    }

    /// Every device name ever recorded ("" for unknown), most items first.
    pub fn device_names(&self) -> Result<Vec<(String, i64)>> {
        let mut stmt =
            self.conn.prepare("SELECT coalesce(device, ''), count(*) FROM items GROUP BY 1 ORDER BY 2 DESC")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// All items, newest first, for export.
    pub fn export_rows(&self) -> Result<Vec<ExportRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.id, i.kind, i.title, i.text, i.meta, json_extract(i.meta, '$.url'), a.bundle_id, a.name,
                    i.device, i.created_at, i.timestamp, i.use_count, i.last_used_at, i.in_history, o.text, i.sensitive
             FROM items i LEFT JOIN apps a ON a.id = i.app_id LEFT JOIN ocr o ON o.item_id = i.id
             ORDER BY i.timestamp DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ExportRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                title: r.get(2)?,
                text: r.get(3)?,
                meta: r.get(4)?,
                url: r.get(5)?,
                app_bundle_id: r.get(6)?,
                app_name: r.get(7)?,
                device: r.get(8)?,
                created_at: r.get(9)?,
                timestamp: r.get(10)?,
                use_count: r.get(11)?,
                last_used_at: r.get(12)?,
                in_history: r.get(13)?,
                ocr: r.get(14)?,
                sensitive: r.get(15)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Pinboard names per item id.
    pub fn item_pinboard_names(&self) -> Result<HashMap<i64, Vec<String>>> {
        let mut stmt = self.conn.prepare(
            "SELECT pi.item_id, p.name FROM pinboard_items pi JOIN pinboards p ON p.id = pi.pinboard_id
             ORDER BY p.sort, pi.sort",
        )?;
        let mut out: HashMap<i64, Vec<String>> = HashMap::new();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, name) = row?;
            out.entry(id).or_default().push(name);
        }
        Ok(out)
    }

    pub fn all_link_previews(&self) -> Result<Vec<crate::link_preview::LinkPreview>> {
        let mut stmt =
            self.conn.prepare("SELECT url, title, description, image, icon FROM link_previews WHERE status = 'ok'")?;
        let rows = stmt.query_map([], |r| {
            Ok(crate::link_preview::LinkPreview {
                url: r.get(0)?,
                title: r.get(1)?,
                description: r.get(2)?,
                image: r.get(3)?,
                icon: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn item_pinboard_names_for(&self, item_id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT p.name FROM pinboard_items pi JOIN pinboards p ON p.id = pi.pinboard_id
             WHERE pi.item_id = ?1 ORDER BY p.sort",
        )?;
        let rows = stmt.query_map([item_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn item_in_history(&self, item_id: i64) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT in_history FROM items WHERE id = ?1", [item_id], |r| r.get(0))
            .optional()?
            .unwrap_or(false))
    }

    pub fn ocr_text(&self, item_id: i64) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT text FROM ocr WHERE item_id = ?1", [item_id], |r| r.get(0)).optional()?)
    }

    pub fn all_checksums(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT checksum FROM items")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_usage(&self, id: i64, use_count: i64, last_used_at: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE items SET use_count = ?2, last_used_at = ?3 WHERE id = ?1",
            params![id, use_count, last_used_at],
        )?;
        Ok(())
    }

    pub fn begin_import(&self, source: &str) -> Result<i64> {
        self.conn.execute("INSERT INTO import_runs (source, started_at) VALUES (?1, ?2)", params![source, now_ms()])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// When the last completed import finished and how many items it added.
    pub fn last_import(&self) -> Result<Option<(i64, i64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT finished_at, imported FROM import_runs WHERE finished_at IS NOT NULL ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn finish_import(&self, run: i64, imported: usize, skipped: usize, failed: usize) -> Result<()> {
        self.conn.execute(
            "UPDATE import_runs SET finished_at = ?2, imported = ?3, skipped = ?4, failed = ?5 WHERE id = ?1",
            params![run, now_ms(), imported as i64, skipped as i64, failed as i64],
        )?;
        Ok(())
    }

    /// Inside `batch`, commits what has been written so far so other
    /// connections are not blocked for the whole import.
    pub fn commit_progress(&self) -> Result<()> {
        if !self.conn.is_autocommit() {
            self.conn.execute_batch("COMMIT; BEGIN")?;
        }
        Ok(())
    }

    /// Wraps many writes in one transaction (much faster for imports).
    pub fn batch<T>(&mut self, f: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN")?;
        match f(self) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db"), dir.path().join("blobs")).unwrap();
        (dir, store)
    }

    pub fn text_item(text: &str, ts: i64) -> NewItem {
        let reps = vec![Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: text.as_bytes().to_vec() }];
        let c = crate::clipboard::classify::classify(&reps).unwrap();
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

    #[test]
    fn insert_dedupe_and_order() {
        let (_d, mut s) = temp_store();
        let a = s.insert(&text_item("alpha", 1), true).unwrap();
        let b = s.insert(&text_item("beta", 2), true).unwrap();
        assert!(matches!(a, Inserted::New(_)) && matches!(b, Inserted::New(_)));
        let again = s.insert(&text_item("alpha", 3), true).unwrap();
        assert_eq!(again, Inserted::Existing(a.id()));
        let items = s.search(&Query::default(), 10, 0).unwrap();
        assert_eq!(items.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(), ["alpha", "beta"]);
    }

    #[test]
    fn large_representations_use_blobs() {
        let (_d, mut s) = temp_store();
        let big = "x".repeat(INLINE_LIMIT * 2);
        let id = s.insert(&text_item(&big, 1), true).unwrap().id();
        assert_eq!(s.representations(id).unwrap()[0].data.len(), big.len());
        assert_eq!(s.collect_garbage().unwrap(), 0);
        s.delete(id).unwrap();
        assert_eq!(s.collect_garbage().unwrap(), 1);
    }

    #[test]
    fn search_text_filters_and_short_terms() {
        let (_d, mut s) = temp_store();
        s.insert(&text_item("The quick brown fox", 1), true).unwrap();
        s.insert(&text_item("https://example.com/fox", 2), true).unwrap();
        s.insert(&text_item("Ünïcode café", 3), true).unwrap();
        s.insert(&text_item("go to it", 4), true).unwrap();
        let find = |q: &str| s.search(&Query::parse(q), 10, 0).unwrap().len();
        assert_eq!(find("fox"), 2);
        assert_eq!(find("QUICK fox"), 1);
        assert_eq!(find("type:link fox"), 1);
        assert_eq!(find("-type:link fox"), 1);
        assert_eq!(find("cafe"), 1);
        assert_eq!(find("uick"), 1);
        assert_eq!(find("to"), 1);
        assert_eq!(find("app:safari"), 4);
        assert_eq!(find("app:chrome"), 0);
        assert_eq!(find("\"quoted"), 0);
    }

    #[test]
    fn pinboards_and_retention() {
        let (_d, mut s) = temp_store();
        let now = 100 * 24 * 3600 * 1000;
        let old = s.insert(&text_item("old", 1), true).unwrap().id();
        let pinned = s.insert(&text_item("old but pinned", 2), true).unwrap().id();
        let fresh = s.insert(&text_item("fresh", now), true).unwrap().id();
        let board = s.create_pinboard("Work", None, None).unwrap();
        s.pin(pinned, board, None).unwrap();
        assert_eq!(s.prune(Retention::Month, now).unwrap(), 1);
        assert!(s.get(old).unwrap().is_none());
        assert!(s.get(pinned).unwrap().is_some());
        let history = s.search(&Query::default(), 10, 0).unwrap();
        assert_eq!(history.iter().map(|i| i.id).collect::<Vec<_>>(), [fresh]);
        let on_board = s.search(&Query::parse("board:work"), 10, 0).unwrap();
        assert_eq!(on_board[0].id, pinned);
        s.unpin(pinned, board).unwrap();
        assert!(s.get(pinned).unwrap().is_none());
    }

    #[test]
    fn edit_and_title() {
        let (_d, mut s) = temp_store();
        let id = s.insert(&text_item("hello", 1), true).unwrap().id();
        s.set_title(id, Some("Greeting")).unwrap();
        s.set_text(id, "https://example.org").unwrap();
        let item = s.get(id).unwrap().unwrap();
        assert_eq!(item.kind, ItemKind::Link);
        assert_eq!(item.title.as_deref(), Some("Greeting"));
        assert_eq!(s.search(&Query::text("greet"), 10, 0).unwrap().len(), 1);
        assert_eq!(s.search(&Query::text("hello"), 10, 0).unwrap().len(), 0);
    }

    #[test]
    fn link_previews_are_searchable_and_queued() {
        let (_d, mut s) = temp_store();
        s.insert(&text_item("https://example.com/a", 1), true).unwrap();
        s.insert(&text_item("https://example.com/b", 2), true).unwrap();
        s.insert(&text_item("plain words", 3), true).unwrap();
        assert_eq!(s.links_needing_preview(10).unwrap(), ["https://example.com/b", "https://example.com/a"]);
        let p = crate::link_preview::LinkPreview {
            url: "https://example.com/a".into(),
            title: Some("Quarterly Roadmap".into()),
            ..Default::default()
        };
        s.set_link_preview(&p, "ok").unwrap();
        s.set_link_preview(
            &crate::link_preview::LinkPreview { url: "https://example.com/b".into(), ..Default::default() },
            "failed",
        )
        .unwrap();
        assert!(s.links_needing_preview(10).unwrap().is_empty());
        assert_eq!(s.search(&Query::text("roadmap"), 10, 0).unwrap().len(), 1);
        assert_eq!(
            s.link_preview("https://example.com/a").unwrap().unwrap().title.as_deref(),
            Some("Quarterly Roadmap")
        );
        assert!(s.link_preview("https://example.com/b").unwrap().is_none());
        assert!(!s.import_link_preview(&p).unwrap());
    }

    #[test]
    fn rule_outcomes_expire_and_pin() {
        let (_d, mut s) = temp_store();
        let temp = s.insert(&text_item("temporary", 1_000), true).unwrap().id();
        let keep = s.insert(&text_item("JIRA-1", 1_000), true).unwrap().id();
        let expire = crate::rules::Outcome { expire_after_ms: Some(60_000), ..Default::default() };
        s.apply_rules(temp, &expire, 1_000).unwrap();
        assert_eq!(s.get(temp).unwrap().unwrap().expires_at, Some(61_000));
        let pin = crate::rules::Outcome {
            expire_after_ms: Some(60_000),
            pin_to: vec!["Tickets".into()],
            ..Default::default()
        };
        assert_eq!(s.apply_rules(keep, &pin, 1_000).unwrap(), ["Tickets"]);
        assert_eq!(s.get(keep).unwrap().unwrap().expires_at, None);
        assert_eq!(s.pinboards().unwrap()[0].name, "Tickets");

        assert_eq!(s.expire_due(60_000).unwrap(), 0);
        assert_eq!(s.expire_due(61_000).unwrap(), 1);
        assert!(s.get(temp).unwrap().is_none());
        assert!(s.get(keep).unwrap().is_some());

        // Pinning by hand cancels a pending expiry.
        let later = s.insert(&text_item("later", 2_000), true).unwrap().id();
        s.apply_rules(later, &expire, 2_000).unwrap();
        let board = s.pinboard_by_name("Tickets").unwrap().unwrap();
        s.pin(later, board, None).unwrap();
        assert_eq!(s.expire_due(i64::MAX).unwrap(), 0);
    }

    #[test]
    fn report_buckets_by_local_day_and_hour() {
        use chrono::{Local, TimeZone};
        let (_d, mut s) = temp_store();
        let at = |d: u32, h: u32| Local.with_ymd_and_hms(2026, 10, d, h, 30, 0).unwrap().timestamp_millis();
        let now = at(10, 18);
        s.insert(&text_item("a", at(10, 9)), true).unwrap();
        s.insert(&text_item("b", at(10, 9)), true).unwrap();
        s.insert(&text_item("https://x.com", at(8, 23)), true).unwrap();
        s.insert(&text_item("old", at(1, 12)), true).unwrap();
        let pasted = s.insert(&text_item("pasted", at(9, 0)), true).unwrap().id();
        s.mark_used(pasted).unwrap();
        s.mark_used(pasted).unwrap();

        let r = s.report(7, now, None).unwrap();
        assert_eq!(r.per_day.len(), 7);
        assert_eq!(r.per_day.last().unwrap(), &(chrono::NaiveDate::from_ymd_opt(2026, 10, 10).unwrap(), 2));
        assert_eq!(r.per_day[4].1, 1, "Oct 8");
        assert_eq!(r.added, 4);
        assert_eq!(r.added_previous, 1, "Oct 1 falls in the previous 7 days");
        assert_eq!((r.per_hour[9], r.per_hour[23], r.per_hour[0]), (2, 1, 1));
        assert_eq!(r.by_kind[0], ("text".to_string(), 3));
        assert_eq!(r.top_apps[0].1, "Safari");
        assert_eq!(r.most_pasted.len(), 1);
        assert_eq!((r.most_pasted[0].id, r.most_pasted[0].use_count), (pasted, 2));
        assert_eq!(r.pastes, 2);
    }

    #[test]
    fn sensitive_items_are_hidden_when_expired_and_scrubbed() {
        let (dir, mut s) = temp_store();
        let now = now_ms();
        let secret = "x".repeat(40_000) + "TOP-SECRET-TOKEN"; // large enough for a blob file
        let id = s.insert(&text_item(&secret, now), true).unwrap().id();
        let keep = s.insert(&text_item("normal", now), true).unwrap().id();
        s.mark_sensitive(id, now + 60_000).unwrap();
        let item = s.get(id).unwrap().unwrap();
        assert!(item.sensitive);
        assert!(item.summary(80).contains("sensitive, hidden"));
        assert_eq!(s.search(&Query::default(), 10, 0).unwrap().len(), 2);
        // Once past its deadline it disappears from search even before the sweep.
        s.mark_sensitive(id, now - 1).unwrap();
        let visible: Vec<i64> = s.search(&Query::default(), 10, 0).unwrap().iter().map(|i| i.id).collect();
        assert_eq!(visible, [keep]);
        assert_eq!(s.expire_due(now_ms()).unwrap(), 1);
        drop(s);
        // Nothing of the secret is left in any file: db, WAL or blobs.
        let mut stack = vec![dir.path().to_path_buf()];
        while let Some(p) = stack.pop() {
            for e in std::fs::read_dir(&p).unwrap().flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let data = std::fs::read(&path).unwrap();
                    assert!(!data.windows(16).any(|w| w == b"TOP-SECRET-TOKEN"), "found in {}", path.display());
                }
            }
        }
    }

    #[test]
    fn fuzzy_fills_in_after_exact_results() {
        let (_d, mut s) = temp_store();
        s.insert(&text_item("Quarterly roadmap review", 1), true).unwrap();
        s.insert(&text_item("https://github.com/rust-lang/rust", 2), true).unwrap();
        s.insert(&text_item("roadmap draft", 3), true).unwrap();
        let q = Query::parse("roadmap");
        let (items, more, fuzzy) = s.search_with_fuzzy(&q, 10, 0, true).unwrap();
        assert_eq!((items.len(), more, fuzzy), (2, false, 0), "exact matches only, nothing fuzzy to add");
        let (items, _, fuzzy) = s.search_with_fuzzy(&Query::parse("quartely"), 10, 0, true).unwrap();
        assert_eq!((items.len(), fuzzy), (1, 1));
        assert_eq!(items[0].text, "Quarterly roadmap review");
        let (items, _, fuzzy) = s.search_with_fuzzy(&Query::parse("gthb"), 10, 0, true).unwrap();
        assert_eq!((items.len(), fuzzy), (1, 1));
        let (items, _, _) = s.search_with_fuzzy(&Query::parse("gthb"), 10, 0, false).unwrap();
        assert!(items.is_empty(), "fuzzy can be turned off");
        // Filters still apply to fuzzy matches.
        let (items, _, _) = s.search_with_fuzzy(&Query::parse("type:link quartely"), 10, 0, true).unwrap();
        assert!(items.is_empty());
        // Exact first, then close matches, without duplicates.
        let (items, _, fuzzy) = s.search_with_fuzzy(&Query::parse("roadmp"), 10, 0, true).unwrap();
        assert_eq!((items.len(), fuzzy), (2, 2));
    }

    #[test]
    fn report_per_device() {
        let (_d, mut s) = temp_store();
        let now = now_ms();
        let on = |text: &str, device: Option<&str>| {
            let mut item = text_item(text, now - 1_000);
            item.device = device.map(str::to_string);
            item
        };
        s.insert(&on("a", Some("MacBook Pro")), true).unwrap();
        s.insert(&on("b", Some("MacBook Pro")), true).unwrap();
        s.insert(&on("c", Some("Mac mini")), true).unwrap();
        s.insert(&on("d", None), true).unwrap();
        let all = s.report(7, now, None).unwrap();
        assert_eq!(all.added, 4);
        let names: Vec<_> = all.devices.iter().map(|d| (d.name.as_str(), d.items)).collect();
        assert_eq!(names[0], ("MacBook Pro", 2));
        assert!(names.contains(&("", 1)) && names.contains(&("Mac mini", 1)));
        assert_eq!(all.devices[0].top_app.as_deref(), Some("Safari"));
        let pro = s.report(7, now, Some("MacBook Pro")).unwrap();
        assert_eq!((pro.added, pro.history_items), (2, 2));
        assert_eq!(pro.devices.len(), 3, "the breakdown always lists every device");
        let unknown = s.report(7, now, Some("")).unwrap();
        assert_eq!(unknown.added, 1);
        assert_eq!(s.device_names().unwrap()[0], ("MacBook Pro".to_string(), 2));
    }

    #[test]
    fn ocr_is_searchable() {
        let (_d, mut s) = temp_store();
        let id = s.insert(&text_item("img placeholder", 1), true).unwrap().id();
        s.set_ocr(id, "invoice total").unwrap();
        assert_eq!(s.search(&Query::text("invoice"), 10, 0).unwrap().len(), 1);
    }
}
