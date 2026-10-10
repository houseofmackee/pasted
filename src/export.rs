//! Export history to a ZIP backup, JSON or CSV, and restore ZIP backups.
//!
//! A ZIP backup holds `items.json` (the manifest) and `data/<sha256>` files,
//! one per distinct pasteboard representation, so restoring is lossless.
//! JSON is the same manifest without data files; CSV is one row per item.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{ItemKind, ItemMeta, NewItem, Representation, SourceApp, UTI_PLAIN_TEXT};
use crate::search::Query;
use crate::store::Store;

const FORMAT_NAME: &str = "pasted-export";
const FORMAT_VERSION: u32 = 1;
const MANIFEST: &str = "items.json";
/// Refuse archive entries larger than this (zip bomb protection).
const MAX_ENTRY: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Json,
    Csv,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Zip, Format::Json, Format::Csv];

    pub fn extension(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::Json => "json",
            Format::Csv => "csv",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "Backup archive (.zip): everything, can be restored",
            Format::Json => "JSON (.json): text and details, no images or files",
            Format::Csv => "Spreadsheet (.csv): one row per item",
        }
    }

    pub fn parse(s: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.extension().eq_ignore_ascii_case(s.trim_start_matches('.')))
    }

    pub fn from_path(path: &Path) -> Option<Format> {
        path.extension().and_then(|e| e.to_str()).and_then(Format::parse)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    exported_at: String,
    app_version: String,
    pinboards: Vec<BoardOut>,
    items: Vec<ItemOut>,
    #[serde(default)]
    link_previews: Vec<PreviewOut>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BoardOut {
    name: String,
    /// ARGB, for example "#FFFF3B30".
    color: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct RepOut {
    index: u32,
    uti: String,
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ItemOut {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    text: String,
    meta: ItemMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_bundle_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<String>,
    /// Unix milliseconds, plus a readable copy.
    created_at: i64,
    created: String,
    timestamp: i64,
    use_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_used_at: Option<i64>,
    in_history: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pinboards: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ocr: Option<String>,
    representations: Vec<RepOut>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PreviewOut {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon_sha256: Option<String>,
}

#[derive(Debug, Default)]
pub struct ExportSummary {
    pub items: usize,
    pub skipped_sensitive: usize,
    pub files: usize,
    pub bytes: u64,
}

impl ExportSummary {
    pub fn describe(&self, path: &Path) -> String {
        let mut s = format!("Exported {} items to {}", self.items, path.display());
        if self.files > 0 {
            s.push_str(&format!(" ({} data files, {:.1} MB)", self.files, self.bytes as f64 / 1_048_576.0));
        }
        if self.skipped_sensitive > 0 {
            s.push_str(&format!("; {} sensitive items were left out", self.skipped_sensitive));
        }
        s
    }
}

#[derive(Debug, Default)]
pub struct RestoreSummary {
    pub added: usize,
    pub already_present: usize,
    pub failed: usize,
    pub pinboards_created: usize,
}

impl RestoreSummary {
    pub fn describe(&self) -> String {
        format!(
            "Restored {} items; {} were already in history; {} could not be restored; {} pinboards created.",
            self.added, self.already_present, self.failed, self.pinboards_created
        )
    }
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn iso(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms).single().map(|d| d.to_rfc3339()).unwrap_or_default()
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

/// Writes `path` atomically: to a temporary file first, then renamed.
fn write_atomic(path: &Path, f: impl FnOnce(&mut std::fs::File) -> Result<()>) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir).with_context(|| format!("writing to {}", dir.display()))?;
    f(tmp.as_file_mut())?;
    persist(tmp, path)
}

/// Moves a finished temporary file into place with normal file permissions
/// (temporary files are created readable by the owner only).
fn persist(tmp: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tmp.as_file().set_permissions(std::fs::Permissions::from_mode(0o644))?;
    tmp.persist(path).with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

/// Item ids to export: everything, or what `query` matches.
fn selected_ids(store: &Store, query: Option<&Query>) -> Result<Option<HashSet<i64>>> {
    let Some(q) = query else { return Ok(None) };
    Ok(Some(store.search(q, usize::MAX >> 1, 0)?.into_iter().map(|i| i.id).collect()))
}

pub fn export(store: &Store, path: &Path, format: Format, query: Option<&Query>) -> Result<ExportSummary> {
    let ids = selected_ids(store, query)?;
    let mut rows: Vec<_> =
        store.export_rows()?.into_iter().filter(|r| ids.as_ref().is_none_or(|s| s.contains(&r.id))).collect();
    // Sensitive items only live for minutes and are never written to exports.
    let before = rows.len();
    rows.retain(|r| !r.sensitive);
    let skipped_sensitive = before - rows.len();
    let board_names = store.item_pinboard_names()?;
    let mut summary = ExportSummary { items: rows.len(), skipped_sensitive, ..ExportSummary::default() };

    if format == Format::Csv {
        write_atomic(path, |f| {
            writeln!(f, "id,created,last_copied,type,app,title,pinboards,text")?;
            for r in &rows {
                let boards = board_names.get(&r.id).map(|b| b.join("; ")).unwrap_or_default();
                let line = [
                    r.id.to_string(),
                    iso(r.created_at),
                    iso(r.timestamp),
                    r.kind.clone(),
                    r.app_name.clone().unwrap_or_default(),
                    r.title.clone().unwrap_or_default(),
                    boards,
                    r.text.clone(),
                ]
                .iter()
                .map(|v| csv_field(v))
                .collect::<Vec<_>>()
                .join(",");
                writeln!(f, "{line}")?;
            }
            Ok(())
        })?;
        return Ok(summary);
    }

    let with_data = format == Format::Zip;
    let mut written: HashSet<String> = HashSet::new();
    // The archive is written next to its destination, then renamed into place.
    let tmp_zip = if with_data {
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        Some(tempfile::NamedTempFile::new_in(dir)?)
    } else {
        None
    };
    let mut zip = tmp_zip.as_ref().map(|t| t.reopen().map(zip::ZipWriter::new)).transpose()?;
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
    let mut add_data = |zip: &mut Option<zip::ZipWriter<std::fs::File>>, data: &[u8]| -> Result<String> {
        let hash = sha256_hex(data);
        if let Some(z) = zip.as_mut()
            && written.insert(hash.clone())
        {
            z.start_file(format!("data/{hash}"), stored)?;
            z.write_all(data)?;
            summary.files += 1;
            summary.bytes += data.len() as u64;
        }
        Ok(hash)
    };

    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let reps = store.representations(r.id)?;
        let mut out = Vec::with_capacity(reps.len());
        for rep in &reps {
            let sha256 = if with_data { Some(add_data(&mut zip, &rep.data)?) } else { None };
            out.push(RepOut { index: rep.index, uti: rep.uti.clone(), size: rep.data.len() as u64, sha256 });
        }
        items.push(ItemOut {
            kind: r.kind.clone(),
            title: r.title.clone(),
            text: r.text.clone(),
            meta: serde_json::from_str(&r.meta).unwrap_or_default(),
            app_bundle_id: r.app_bundle_id.clone(),
            app_name: r.app_name.clone(),
            device: r.device.clone(),
            created_at: r.created_at,
            created: iso(r.created_at),
            timestamp: r.timestamp,
            use_count: r.use_count,
            last_used_at: r.last_used_at,
            in_history: r.in_history,
            pinboards: board_names.get(&r.id).cloned().unwrap_or_default(),
            ocr: r.ocr.clone().filter(|o| !o.is_empty()),
            representations: out,
        });
    }

    let exported_urls: HashSet<&str> = rows.iter().filter_map(|r| r.url.as_deref()).collect();
    let mut link_previews = Vec::new();
    for p in store.all_link_previews()? {
        if !exported_urls.contains(p.url.as_str()) {
            continue;
        }
        let image_sha256 = match (&p.image, with_data) {
            (Some(i), true) => Some(add_data(&mut zip, i)?),
            _ => None,
        };
        let icon_sha256 = match (&p.icon, with_data) {
            (Some(i), true) => Some(add_data(&mut zip, i)?),
            _ => None,
        };
        link_previews.push(PreviewOut {
            url: p.url,
            title: p.title,
            description: p.description,
            image_sha256,
            icon_sha256,
        });
    }

    let manifest = Manifest {
        format: FORMAT_NAME.into(),
        version: FORMAT_VERSION,
        exported_at: iso(crate::model::now_ms()),
        app_version: env!("CARGO_PKG_VERSION").into(),
        pinboards: store
            .pinboards()?
            .into_iter()
            .map(|b| BoardOut { name: b.name, color: format!("#{:08X}", b.color) })
            .collect(),
        items,
        link_previews,
    };
    let json = serde_json::to_vec_pretty(&manifest)?;
    match zip {
        Some(mut z) => {
            z.start_file(MANIFEST, zip::write::SimpleFileOptions::default())?;
            z.write_all(&json)?;
            z.finish()?;
            if let Some(t) = tmp_zip {
                persist(t, path)?;
            }
        }
        None => write_atomic(path, |f| Ok(f.write_all(&json)?))?,
    }
    Ok(summary)
}

fn read_entry(archive: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<Option<Vec<u8>>> {
    let Ok(mut file) = archive.by_name(name) else { return Ok(None) };
    if file.size() > MAX_ENTRY {
        bail!("{name} is too large");
    }
    let mut data = Vec::with_capacity(file.size() as usize);
    (&mut file).take(MAX_ENTRY + 1).read_to_end(&mut data)?;
    Ok(Some(data))
}

/// Reads `data/<sha256>` and checks that the content matches its name.
fn read_data(archive: &mut zip::ZipArchive<std::fs::File>, sha: &str) -> Result<Option<Vec<u8>>> {
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid data reference {sha:?}");
    }
    let Some(data) = read_entry(archive, &format!("data/{sha}"))? else { return Ok(None) };
    if sha256_hex(&data) != sha.to_ascii_lowercase() {
        bail!("data/{sha} is corrupt (content does not match its hash)");
    }
    Ok(Some(data))
}

fn parse_argb(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim_start_matches('#'), 16).ok()
}

/// Restores a ZIP backup made by `export`. Items whose content is already in
/// the store are skipped, so restoring twice is harmless.
pub fn restore(store: &mut Store, path: &Path) -> Result<RestoreSummary> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("not a ZIP archive")?;
    let manifest_bytes =
        read_entry(&mut archive, MANIFEST)?.context("this archive has no items.json; is it a Pasted backup?")?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes).context("reading items.json")?;
    if manifest.format != FORMAT_NAME || manifest.version > FORMAT_VERSION {
        bail!("unsupported backup format {} version {}", manifest.format, manifest.version);
    }
    let mut summary = RestoreSummary::default();
    // Only content that existed before this restore counts as "already present",
    // so items that are duplicates within the backup itself are kept.
    let existing = store.all_checksums()?;
    let colors: HashMap<String, u32> =
        manifest.pinboards.iter().filter_map(|b| Some((b.name.clone(), parse_argb(&b.color)?))).collect();
    // Keep the archive's pinboard order for boards that do not exist yet.
    for b in &manifest.pinboards {
        if store.pinboard_by_name(&b.name)?.is_none() {
            store.create_pinboard(&b.name, colors.get(&b.name).copied(), None)?;
            summary.pinboards_created += 1;
        }
    }

    store.batch(|store| {
        for item in &manifest.items {
            let mut reps = Vec::with_capacity(item.representations.len());
            let mut complete = true;
            for r in &item.representations {
                match r.sha256.as_deref().map(|sha| read_data(&mut archive, sha)) {
                    Some(Ok(Some(data))) => reps.push(Representation { index: r.index, uti: r.uti.clone(), data }),
                    _ => complete = false,
                }
            }
            let kind = ItemKind::parse(&item.kind).unwrap_or(ItemKind::Text);
            if !complete || reps.is_empty() {
                // Text survives even without its data files; images and files do not.
                if matches!(kind, ItemKind::Image | ItemKind::File) || item.text.is_empty() {
                    summary.failed += 1;
                    continue;
                }
                reps =
                    vec![Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: item.text.clone().into_bytes() }];
            }
            if existing.contains(&crate::store::checksum(&reps)) {
                summary.already_present += 1;
                continue;
            }
            let new = NewItem {
                kind,
                title: item.title.clone(),
                text: item.text.clone(),
                meta: item.meta.clone(),
                representations: reps,
                source_app: item
                    .app_bundle_id
                    .clone()
                    .map(|bundle_id| SourceApp { bundle_id, name: item.app_name.clone() }),
                device: item.device.clone(),
                created_at: item.created_at,
                timestamp: item.timestamp,
                external_id: None,
                in_history: item.in_history || item.pinboards.is_empty(),
            };
            let id = store.insert(&new, false)?.id();
            for name in &item.pinboards {
                let board = match store.pinboard_by_name(name)? {
                    Some(b) => b,
                    None => {
                        summary.pinboards_created += 1;
                        store.create_pinboard(name, colors.get(name).copied(), None)?
                    }
                };
                store.pin(id, board, None)?;
            }
            if let Some(ocr) = &item.ocr {
                store.set_ocr(id, ocr)?;
            }
            store.set_usage(id, item.use_count, item.last_used_at)?;
            summary.added += 1;
            if summary.added.is_multiple_of(200) {
                store.commit_progress()?;
            }
        }
        Ok(())
    })?;

    for p in &manifest.link_previews {
        let image = p.image_sha256.as_deref().and_then(|s| read_data(&mut archive, s).ok().flatten());
        let icon = p.icon_sha256.as_deref().and_then(|s| read_data(&mut archive, s).ok().flatten());
        store.import_link_preview(&crate::link_preview::LinkPreview {
            url: p.url.clone(),
            title: p.title.clone(),
            description: p.description.clone(),
            image,
            icon,
        })?;
    }
    Ok(summary)
}

/// A default file name such as "Pasted Backup 2026-10-10.zip".
pub fn default_file_name(format: Format) -> PathBuf {
    let date = chrono::Local::now().format("%Y-%m-%d");
    let stem = if format == Format::Zip { "Pasted Backup" } else { "Pasted Export" };
    PathBuf::from(format!("{stem} {date}.{}", format.extension()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{temp_store, text_item};

    fn image_item(ts: i64) -> NewItem {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&2u32.to_be_bytes());
        png.extend_from_slice(&3u32.to_be_bytes());
        png.extend(vec![7u8; 100_000]);
        let mut item = text_item("placeholder", ts);
        item.kind = ItemKind::Image;
        item.text = String::new();
        item.representations = vec![Representation { index: 0, uti: "public.png".into(), data: png }];
        item
    }

    fn populated() -> (tempfile::TempDir, Store) {
        let (d, mut s) = temp_store();
        s.insert(&text_item("hello, \"world\"\nline two", 1_000), true).unwrap();
        // The same content twice (allowed for imported history) must survive a round trip.
        s.insert(&text_item("twice", 1_500), false).unwrap();
        s.insert(&text_item("twice", 1_600), false).unwrap();
        let link = s.insert(&text_item("https://example.com/a", 2_000), true).unwrap().id();
        let img = s.insert(&image_item(3_000), true).unwrap().id();
        let board = s.create_pinboard("Work", Some(0xFF34C759), None).unwrap();
        s.pin(img, board, None).unwrap();
        s.set_ocr(img, "receipt total").unwrap();
        s.mark_used(link).unwrap();
        s.set_link_preview(
            &crate::link_preview::LinkPreview {
                url: "https://example.com/a".into(),
                title: Some("Example A".into()),
                icon: Some(vec![1, 2, 3]),
                ..Default::default()
            },
            "ok",
        )
        .unwrap();
        (d, s)
    }

    #[test]
    fn zip_round_trip_is_lossless_and_idempotent() {
        let (dir, source) = populated();
        let path = dir.path().join("backup.zip");
        let summary = export(&source, &path, Format::Zip, None).unwrap();
        assert_eq!(summary.items, 5);
        assert!(summary.files >= 4, "text, link, image and icon data");

        let (_d2, mut target) = temp_store();
        let r = restore(&mut target, &path).unwrap();
        assert_eq!((r.added, r.already_present, r.failed), (5, 0, 0));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);
        let items = target.search(&Query::default(), 10, 0).unwrap();
        assert_eq!(items.len(), 5);
        let img = items.iter().find(|i| i.kind == ItemKind::Image).unwrap();
        assert_eq!(target.representations(img.id).unwrap()[0].data.len(), 100_024);
        assert_eq!(target.search(&Query::text("receipt"), 10, 0).unwrap().len(), 1);
        assert_eq!(target.search(&Query::parse("board:work"), 10, 0).unwrap().len(), 1);
        assert_eq!(target.pinboards().unwrap()[0].color, 0xFF34C759);
        let link = items.iter().find(|i| i.kind == ItemKind::Link).unwrap();
        assert_eq!(link.use_count, 1);
        assert_eq!(link.created_at, 2_000);
        assert_eq!(target.link_preview("https://example.com/a").unwrap().unwrap().title.as_deref(), Some("Example A"));

        let again = restore(&mut target, &path).unwrap();
        assert_eq!((again.added, again.already_present), (0, 5));
    }

    #[test]
    fn corrupt_data_is_rejected() {
        let (dir, source) = populated();
        let path = dir.path().join("backup.zip");
        export(&source, &path, Format::Zip, None).unwrap();
        // Rewrite the archive with one data file's content changed.
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        let bad = dir.path().join("bad.zip");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&bad).unwrap());
        for i in 0..archive.len() {
            let mut f = archive.by_index(i).unwrap();
            let name = f.name().unwrap().to_string();
            let mut data = Vec::new();
            f.read_to_end(&mut data).unwrap();
            if name.starts_with("data/") && data.len() > 50_000 {
                data[20] ^= 0xff;
            }
            w.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(&data).unwrap();
        }
        w.finish().unwrap();
        let (_d2, mut target) = temp_store();
        let r = restore(&mut target, &bad).unwrap();
        assert_eq!((r.added, r.failed), (4, 1), "the image with corrupt data is not restored");
    }

    #[test]
    fn json_and_csv() {
        let (dir, source) = populated();
        let json = dir.path().join("out.json");
        let s = export(&source, &json, Format::Json, None).unwrap();
        assert_eq!((s.items, s.files), (5, 0));
        let m: Manifest = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
        assert_eq!(m.items.len(), 5);
        assert!(m.items.iter().all(|i| i.representations.iter().all(|r| r.sha256.is_none())));

        let csv = dir.path().join("out.csv");
        export(&source, &csv, Format::Csv, Some(&Query::parse("type:text"))).unwrap();
        let text = std::fs::read_to_string(&csv).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "id,created,last_copied,type,app,title,pinboards,text");
        assert!(text.contains("\"hello, \"\"world\"\"\nline two\""), "{text}");
        assert!(!text.contains("example.com"), "the filter keeps only text items");
    }

    #[test]
    fn sensitive_items_are_not_exported() {
        let (dir, mut s) = temp_store();
        s.insert(&text_item("keep me", 1_000), true).unwrap();
        let secret = s.insert(&text_item("one-time code 123456", 2_000), true).unwrap().id();
        s.mark_sensitive(secret, crate::model::now_ms() + 60_000).unwrap();
        let path = dir.path().join("out.csv");
        let summary = export(&s, &path, Format::Csv, None).unwrap();
        assert_eq!((summary.items, summary.skipped_sensitive), (1, 1));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("123456"));
        assert!(summary.describe(&path).contains("1 sensitive items were left out"));
    }

    #[test]
    fn formats() {
        assert_eq!(Format::from_path(Path::new("a/b.ZIP")), Some(Format::Zip));
        assert_eq!(Format::parse(".csv"), Some(Format::Csv));
        assert_eq!(Format::from_path(Path::new("x.txt")), None);
        assert!(default_file_name(Format::Zip).to_string_lossy().starts_with("Pasted Backup "));
    }
}
