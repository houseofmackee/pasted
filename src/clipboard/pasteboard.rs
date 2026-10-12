//! Reading and writing `NSPasteboard`.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting, NSWorkspace};
use objc2_foundation::{NSArray, NSData, NSString};

use crate::model::{Representation, SourceApp, UTI_PLAIN_TEXT};

/// Marker so other clipboard tools know who wrote the pasteboard.
pub const SOURCE_TYPE: &str = "org.nspasteboard.source";
pub const BUNDLE_ID: &str = "io.github.houseofmackee.pasted";

pub fn general() -> Retained<NSPasteboard> {
    NSPasteboard::generalPasteboard()
}

/// Legacy aliases such as "NeXT plain ascii pasteboard type" duplicate modern
/// UTIs; macOS synthesizes them, so storing them only wastes space.
fn is_legacy_alias(uti: &str) -> bool {
    uti.contains(' ')
}

/// All types across every pasteboard item.
pub fn all_types(pb: &NSPasteboard) -> Vec<String> {
    pb.types().map(|types| types.iter().map(|t| t.to_string()).collect()).unwrap_or_default()
}

pub enum ReadError {
    TooLarge(usize),
    Empty,
}

/// Reads every representation of every item, stopping once `max_bytes` is exceeded.
pub fn read(pb: &NSPasteboard, max_bytes: usize) -> Result<Vec<Representation>, ReadError> {
    let Some(items) = pb.pasteboardItems() else { return Err(ReadError::Empty) };
    let mut out = Vec::new();
    let mut total = 0usize;
    for (index, item) in items.iter().enumerate() {
        for uti in item.types().iter() {
            let uti_str = uti.to_string();
            if is_legacy_alias(&uti_str) || uti_str == SOURCE_TYPE {
                continue;
            }
            let Some(data) = item.dataForType(&uti) else { continue };
            total += data.len();
            if total > max_bytes {
                return Err(ReadError::TooLarge(total));
            }
            out.push(Representation { index: index as u32, uti: uti_str, data: data.to_vec() });
        }
    }
    let out = crate::model::without_redundant_utf16(out);
    if out.is_empty() { Err(ReadError::Empty) } else { Ok(out) }
}

/// Replaces the pasteboard contents. Returns the new change count.
pub fn write(pb: &NSPasteboard, reps: &[Representation]) -> isize {
    pb.clearContents();
    let mut indices: Vec<u32> = reps.iter().map(|r| r.index).collect();
    indices.dedup();
    let mut objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = Vec::new();
    for (n, index) in indices.iter().enumerate() {
        let item = NSPasteboardItem::new();
        for r in reps.iter().filter(|r| r.index == *index) {
            item.setData_forType(&NSData::with_bytes(&r.data), &NSString::from_str(&r.uti));
        }
        if n == 0 {
            item.setString_forType(&NSString::from_str(BUNDLE_ID), &NSString::from_str(SOURCE_TYPE));
        }
        objects.push(ProtocolObject::from_retained(item));
    }
    pb.writeObjects(&NSArray::from_retained_slice(&objects));
    pb.changeCount()
}

pub fn write_text(pb: &NSPasteboard, text: &str) -> isize {
    write(pb, &[Representation { index: 0, uti: UTI_PLAIN_TEXT.into(), data: text.as_bytes().to_vec() }])
}

pub fn frontmost_app() -> Option<SourceApp> {
    let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    let bundle_id = app.bundleIdentifier()?.to_string();
    Some(SourceApp { bundle_id, name: app.localizedName().map(|n| n.to_string()) })
}
