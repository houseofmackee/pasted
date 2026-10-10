//! Decoding of Paste's stored pasteboard blobs.
//!
//! `ZRAWPASTEBOARDITEMS` starts with a Core Data marker byte:
//! `0x02` + NUL terminated UUID means the data lives in
//! `.db_SUPPORT/_EXTERNAL_DATA/<UUID>`; `0x01` means inline data follows.
//! The payload is JSON (older versions), raw deflate, LZFSE (`bvx2`/`bvxn`)
//! or a binary plist. All decode to an array of
//! `{ "types": [uti], "dataByType": { uti: data } }`.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;

use crate::model::Representation;

const MAX_DEPTH: usize = 4;

pub fn decode_item_blob(raw: &[u8], external_dir: &Path) -> Result<Vec<Representation>> {
    match raw.first() {
        Some(0x02) => {
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            let name = std::str::from_utf8(&raw[1..end]).context("external reference is not UTF-8")?;
            if name.contains('/') || name.contains("..") {
                bail!("suspicious external reference {name:?}");
            }
            let path = external_dir.join(name);
            let data = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            decode_payload(&data, 0)
        }
        Some(0x01) => decode_payload(&raw[1..], 0),
        Some(_) => decode_payload(raw, 0),
        None => bail!("empty blob"),
    }
}

fn decode_payload(data: &[u8], depth: usize) -> Result<Vec<Representation>> {
    if depth > MAX_DEPTH {
        bail!("blob nested too deeply");
    }
    if data.starts_with(b"bvx") {
        let mut out = Vec::new();
        lzfse_rust::decode_bytes(data, &mut out).map_err(|e| anyhow!("LZFSE: {e:?}"))?;
        return decode_payload(&out, depth + 1);
    }
    if data.starts_with(b"bplist") {
        let value = plist::Value::from_reader(std::io::Cursor::new(data)).context("binary plist")?;
        return from_plist(&value);
    }
    let first = data.iter().find(|b| !b.is_ascii_whitespace());
    if first == Some(&b'[') || first == Some(&b'{') {
        let value: serde_json::Value = serde_json::from_slice(data).context("JSON")?;
        return from_json(&value);
    }
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(data)
        .read_to_end(&mut out)
        .context("unknown blob encoding (not LZFSE, plist, JSON or deflate)")?;
    decode_payload(&out, depth + 1)
}

/// Pasteboard items in `types` order; types without data are skipped.
fn collect<V>(
    items: impl Iterator<Item = (Vec<String>, Vec<(String, V)>)>,
    to_bytes: impl Fn(&V) -> Option<Vec<u8>>,
) -> Vec<Representation> {
    let mut reps = Vec::new();
    for (index, (types, data)) in items.enumerate() {
        let mut ordered: Vec<&String> = types.iter().collect();
        for (uti, _) in &data {
            if !ordered.contains(&uti) {
                ordered.push(uti);
            }
        }
        for uti in ordered {
            if let Some((_, v)) = data.iter().find(|(u, _)| u == uti)
                && let Some(bytes) = to_bytes(v)
            {
                reps.push(Representation { index: index as u32, uti: uti.clone(), data: bytes });
            }
        }
    }
    reps
}

fn from_plist(value: &plist::Value) -> Result<Vec<Representation>> {
    let items = value.as_array().ok_or_else(|| anyhow!("plist root is not an array"))?;
    let parsed = items.iter().filter_map(|item| {
        let dict = item.as_dictionary()?;
        let types = dict
            .get("types")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_string().map(str::to_string)).collect())
            .unwrap_or_default();
        let data = dict
            .get("dataByType")
            .and_then(|d| d.as_dictionary())
            .map(|d| d.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        Some((types, data))
    });
    Ok(collect(parsed, |v: &plist::Value| match v {
        plist::Value::Data(d) => Some(d.clone()),
        plist::Value::String(s) => Some(s.as_bytes().to_vec()),
        _ => None,
    }))
}

fn from_json(value: &serde_json::Value) -> Result<Vec<Representation>> {
    let items = match value {
        serde_json::Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    let parsed = items.into_iter().map(|item| {
        let types = item
            .get("types")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let data = item
            .get("dataByType")
            .and_then(|d| d.as_object())
            .map(|d| d.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        (types, data)
    });
    Ok(collect(parsed, |v: &serde_json::Value| {
        let s = v.as_str()?;
        Some(base64::engine::general_purpose::STANDARD.decode(s).unwrap_or_else(|_| s.as_bytes().to_vec()))
    }))
}

/// Paste's stored link preview (`ZRAWPREVIEW`): `urlName`, `urlImageData` and
/// `urlIconData`, as JSON (older versions, data base64 encoded) or a binary plist.
pub fn decode_preview(raw: &[u8], external_dir: &Path) -> Option<crate::link_preview::LinkPreview> {
    let body: Vec<u8> = match raw.first()? {
        0x02 => {
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            let name = std::str::from_utf8(&raw[1..end]).ok()?;
            if name.contains('/') || name.contains("..") {
                return None;
            }
            std::fs::read(external_dir.join(name)).ok()?
        }
        0x01 => raw[1..].to_vec(),
        _ => raw.to_vec(),
    };
    let mut p = crate::link_preview::LinkPreview::default();
    let bytes_of_json = |v: &serde_json::Value| {
        v.as_str().and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok()).filter(|b| !b.is_empty())
    };
    if body.starts_with(b"bplist") {
        let value = plist::Value::from_reader(std::io::Cursor::new(&body)).ok()?;
        let dict = value.as_dictionary()?;
        let data = |k: &str| match dict.get(k) {
            Some(plist::Value::Data(d)) if !d.is_empty() => Some(d.clone()),
            Some(plist::Value::String(s)) => base64::engine::general_purpose::STANDARD.decode(s).ok(),
            _ => None,
        };
        p.title = dict.get("urlName").and_then(|v| v.as_string()).map(str::to_string);
        p.image = data("urlImageData");
        p.icon = data("urlIconData");
    } else {
        let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
        p.title = v.get("urlName").and_then(|n| n.as_str()).map(str::to_string);
        p.image = v.get("urlImageData").and_then(bytes_of_json);
        p.icon = v.get("urlIconData").and_then(bytes_of_json);
    }
    p.title = p.title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    Some(p)
}

/// Text from Paste's OCR metadata: `{"words":[{"string":..,"confidence":..}]}`.
pub fn ocr_text(metadata: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(metadata).ok()?;
    let words: Vec<&str> = v.get("words")?.as_array()?.iter().filter_map(|w| w.get("string")?.as_str()).collect();
    (!words.is_empty()).then(|| words.join(" "))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn sample_plist() -> Vec<u8> {
        let mut by_type = plist::Dictionary::new();
        by_type.insert("public.utf8-plain-text".into(), plist::Value::Data(b"hello".to_vec()));
        by_type.insert("public.html".into(), plist::Value::Data(b"<b>hello</b>".to_vec()));
        let mut item = plist::Dictionary::new();
        item.insert("types".into(), plist::Value::Array(vec!["public.html".into(), "public.utf8-plain-text".into()]));
        item.insert("dataByType".into(), plist::Value::Dictionary(by_type));
        let mut out = Vec::new();
        plist::Value::Array(vec![plist::Value::Dictionary(item)]).to_writer_binary(&mut out).unwrap();
        out
    }

    fn check(reps: &[Representation]) {
        assert_eq!(reps.len(), 2);
        assert_eq!(reps[0].uti, "public.html");
        assert_eq!(reps[1].data, b"hello");
    }

    #[test]
    fn inline_plist() {
        let mut blob = vec![1];
        blob.extend(sample_plist());
        check(&decode_item_blob(&blob, Path::new("/nonexistent")).unwrap());
    }

    #[test]
    fn lzfse_plist() {
        let mut compressed = Vec::new();
        lzfse_rust::encode_bytes(&sample_plist(), &mut compressed).unwrap();
        let mut blob = vec![1];
        blob.extend(compressed);
        check(&decode_item_blob(&blob, Path::new("/nonexistent")).unwrap());
    }

    #[test]
    fn deflate_json() {
        let json = br#"[{"types":["public.html","public.utf8-plain-text"],"dataByType":{"public.utf8-plain-text":"aGVsbG8=","public.html":"PGI+aGVsbG88L2I+"}}]"#;
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(json).unwrap();
        let mut blob = vec![1];
        blob.extend(enc.finish().unwrap());
        check(&decode_item_blob(&blob, Path::new("/nonexistent")).unwrap());
    }

    #[test]
    fn external_reference() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ABC-123"), sample_plist()).unwrap();
        let mut blob = vec![2];
        blob.extend(b"ABC-123\0");
        check(&decode_item_blob(&blob, dir.path()).unwrap());
        let mut evil = vec![2];
        evil.extend(b"../etc/passwd\0");
        assert!(decode_item_blob(&evil, dir.path()).is_err());
    }

    #[test]
    fn previews() {
        let mut d = plist::Dictionary::new();
        d.insert("type".into(), "link".into());
        d.insert("urlName".into(), " Example Site ".into());
        d.insert("urlIconData".into(), plist::Value::Data(vec![1, 2, 3]));
        let mut raw = vec![1];
        plist::Value::Dictionary(d).to_writer_binary(&mut raw).unwrap();
        let p = decode_preview(&raw, Path::new("/nonexistent")).unwrap();
        assert_eq!(p.title.as_deref(), Some("Example Site"));
        assert_eq!(p.icon, Some(vec![1, 2, 3]));
        assert!(p.image.is_none());

        let json = br#"{"type":"link","url":"https://x.com","urlName":"X","urlImageData":"AQID"}"#;
        let mut raw = vec![1];
        raw.extend_from_slice(json);
        let p = decode_preview(&raw, Path::new("/nonexistent")).unwrap();
        assert_eq!((p.title.as_deref(), p.image), (Some("X"), Some(vec![1, 2, 3])));
    }

    #[test]
    fn ocr() {
        let m = br#"{"words":[{"string":"Hello","confidence":1,"boundingBox":[]},{"string":"World","confidence":1}]}"#;
        assert_eq!(ocr_text(m).as_deref(), Some("Hello World"));
        assert_eq!(ocr_text(b"{}"), None);
    }
}
