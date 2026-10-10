//! Importer for the Paste app (pasteapp.io) by Wiheads.
//!
//! Paste's own files are never opened directly: the Core Data store and search
//! index are copied to a temporary directory first and read from there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};

use super::decode::{decode_item_blob, decode_preview, ocr_text};
use crate::clipboard::classify::classify;
use crate::model::{ItemKind, ItemMeta, NewItem, SourceApp};
use crate::settings::{Retention, Settings};
use crate::shortcut::Shortcut;
use crate::store::Store;

/// Seconds between 1970-01-01 and Core Data's reference date 2001-01-01.
const CORE_DATA_EPOCH: f64 = 978_307_200.0;

const BUNDLE_IDS: &[&str] = &["com.wiheads.paste-setapp", "com.wiheads.paste", "com.wiheads.paste-mas"];

#[derive(Debug, Clone)]
pub struct PasteInstall {
    pub bundle_id: String,
    pub data_dir: PathBuf,
    pub prefs: Option<PathBuf>,
}

impl PasteInstall {
    pub fn from_dir(dir: &Path) -> Result<PasteInstall> {
        if !dir.join("db.sqlite").exists() {
            bail!("{} does not contain a Paste db.sqlite", dir.display());
        }
        let bundle_id = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| n.starts_with("com.wiheads."))
            .unwrap_or_else(|| "com.wiheads.paste".into());
        let prefs = prefs_path(&bundle_id);
        Ok(PasteInstall { bundle_id, data_dir: dir.to_path_buf(), prefs })
    }
}

fn prefs_path(bundle_id: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    [
        home.join("Library/Preferences").join(format!("{bundle_id}.plist")),
        home.join("Library/Containers").join(bundle_id).join(format!("Data/Library/Preferences/{bundle_id}.plist")),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// Finds Paste installations that have a database on this Mac.
pub fn detect() -> Vec<PasteInstall> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let mut found = Vec::new();
    for id in BUNDLE_IDS {
        let candidates = [
            home.join("Library/Application Support").join(id),
            home.join("Library/Containers").join(id).join("Data/Library/Application Support").join(id),
        ];
        if let Some(dir) = candidates.into_iter().find(|d| d.join("db.sqlite").exists()) {
            found.push(PasteInstall { bundle_id: id.to_string(), data_dir: dir, prefs: prefs_path(id) });
        }
    }
    found
}

#[derive(Debug, Default)]
pub struct ImportOptions {
    pub dry_run: bool,
    pub history: bool,
    pub settings: bool,
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub items_found: usize,
    pub items_imported: usize,
    pub items_already_present: usize,
    pub items_failed: usize,
    pub pinboards_created: usize,
    pub ocr_imported: usize,
    pub previews_imported: usize,
    pub settings_changed: Vec<String>,
    /// Imported history items older than the retention setting; they will be
    /// removed by the next cleanup unless retention is raised.
    pub items_beyond_retention: usize,
    /// Items skipped by the optional import privacy filter.
    pub items_filtered: usize,
    pub retention: Option<Retention>,
    pub errors: Vec<String>,
}

impl ImportReport {
    fn error(&mut self, msg: String) {
        if self.errors.len() < 25 {
            self.errors.push(msg);
        }
    }

    pub fn summary(&self) -> String {
        let mut lines = vec![
            format!("Items found: {}", self.items_found),
            format!("Imported: {}", self.items_imported),
            format!("Already imported: {}", self.items_already_present),
            format!("Failed: {}", self.items_failed),
            format!("Skipped by privacy filter: {}", self.items_filtered),
            format!("Pinboards created: {}", self.pinboards_created),
            format!("OCR texts imported: {}", self.ocr_imported),
            format!("Link previews imported: {}", self.previews_imported),
        ];
        if self.items_beyond_retention > 0 {
            lines.push(format!(
                "Warning: {} imported history items are older than the history retention setting ({}) and will be \
                 removed at the next cleanup. Raise history_retention in settings to keep them.",
                self.items_beyond_retention,
                self.retention.map(|r| format!("{r:?}").to_lowercase()).unwrap_or_default()
            ));
        }
        if !self.settings_changed.is_empty() {
            lines.push(format!("Settings: {}", self.settings_changed.join("; ")));
        }
        for e in &self.errors {
            lines.push(format!("  error: {e}"));
        }
        lines.join("\n")
    }
}

/// Rules applied to each imported item.
struct ImportPolicy {
    /// History older than this (Unix ms) will be pruned by retention.
    retention_cutoff: Option<i64>,
    privacy: ImportPrivacy,
}

/// The optional privacy checks from `settings.import`, ready to apply.
struct ImportPrivacy {
    settings: crate::settings::ImportFilter,
    excluded_apps: Vec<String>,
    patterns: Vec<regex::Regex>,
}

impl ImportPrivacy {
    fn new(settings: &Settings) -> ImportPrivacy {
        ImportPrivacy {
            settings: settings.import.clone(),
            excluded_apps: settings.privacy.excluded_apps.clone(),
            patterns: settings.privacy.ignore_patterns.iter().filter_map(|p| regex::Regex::new(p).ok()).collect(),
        }
    }

    fn skips(&self, text: &str, bundle_id: Option<&str>) -> bool {
        use crate::clipboard::privacy::{looks_secret, mentions_password};
        let s = &self.settings;
        (s.skip_excluded_apps
            && bundle_id.is_some_and(|b| self.excluded_apps.iter().any(|a| a.eq_ignore_ascii_case(b))))
            || (s.skip_secrets && looks_secret(text))
            || (s.skip_ignore_patterns && self.patterns.iter().any(|p| p.is_match(text)))
            || (s.skip_password_mentions && mentions_password(text))
    }
}

/// Copies the database files to a temp dir so Paste's files are never touched.
fn snapshot(install: &PasteInstall) -> Result<tempfile::TempDir> {
    let tmp = tempfile::Builder::new().prefix("pasted-import").tempdir()?;
    for base in ["db.sqlite", "index.sqlite"] {
        for suffix in ["", "-wal", "-shm"] {
            let name = format!("{base}{suffix}");
            let src = install.data_dir.join(&name);
            if src.exists() {
                std::fs::copy(&src, tmp.path().join(&name)).with_context(|| format!("copying {}", src.display()))?;
            }
        }
    }
    Ok(tmp)
}

fn core_data_ms(seconds: Option<f64>) -> Option<i64> {
    seconds.map(|s| ((s + CORE_DATA_EPOCH) * 1000.0) as i64)
}

fn kind_from_raw_type(raw: i64) -> ItemKind {
    match raw {
        0 => ItemKind::Color,
        1 => ItemKind::Image,
        3 => ItemKind::File,
        4 => ItemKind::Link,
        _ => ItemKind::Text,
    }
}

/// Reads a column that may be stored as either BLOB or TEXT.
fn bytes_col(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<Vec<u8>>> {
    use rusqlite::types::ValueRef;
    Ok(match row.get_ref(idx)? {
        ValueRef::Blob(b) | ValueRef::Text(b) => Some(b.to_vec()),
        _ => None,
    })
}

struct PasteList {
    identifier: String,
    name: String,
    is_pinboard: bool,
    color: Option<u32>,
    capacity_days: Option<i64>,
    order: i64,
}

fn read_lists(db: &Connection) -> Result<HashMap<i64, PasteList>> {
    let mut stmt = db.prepare(
        "SELECT l.Z_PK, l.ZRAWTYPE, l.ZIDENTIFIER, l.ZNAME, l.ZRAWATTRIBUTES,
                (SELECT min(m.ZINDEX) FROM ZLISTMETADATAENTITY m WHERE m.ZLIST = l.Z_PK)
         FROM ZLISTENTITY l",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            bytes_col(r, 4)?,
            r.get::<_, Option<i64>>(5)?,
        ))
    })?;
    let mut lists = HashMap::new();
    for row in rows {
        let (pk, raw_type, identifier, name, attrs, order) = row?;
        let attrs: serde_json::Value =
            attrs.and_then(|a| serde_json::from_slice(&a).ok()).unwrap_or(serde_json::Value::Null);
        let kind = attrs.get("type").and_then(|t| t.as_str());
        let is_pinboard = kind == Some("pinboard") || (kind.is_none() && raw_type == Some(2));
        lists.insert(
            pk,
            PasteList {
                identifier: identifier.unwrap_or_else(|| pk.to_string()),
                name: name.unwrap_or_else(|| "Pinboard".into()),
                is_pinboard,
                color: attrs.get("colorCode").and_then(|c| c.as_u64()).map(|c| c as u32),
                capacity_days: attrs.get("capacity").and_then(|c| c.as_i64()),
                order: order.unwrap_or(pk),
            },
        );
    }
    Ok(lists)
}

pub fn import(
    install: &PasteInstall,
    store: &mut Store,
    settings: &mut Settings,
    opts: &ImportOptions,
) -> Result<ImportReport> {
    let tmp = snapshot(install)?;
    let db = Connection::open(tmp.path().join("db.sqlite")).context("opening copy of Paste database")?;
    let mut report = ImportReport::default();
    let lists = read_lists(&db)?;

    if opts.settings {
        import_settings(install, &lists, settings, &mut report);
    }
    if opts.history {
        let external_dir = install.data_dir.join(".db_SUPPORT/_EXTERNAL_DATA");
        let run = if opts.dry_run { None } else { Some(store.begin_import(&install.bundle_id)?) };
        let cutoff = settings.general.history_retention.max_age_ms().map(|age| crate::model::now_ms() - age);
        let policy = ImportPolicy { retention_cutoff: cutoff, privacy: ImportPrivacy::new(settings) };
        let result = if opts.dry_run {
            import_items(&db, &lists, &external_dir, store, opts, &policy, &mut report)
        } else {
            store.batch(|store| import_items(&db, &lists, &external_dir, store, opts, &policy, &mut report))
        };
        report.retention = Some(settings.general.history_retention);
        result?;
        if !opts.dry_run {
            import_ocr(tmp.path(), store, &mut report)?;
        }
        if let Some(run) = run {
            store.finish_import(run, report.items_imported, report.items_already_present, report.items_failed)?;
        }
    }
    Ok(report)
}

fn import_items(
    db: &Connection,
    lists: &HashMap<i64, PasteList>,
    external_dir: &Path,
    store: &mut Store,
    opts: &ImportOptions,
    policy: &ImportPolicy,
    report: &mut ImportReport,
) -> Result<()> {
    // Pinboards first, in Paste's order.
    let mut boards: HashMap<i64, i64> = HashMap::new();
    let mut pinboard_lists: Vec<(&i64, &PasteList)> = lists.iter().filter(|(_, l)| l.is_pinboard).collect();
    pinboard_lists.sort_by_key(|(_, l)| l.order);
    for (pk, list) in pinboard_lists {
        let external = format!("paste:list:{}", list.identifier);
        let id = match store.pinboard_by_external_id(&external)? {
            Some(id) => id,
            None if opts.dry_run => {
                report.pinboards_created += 1;
                continue;
            }
            None => {
                report.pinboards_created += 1;
                store.create_pinboard(&list.name, list.color, Some(&external))?
            }
        };
        boards.insert(*pk, id);
    }

    let mut stmt = db.prepare(
        "SELECT i.ZIDENTIFIER, i.ZRAWTYPE, i.ZTITLE, i.ZTIMESTAMP, i.ZCREATEDAT, i.ZLIST,
                i.ZDISPLAYORDERINPINBOARD, a.ZBUNDLEIDENTIFIER, a.ZNAME, dev.ZNAME,
                (SELECT d.ZRAWPASTEBOARDITEMS FROM ZITEMDATAENTITY d WHERE d.ZITEM = i.Z_PK OR d.Z_PK = i.ZDATA LIMIT 1),
                i.ZRAWPREVIEW
         FROM ZITEMENTITY i
         LEFT JOIN ZAPPLICATIONENTITY a ON a.Z_PK = i.ZSOURCEAPPLICATION
         LEFT JOIN ZDEVICEENTITY dev ON dev.Z_PK = i.ZDEVICE
         ORDER BY i.ZTIMESTAMP",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        report.items_found += 1;
        let identifier: Option<String> = r.get(0)?;
        let Some(identifier) = identifier else {
            report.items_failed += 1;
            continue;
        };
        let external_id = format!("paste:item:{identifier}");
        let preview_raw = bytes_col(r, 11)?;
        if let Some(id) = store.item_by_external_id(&external_id)? {
            report.items_already_present += 1;
            // Earlier imports did not bring previews; fill them in.
            if !opts.dry_run
                && let Some(url) = store.get(id)?.and_then(|i| i.meta.url)
            {
                import_preview(store, &url, preview_raw.as_deref(), external_dir, report)?;
            }
            continue;
        }
        let raw_type: Option<i64> = r.get(1)?;
        let title: Option<String> = r.get(2)?;
        let timestamp = core_data_ms(r.get(3)?);
        let created = core_data_ms(r.get(4)?);
        let list_pk: Option<i64> = r.get(5)?;
        let pin_order: Option<i64> = r.get(6)?;
        let bundle_id: Option<String> = r.get(7)?;
        let app_name: Option<String> = r.get(8)?;
        let device: Option<String> = r.get(9)?;
        let blob = bytes_col(r, 10)?;

        let reps = match blob.as_deref().map(|b| decode_item_blob(b, external_dir)) {
            Some(Ok(reps)) if !reps.is_empty() => reps,
            Some(Ok(_)) | None => {
                report.items_failed += 1;
                report.error(format!("{identifier}: no pasteboard data"));
                continue;
            }
            Some(Err(e)) => {
                report.items_failed += 1;
                report.error(format!("{identifier}: {e:#}"));
                continue;
            }
        };
        let (kind, text, meta) = match classify(&reps) {
            Some(c) => (c.kind, c.text, c.meta),
            None => (kind_from_raw_type(raw_type.unwrap_or(5)), title.clone().unwrap_or_default(), ItemMeta::default()),
        };
        if policy.privacy.skips(&text, bundle_id.as_deref()) {
            report.items_filtered += 1;
            continue;
        }
        let in_history = list_pk.and_then(|pk| boards.get(&pk)).is_none()
            && !list_pk.and_then(|pk| lists.get(&pk)).is_some_and(|l| l.is_pinboard);
        if in_history && policy.retention_cutoff.is_some_and(|c| timestamp.or(created).unwrap_or(i64::MAX) < c) {
            report.items_beyond_retention += 1;
        }
        if opts.dry_run {
            report.items_imported += 1;
            continue;
        }

        let title = title.filter(|t| !t.trim().is_empty() && t.trim() != text.trim());
        let board = list_pk.and_then(|pk| boards.get(&pk).copied());
        let timestamp = timestamp.or(created).unwrap_or_else(crate::model::now_ms);
        let item = NewItem {
            kind,
            title,
            text,
            meta,
            representations: reps,
            source_app: bundle_id.map(|bundle_id| SourceApp { bundle_id, name: app_name }),
            device,
            created_at: created.unwrap_or(timestamp),
            timestamp,
            external_id: Some(external_id),
            in_history: board.is_none(),
        };
        let id = store.insert(&item, false)?.id();
        if let (ItemKind::Link, Some(url)) = (item.kind, &item.meta.url) {
            import_preview(store, url, preview_raw.as_deref(), external_dir, report)?;
        }
        if let Some(board) = board {
            store.pin(id, board, pin_order)?;
        }
        report.items_imported += 1;
        if report.items_imported.is_multiple_of(100) {
            store.commit_progress()?;
        }
    }
    Ok(())
}

fn import_preview(
    store: &Store,
    url: &str,
    raw: Option<&[u8]>,
    external_dir: &Path,
    report: &mut ImportReport,
) -> Result<()> {
    let Some(p) = raw.and_then(|raw| decode_preview(raw, external_dir)) else { return Ok(()) };
    if p.title.is_none() && p.image.is_none() {
        // Nothing useful; let the fetcher try instead.
        return Ok(());
    }
    let preview = crate::link_preview::LinkPreview { url: url.to_string(), ..p };
    if store.import_link_preview(&preview)? {
        report.previews_imported += 1;
    }
    Ok(())
}

fn import_ocr(tmp: &Path, store: &mut Store, report: &mut ImportReport) -> Result<()> {
    let path = tmp.join("index.sqlite");
    if !path.exists() {
        return Ok(());
    }
    let index = Connection::open(&path)?;
    let has_table: Option<String> =
        index.query_row("SELECT name FROM sqlite_master WHERE name = 'items_ocr'", [], |r| r.get(0)).optional()?;
    if has_table.is_none() {
        return Ok(());
    }
    let mut stmt = index.prepare("SELECT item_id, metadata FROM items_ocr")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, bytes_col(r, 1)?.unwrap_or_default())))?;
    store.batch(|store| {
        for row in rows {
            let (paste_id, metadata) = row?;
            let Some(text) = ocr_text(&metadata) else { continue };
            if let Some(id) = store.item_by_external_id(&format!("paste:item:{paste_id}"))? {
                store.set_ocr(id, &text)?;
                report.ocr_imported += 1;
            }
        }
        Ok(())
    })
}

fn import_settings(
    install: &PasteInstall,
    lists: &HashMap<i64, PasteList>,
    settings: &mut Settings,
    report: &mut ImportReport,
) {
    if let Some(days) = lists.values().find(|l| !l.is_pinboard).and_then(|l| l.capacity_days) {
        let retention = Retention::from_days(days);
        settings.general.history_retention = retention;
        report.settings_changed.push(format!("history retention {retention:?} ({days} days)"));
    }

    let Some(prefs_path) = &install.prefs else {
        report.error("Paste preferences file not found; shortcuts not imported".into());
        return;
    };
    let prefs = match plist::Value::from_file(prefs_path) {
        Ok(plist::Value::Dictionary(d)) => d,
        Ok(_) => return,
        Err(e) => {
            report.error(format!("reading {}: {e}", prefs_path.display()));
            return;
        }
    };
    let shortcut = |key: &str| -> Option<Shortcut> {
        match prefs.get(key)? {
            plist::Value::Data(d) => Shortcut::from_paste_json(d),
            plist::Value::String(s) => Shortcut::from_paste_json(s.as_bytes()),
            _ => None,
        }
    };
    let sc = &mut settings.shortcuts;
    for (key, target, label) in [
        ("activationShortcut", &mut sc.open_shelf, "open shelf"),
        ("pasteStackShortcut", &mut sc.paste_stack, "paste stack"),
        ("nextPinboardShortcut", &mut sc.next_pinboard, "next pinboard"),
        ("previousPinboardShortcut", &mut sc.previous_pinboard, "previous pinboard"),
    ] {
        if let Some(s) = shortcut(key) {
            *target = s;
            report.settings_changed.push(format!("{label} shortcut {s}"));
        }
    }
    if let Some(visible) = prefs.get("isScreenSharingContentVisible").and_then(|v| v.as_boolean()) {
        settings.privacy.visible_during_screen_sharing = visible;
        report.settings_changed.push(format!("visible during screen sharing: {visible}"));
    }
    // Excluded apps are only written by Paste once customized; accept any
    // array of bundle identifiers under an ignore/exclude key.
    for (key, value) in prefs.iter() {
        let k = key.to_ascii_lowercase();
        if !(k.contains("ignore") || k.contains("exclude")) {
            continue;
        }
        let Some(array) = value.as_array() else { continue };
        for id in array.iter().filter_map(|v| v.as_string()) {
            if id.contains('.') && !settings.privacy.excluded_apps.iter().any(|a| a == id) {
                settings.privacy.excluded_apps.push(id.to_string());
                report.settings_changed.push(format!("excluded app {id}"));
            }
        }
    }
}

/// True when Paste is running (its database may change while we copy it).
pub fn is_running(install: &PasteInstall) -> bool {
    let apps = objc2_app_kit::NSRunningApplication::runningApplicationsWithBundleIdentifier(
        &objc2_foundation::NSString::from_str(&install.bundle_id),
    );
    !apps.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Query;

    /// Builds a minimal Paste-shaped Core Data store with synthetic content.
    fn fixture(dir: &Path) {
        let db = Connection::open(dir.join("db.sqlite")).unwrap();
        db.execute_batch(
            r#"
            CREATE TABLE ZAPPLICATIONENTITY (Z_PK INTEGER PRIMARY KEY, ZBUNDLEIDENTIFIER VARCHAR, ZNAME VARCHAR);
            CREATE TABLE ZDEVICEENTITY (Z_PK INTEGER PRIMARY KEY, ZNAME VARCHAR);
            CREATE TABLE ZLISTENTITY (Z_PK INTEGER PRIMARY KEY, ZRAWTYPE INTEGER, ZIDENTIFIER VARCHAR, ZNAME VARCHAR, ZRAWATTRIBUTES BLOB);
            CREATE TABLE ZLISTMETADATAENTITY (Z_PK INTEGER PRIMARY KEY, ZINDEX INTEGER, ZLIST INTEGER);
            CREATE TABLE ZITEMENTITY (Z_PK INTEGER PRIMARY KEY, ZDISPLAYORDERINPINBOARD INTEGER, ZRAWTYPE INTEGER,
                ZDATA INTEGER, ZDEVICE INTEGER, ZLIST INTEGER, ZSOURCEAPPLICATION INTEGER, ZCREATEDAT TIMESTAMP,
                ZTIMESTAMP TIMESTAMP, ZIDENTIFIER VARCHAR, ZTITLE VARCHAR, ZRAWPREVIEW BLOB);
            CREATE TABLE ZITEMDATAENTITY (Z_PK INTEGER PRIMARY KEY, ZITEM INTEGER, ZRAWPASTEBOARDITEMS BLOB);
            INSERT INTO ZAPPLICATIONENTITY VALUES (1, 'com.example.editor', 'Editor');
            INSERT INTO ZDEVICEENTITY VALUES (1, 'Test Mac');
            INSERT INTO ZLISTENTITY VALUES (1, 1, 'clip', 'Clipboard', '{"type":"clipboard","capacity":365}');
            INSERT INTO ZLISTENTITY VALUES (2, 2, 'pin-a', 'Snippets', '{"type":"pinboard","colorCode":4293940557}');
            INSERT INTO ZLISTMETADATAENTITY VALUES (1, 0, 2);
            "#,
        )
        .unwrap();
        let json = |text: &str| {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(text);
            let mut v = vec![1u8];
            let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            std::io::Write::write_all(
                &mut enc,
                format!(
                    r#"[{{"types":["public.utf8-plain-text"],"dataByType":{{"public.utf8-plain-text":"{b64}"}}}}]"#
                )
                .as_bytes(),
            )
            .unwrap();
            v.extend(enc.finish().unwrap());
            v
        };
        let items: [(i64, &str, Option<i64>, &str, f64); 5] = [
            (1, "item-1", Some(1), "first clip", 700_000_000.0),
            (2, "item-2", Some(1), "https://example.com", 700_000_100.0),
            (3, "item-3", Some(2), "pinned snippet", 700_000_200.0),
            (5, "item-5", Some(1), "key AKIAIOSFODNN7EXAMPLE", 700_000_400.0),
            (6, "item-6", Some(1), "staging login admin / Password1", 700_000_500.0),
        ];
        for (pk, ident, list, text, ts) in items {
            db.execute(
                "INSERT INTO ZITEMENTITY (Z_PK, ZDISPLAYORDERINPINBOARD, ZRAWTYPE, ZDATA, ZDEVICE, ZLIST,
                     ZSOURCEAPPLICATION, ZCREATEDAT, ZTIMESTAMP, ZIDENTIFIER, ZTITLE)
                 VALUES (?1, 0, 5, ?1, 1, ?2, 1, ?3, ?3, ?4, NULL)",
                rusqlite::params![pk, list, ts, ident],
            )
            .unwrap();
            db.execute("INSERT INTO ZITEMDATAENTITY VALUES (?1, ?1, ?2)", rusqlite::params![pk, json(text)]).unwrap();
        }
        let mut preview = vec![1u8];
        preview.extend_from_slice(br#"{"type":"link","url":"https://example.com","urlName":"Example Domain Page"}"#);
        db.execute("UPDATE ZITEMENTITY SET ZRAWPREVIEW = ?1 WHERE Z_PK = 2", [preview]).unwrap();
        // A broken item must be reported, not abort the import.
        db.execute(
            "INSERT INTO ZITEMENTITY (Z_PK, ZIDENTIFIER, ZTIMESTAMP, ZLIST) VALUES (4, 'item-4', 700000300, 1)",
            [],
        )
        .unwrap();
        let index = Connection::open(dir.join("index.sqlite")).unwrap();
        index
            .execute_batch(
                r#"CREATE TABLE items_ocr(item_id TEXT PRIMARY KEY, metadata BLOB NOT NULL);
                   INSERT INTO items_ocr VALUES ('item-1', '{"words":[{"string":"receipt","confidence":1}]}');"#,
            )
            .unwrap();
    }

    #[test]
    fn imports_fixture_idempotently() {
        let src = tempfile::tempdir().unwrap();
        fixture(src.path());
        let install = PasteInstall { bundle_id: "com.wiheads.paste".into(), data_dir: src.path().into(), prefs: None };
        let (_d, mut store) = crate::store::tests::temp_store();
        let mut settings = Settings::default();
        let opts = ImportOptions { dry_run: false, history: true, settings: true };

        let report = import(&install, &mut store, &mut settings, &opts).unwrap();
        assert_eq!(report.items_found, 6);
        assert_eq!(report.items_imported, 5);
        assert_eq!(report.items_filtered, 0);
        assert_eq!(report.items_failed, 1);
        assert_eq!(report.pinboards_created, 1);
        assert_eq!(report.ocr_imported, 1);
        assert_eq!(report.previews_imported, 1);
        assert_eq!(store.search(&Query::text("domain page"), 10, 0).unwrap().len(), 1);
        assert_eq!(settings.general.history_retention, Retention::Year);
        // Fixture timestamps are from 2023, older than a year.
        assert_eq!(report.items_beyond_retention, 4);

        let history = store.search(&Query::default(), 10, 0).unwrap();
        assert_eq!(history.len(), 4);
        assert_eq!(history[2].kind, ItemKind::Link);
        assert_eq!(history[3].app_name.as_deref(), Some("Editor"));
        assert_eq!(history[3].device.as_deref(), Some("Test Mac"));
        // 700000000 seconds after 2001-01-01 is 2023-03-08.
        assert_eq!(history[3].timestamp, (700_000_000.0 + CORE_DATA_EPOCH) as i64 * 1000);
        assert_eq!(store.search(&Query::text("receipt"), 10, 0).unwrap().len(), 1);
        let boards = store.pinboards().unwrap();
        assert_eq!(boards[0].name, "Snippets");
        assert_eq!(boards[0].color, 0xFFF0554D);
        assert_eq!(store.search(&Query::parse("board:snippets"), 10, 0).unwrap()[0].text, "pinned snippet");

        let again = import(&install, &mut store, &mut settings, &opts).unwrap();
        assert_eq!(again.items_imported, 0);
        assert_eq!(again.items_already_present, 5);
        assert_eq!(again.pinboards_created, 0);
    }

    #[test]
    fn optional_privacy_filter_is_selective() {
        let src = tempfile::tempdir().unwrap();
        fixture(src.path());
        let install = PasteInstall { bundle_id: "com.wiheads.paste".into(), data_dir: src.path().into(), prefs: None };
        let opts = ImportOptions { dry_run: false, history: true, settings: false };

        // Only the secrets check: the AWS key is skipped, the password mention is kept.
        let (_d, mut store) = crate::store::tests::temp_store();
        let mut settings = Settings::default();
        settings.import.skip_secrets = true;
        let report = import(&install, &mut store, &mut settings, &opts).unwrap();
        assert_eq!((report.items_imported, report.items_filtered), (4, 1));
        assert!(store.search(&Query::text("AKIA"), 10, 0).unwrap().is_empty());
        assert_eq!(store.search(&Query::text("Password1"), 10, 0).unwrap().len(), 1);

        // Both checks, plus excluded apps (the fixture's app is excluded here).
        let (_d, mut store) = crate::store::tests::temp_store();
        let mut settings = Settings::default();
        settings.import.skip_secrets = true;
        settings.import.skip_password_mentions = true;
        let report = import(&install, &mut store, &mut settings, &opts).unwrap();
        assert_eq!((report.items_imported, report.items_filtered), (3, 2));
        settings.import.skip_excluded_apps = true;
        settings.privacy.excluded_apps.push("com.example.editor".into());
        let (_d, mut store) = crate::store::tests::temp_store();
        let report = import(&install, &mut store, &mut settings, &opts).unwrap();
        assert_eq!((report.items_imported, report.items_filtered), (0, 5));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let src = tempfile::tempdir().unwrap();
        fixture(src.path());
        let install = PasteInstall { bundle_id: "com.wiheads.paste".into(), data_dir: src.path().into(), prefs: None };
        let (_d, mut store) = crate::store::tests::temp_store();
        let mut settings = Settings::default();
        let opts = ImportOptions { dry_run: true, history: true, settings: false };
        let report = import(&install, &mut store, &mut settings, &opts).unwrap();
        assert_eq!(report.items_imported, 5);
        assert_eq!(store.stats().unwrap().items, 0);
        assert!(store.pinboards().unwrap().is_empty());
    }
}
