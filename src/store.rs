//! SQLite storage for items, representations, pinboards and search.

use std::collections::HashSet;
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

pub struct Store {
    conn: Connection,
    blobs: PathBuf,
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
     i.created_at, i.timestamp, i.use_count, i.size_bytes";

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
    })
}

impl Store {
    pub fn open_default() -> Result<Store> {
        Store::open(&crate::paths::db_path()?, crate::paths::blobs_dir()?)
    }

    pub fn open(db: &Path, blobs: PathBuf) -> Result<Store> {
        let conn = Connection::open(db).with_context(|| format!("opening {}", db.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        std::fs::create_dir_all(&blobs)?;
        let mut store = Store { conn, blobs };
        store.migrate()?;
        Ok(store)
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
        let hash = hex(&Sha256::digest(data));
        let path = self.blob_path(&hash);
        if !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, data)?;
            std::fs::rename(&tmp, &path)?;
        }
        Ok(hash)
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
                (None, Some(hash)) => std::fs::read(self.blob_path(&hash))
                    .with_context(|| format!("reading blob {hash} for item {id}"))?,
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
            Some((None, Some(hash))) => Some(std::fs::read(self.blob_path(&hash))?),
            _ => None,
        })
    }

    pub fn search(&self, q: &Query, limit: usize, offset: usize) -> Result<Vec<Item>> {
        let mut clauses: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();

        let board = match (q.board_id, &q.board_name) {
            (Some(id), _) => Some(id),
            (None, Some(name)) => match self.pinboard_by_name(name)? {
                Some(id) => Some(id),
                None => return Ok(Vec::new()),
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
        args.push((limit as i64).into());
        args.push((offset as i64).into());
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM items i LEFT JOIN apps a ON a.id = i.app_id
             WHERE {} ORDER BY {order} LIMIT ? OFFSET ?",
            clauses.join(" AND ")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), item_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
                 FROM items WHERE kind = 'link' GROUP BY 1
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
    fn ocr_is_searchable() {
        let (_d, mut s) = temp_store();
        let id = s.insert(&text_item("img placeholder", 1), true).unwrap().id();
        s.set_ocr(id, "invoice total").unwrap();
        assert_eq!(s.search(&Query::text("invoice"), 10, 0).unwrap().len(), 1);
    }
}
