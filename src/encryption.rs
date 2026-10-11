//! Opt-in encryption at rest.
//!
//! One random 32-byte master key lives in the login Keychain. From it we
//! derive (HMAC-SHA256) a SQLCipher key for the database, an AES-256-GCM key
//! for blob files, and a key for blob file names, so names do not reveal
//! content hashes.
//!
//! Whether history is encrypted is read from the files themselves (an
//! encrypted database does not start with the SQLite header; sealed blobs
//! start with `MAGIC`), so there is no separate flag that could disagree.

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result, anyhow, bail};
use hmac::{Hmac, Mac};
use rusqlite::Connection;
use sha2::Sha256;

const MAGIC: &[u8; 8] = b"PSTDENC1";

/// Background workers hold this for reading while they use the database;
/// switching encryption on or off takes it for writing, so no other
/// connection is open during the switch.
pub static DB_GATE: std::sync::RwLock<()> = std::sync::RwLock::new(());
const NONCE_LEN: usize = 12;
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const KEYCHAIN_ACCOUNT: &str = "history-encryption-key";

fn keychain_service() -> String {
    // Tests and experiments can use a separate Keychain item.
    std::env::var("PASTED_KEYCHAIN_SERVICE").unwrap_or_else(|_| "io.pasted.app".into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    s.len()
        .is_multiple_of(2)
        .then(|| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect())
        .flatten()
}

#[derive(Clone)]
pub struct Keys {
    db: [u8; 32],
    blob: [u8; 32],
    name: [u8; 32],
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keys(..)")
    }
}

fn derive(master: &[u8; 32], label: &str) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(master).expect("HMAC takes any key length");
    mac.update(b"pasted:");
    mac.update(label.as_bytes());
    mac.finalize().into_bytes().into()
}

impl Keys {
    pub fn from_master(master: &[u8; 32]) -> Keys {
        Keys { db: derive(master, "database"), blob: derive(master, "blob"), name: derive(master, "blob-name") }
    }

    /// SQLCipher raw key literal: `x'...'` (skips SQLCipher's own key derivation).
    pub fn sqlcipher_key(&self) -> String {
        format!("x'{}'", hex(&self.db))
    }

    /// Applies the key to a fresh connection; must run before anything else.
    pub fn apply(&self, conn: &Connection) -> Result<()> {
        conn.execute_batch(&format!("PRAGMA key = \"{}\";", self.sqlcipher_key()))?;
        Ok(())
    }

    /// File name for blob content: keyed, so it does not reveal the content hash.
    pub fn blob_name(&self, data: &[u8]) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.name).expect("any key length");
        mac.update(data);
        hex(&mac.finalize().into_bytes())
    }

    pub fn seal(&self, data: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|e| anyhow!("random nonce: {e}"))?;
        let cipher = Aes256Gcm::new_from_slice(&self.blob).expect("32-byte key");
        let sealed = cipher.encrypt(Nonce::from_slice(&nonce), data).map_err(|_| anyhow!("encryption failed"))?;
        let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + sealed.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        if !is_sealed(sealed) || sealed.len() < MAGIC.len() + NONCE_LEN {
            bail!("not an encrypted Pasted file");
        }
        let nonce = &sealed[MAGIC.len()..MAGIC.len() + NONCE_LEN];
        let cipher = Aes256Gcm::new_from_slice(&self.blob).expect("32-byte key");
        cipher
            .decrypt(Nonce::from_slice(nonce), &sealed[MAGIC.len() + NONCE_LEN..])
            .map_err(|_| anyhow!("decryption failed: wrong key or damaged file"))
    }
}

pub fn is_sealed(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

pub fn new_master_key() -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| anyhow!("random key: {e}"))?;
    Ok(key)
}

/// True if the database file exists and is not plain SQLite.
pub fn database_is_encrypted(db: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 16];
    match std::fs::File::open(db).and_then(|mut f| f.read_exact(&mut header)) {
        Ok(()) => &header != SQLITE_HEADER,
        Err(_) => false,
    }
}

// Keychain

pub fn keychain_load() -> Result<Option<Keys>> {
    match security_framework::passwords::get_generic_password(&keychain_service(), KEYCHAIN_ACCOUNT) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes).context("Keychain item is not text")?;
            let master: [u8; 32] =
                unhex(text.trim()).and_then(|v| v.try_into().ok()).context("Keychain item is not a 32-byte key")?;
            Ok(Some(Keys::from_master(&master)))
        }
        Err(e) if e.code() == -25300 => Ok(None), // errSecItemNotFound
        Err(e) => Err(anyhow!("reading the encryption key from the Keychain: {e}")),
    }
}

fn keychain_save(master: &[u8; 32]) -> Result<()> {
    security_framework::passwords::set_generic_password(&keychain_service(), KEYCHAIN_ACCOUNT, hex(master).as_bytes())
        .map_err(|e| anyhow!("saving the encryption key to the Keychain: {e}"))
}

pub fn keychain_delete() -> Result<()> {
    match security_framework::passwords::delete_generic_password(&keychain_service(), KEYCHAIN_ACCOUNT) {
        Ok(()) => Ok(()),
        Err(e) if e.code() == -25300 => Ok(()),
        Err(e) => Err(anyhow!("removing the encryption key from the Keychain: {e}")),
    }
}

// Switching

#[derive(Debug, Default)]
pub struct SwitchSummary {
    pub blobs: usize,
    pub removed_files: usize,
}

fn sql_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

fn remove_wal(db: &Path) {
    for suffix in ["-wal", "-shm"] {
        let mut p = db.as_os_str().to_owned();
        p.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
}

fn blob_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(&name[..2]).join(name)
}

fn write_file(path: &Path, data: &[u8]) -> Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Copies the database to `target` with a different key (or none), rewrites
/// blob files for the new mode, then swaps the copy into place. Old files are
/// removed only after the swap. Every connection to the database must be
/// closed while this runs.
fn switch(db: &Path, blobs: &Path, from: Option<&Keys>, to: Option<&Keys>) -> Result<SwitchSummary> {
    let mut summary = SwitchSummary::default();
    let target = db.with_extension("db.switching");
    let _ = std::fs::remove_file(&target);
    remove_wal(&target);

    {
        let conn = Connection::open(db)?;
        if let Some(k) = from {
            k.apply(&conn)?;
        }
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).context("opening the database")?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let key = to.map(|k| k.sqlcipher_key()).unwrap_or_default();
        conn.execute_batch(&format!("ATTACH DATABASE {} AS target KEY \"{key}\";", sql_quote(&target)))?;
        conn.query_row("SELECT sqlcipher_export('target')", [], |_| Ok(()))?;
        conn.execute_batch(&format!("PRAGMA target.user_version = {version}; DETACH DATABASE target;"))?;
    }

    // Rewrite blobs for the new mode and point the copy at the new names.
    let mut renamed: Vec<(String, String)> = Vec::new();
    {
        let conn = Connection::open(&target)?;
        if let Some(k) = to {
            k.apply(&conn)?;
        }
        let names: Vec<String> = {
            let mut stmt = conn.prepare("SELECT DISTINCT blob FROM representations WHERE blob IS NOT NULL")?;
            stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        for old in names {
            let raw = std::fs::read(blob_path(blobs, &old)).with_context(|| format!("reading blob {old}"))?;
            let plain =
                if is_sealed(&raw) { from.context("found an encrypted file but no key")?.open(&raw)? } else { raw };
            let (new, content) = match to {
                Some(k) => (k.blob_name(&plain), k.seal(&plain)?),
                None => (hex(&<Sha256 as sha2::Digest>::digest(&plain)), plain),
            };
            if new != old {
                write_file(&blob_path(blobs, &new), &content)?;
                renamed.push((old, new));
            }
            summary.blobs += 1;
        }
        let tx = conn.unchecked_transaction()?;
        for (old, new) in &renamed {
            tx.execute("UPDATE representations SET blob = ?2 WHERE blob = ?1", [old, new])?;
        }
        tx.commit()?;
        let ok: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if ok != "ok" {
            bail!("the converted database failed its integrity check: {ok}");
        }
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    }

    // Swap in the converted copy, then remove what the old one used.
    remove_wal(db);
    remove_wal(&target);
    std::fs::rename(&target, db).context("replacing the database")?;
    for (old, _) in &renamed {
        if std::fs::remove_file(blob_path(blobs, old)).is_ok() {
            summary.removed_files += 1;
        }
    }
    Ok(summary)
}

/// Encrypts history with a new key, which is saved in the Keychain first.
pub fn enable(db: &Path, blobs: &Path) -> Result<SwitchSummary> {
    if database_is_encrypted(db) {
        bail!("history is already encrypted");
    }
    let master = new_master_key()?;
    keychain_save(&master)?;
    enable_with(db, blobs, &Keys::from_master(&master))
}

pub fn enable_with(db: &Path, blobs: &Path, keys: &Keys) -> Result<SwitchSummary> {
    switch(db, blobs, None, Some(keys))
}

/// Decrypts history and removes the key from the Keychain.
pub fn disable(db: &Path, blobs: &Path) -> Result<SwitchSummary> {
    if !database_is_encrypted(db) {
        bail!("history is not encrypted");
    }
    let keys = keychain_load()?.context("history is encrypted but its key is not in the Keychain")?;
    let summary = disable_with(db, blobs, &keys)?;
    keychain_delete()?;
    Ok(summary)
}

pub fn disable_with(db: &Path, blobs: &Path, keys: &Keys) -> Result<SwitchSummary> {
    switch(db, blobs, Some(keys), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Query;
    use crate::store::Store;

    /// Fixed vectors made with the original crates (aes-gcm 0.10, hmac 0.12,
    /// sha2 0.10) and checked with Python's hmac and Apple's CryptoKit. Any
    /// crate update must reproduce them, or existing encrypted history
    /// could no longer be opened.
    #[test]
    fn existing_encrypted_data_still_opens() {
        let k = Keys::from_master(&[7u8; 32]);
        assert_eq!(k.sqlcipher_key(), "x'a44d028f0c0650a4f41916b03be0771c836f586076006cdc87b6f435509571bb'");
        assert_eq!(k.blob_name(b"hello"), "5953e7b98974a142dde739df34290d794103807af755b611c04f91fc97a8764f");
        let sealed = "50535444454e4331cdffda96f005edc790cf05bbf417a0f280a193622a6593d01b6ffcc0cdf4dc4b1325f8aa6231e5a81a851a43f8bf";
        let bytes: Vec<u8> =
            (0..sealed.len()).step_by(2).map(|i| u8::from_str_radix(&sealed[i..i + 2], 16).unwrap()).collect();
        assert_eq!(k.open(&bytes).unwrap(), b"pasted golden blob");
        assert_eq!(k.open(&k.seal(b"round trip").unwrap()).unwrap(), b"round trip");
    }
    use crate::store::tests::text_item;

    fn keys() -> Keys {
        Keys::from_master(&new_master_key().unwrap())
    }

    #[test]
    fn seal_round_trip_and_tamper_detection() {
        let k = keys();
        let sealed = k.seal(b"secret clip").unwrap();
        assert!(is_sealed(&sealed));
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert_eq!(k.open(&sealed).unwrap(), b"secret clip");
        let mut bad = sealed.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(k.open(&bad).is_err());
        assert!(keys().open(&sealed).is_err(), "another key cannot open it");
        assert_ne!(k.blob_name(b"x"), keys().blob_name(b"x"));
        assert_eq!(unhex(&hex(&[0, 255, 16])).unwrap(), [0, 255, 16]);
    }

    #[test]
    fn enable_and_disable_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (db, blobs) = (dir.path().join("pasted.db"), dir.path().join("blobs"));
        let secret = "top secret quarterly numbers ".repeat(2000); // large enough for a blob file
        let (small_id, big_id) = {
            let mut s = Store::open(&db, blobs.clone()).unwrap();
            let small = s.insert(&text_item("hello encrypted world", 1), true).unwrap().id();
            let big = s.insert(&text_item(&secret, 2), true).unwrap().id();
            (small, big)
        };
        let find_plain = |needle: &[u8]| {
            walk(dir.path()).into_iter().any(|p| std::fs::read(&p).unwrap().windows(needle.len()).any(|w| w == needle))
        };
        assert!(find_plain(b"top secret quarterly"));

        let k = keys();
        let summary = enable_with(&db, &blobs, &k).unwrap();
        assert_eq!((summary.blobs, summary.removed_files), (1, 1));
        assert!(database_is_encrypted(&db));
        assert!(!find_plain(b"top secret quarterly"), "no plaintext left in any file");
        assert!(!find_plain(b"hello encrypted world"));
        assert!(Store::open(&db, blobs.clone()).is_err(), "opening without the key fails");

        {
            let s = Store::open_with(&db, blobs.clone(), Some(k.clone())).unwrap();
            assert!(s.is_encrypted());
            assert_eq!(s.search(&Query::text("encrypted"), 10, 0).unwrap()[0].id, small_id);
            assert_eq!(s.representations(big_id).unwrap()[0].data, secret.as_bytes());
            assert_eq!(s.search(&Query::text("quarterly"), 10, 0).unwrap().len(), 1);
        }

        disable_with(&db, &blobs, &k).unwrap();
        assert!(!database_is_encrypted(&db));
        let s = Store::open(&db, blobs.clone()).unwrap();
        assert_eq!(s.representations(big_id).unwrap()[0].data, secret.as_bytes());
        assert_eq!(s.search(&Query::text("hello"), 10, 0).unwrap().len(), 1);
        assert!(walk(&blobs).iter().all(|p| !is_sealed(&std::fs::read(p).unwrap())));
    }

    #[test]
    fn new_blobs_are_sealed_when_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let (db, blobs) = (dir.path().join("pasted.db"), dir.path().join("blobs"));
        let k = keys();
        Store::open(&db, blobs.clone()).unwrap();
        enable_with(&db, &blobs, &k).unwrap();
        let mut s = Store::open_with(&db, blobs.clone(), Some(k)).unwrap();
        let big = "x".repeat(100_000);
        let id = s.insert(&text_item(&big, 1), true).unwrap().id();
        let files = walk(&blobs);
        assert_eq!(files.len(), 1);
        assert!(is_sealed(&std::fs::read(&files[0]).unwrap()));
        assert_eq!(s.representations(id).unwrap()[0].data.len(), 100_000);
        assert_eq!(s.collect_garbage().unwrap(), 0);
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
}
