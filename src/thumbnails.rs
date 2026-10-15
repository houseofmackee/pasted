//! Previews of copied files (videos, PDFs, documents, images copied without
//! their data) made with Quick Look, so file cards show the content instead
//! of a generic icon. Stored in the database, so they outlive the file. Runs
//! on a background thread with its own database connection.

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSDictionary, NSError, NSString, NSURL};
use objc2_quick_look_thumbnailing::{
    QLThumbnailGenerationRequest, QLThumbnailGenerationRequestRepresentationTypes, QLThumbnailGenerator,
    QLThumbnailRepresentation,
};

use crate::store::Store;

const BATCH: usize = 8;
const IDLE: Duration = Duration::from_secs(15);
const BETWEEN_FILES: Duration = Duration::from_millis(100);
const TIMEOUT: Duration = Duration::from_secs(20);
/// Requested size in points (doubled for Retina screens), a bit larger than
/// a card's content area.
const SIZE: (f64, f64) = (260.0, 200.0);

/// A PNG preview of the file, or `None` when Quick Look has only an icon
/// for it (or the file is gone).
pub fn generate(path: &str) -> Result<Option<Vec<u8>>> {
    if !std::path::Path::new(path).exists() {
        return Ok(None);
    }
    let (tx, rx) = mpsc::channel();
    autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        // SAFETY: a valid file URL, size and representation types.
        let request = unsafe {
            QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
                QLThumbnailGenerationRequest::alloc(),
                &url,
                CGSize::new(SIZE.0, SIZE.1),
                2.0,
                QLThumbnailGenerationRequestRepresentationTypes::Thumbnail,
            )
        };
        let handler = RcBlock::new(move |thumb: *mut QLThumbnailRepresentation, _error: *mut NSError| {
            // SAFETY: Quick Look passes a valid representation or null.
            let png = unsafe { thumb.as_ref() }.and_then(|thumb| {
                // SAFETY: the representation is alive for the duration of the handler.
                let cg = unsafe { thumb.CGImage() };
                let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
                // SAFETY: an empty properties dictionary is valid for PNG.
                let data =
                    unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) };
                data.map(|d| d.to_vec())
            });
            let _ = tx.send(png);
        });
        // SAFETY: the handler is called once, on a Quick Look queue; it only sends on the channel.
        unsafe {
            QLThumbnailGenerator::sharedGenerator()
                .generateBestRepresentationForRequest_completionHandler(&request, &handler)
        };
    });
    rx.recv_timeout(TIMEOUT).map_err(|_| anyhow!("Quick Look did not answer for {path}"))
}

fn process_batch(store: &Store) -> Result<usize> {
    let items = store.items_needing_thumbnail(BATCH)?;
    for item in &items {
        let png = match item.meta.files.first().map(|f| generate(f)) {
            Some(Ok(png)) => png,
            Some(Err(e)) => {
                tracing::debug!("thumbnail failed for item {}: {e:#}", item.id);
                None
            }
            None => None,
        };
        // A missing thumbnail is recorded too, so the file is not tried again.
        store.set_thumbnail(item.id, png.as_deref())?;
        std::thread::sleep(BETWEEN_FILES);
    }
    Ok(items.len())
}

pub fn spawn_worker() {
    std::thread::Builder::new()
        .name("pasted-thumbnails".into())
        .spawn(|| {
            loop {
                // Open the database per batch so encryption can be switched in between.
                let result = {
                    let _gate = crate::encryption::DB_GATE.read();
                    Store::open_default().and_then(|store| process_batch(&store))
                };
                match result {
                    Ok(0) => std::thread::sleep(IDLE),
                    Ok(n) => tracing::debug!("made thumbnails for {n} files"),
                    Err(e) => {
                        tracing::warn!("thumbnail batch failed: {e:#}");
                        std::thread::sleep(IDLE);
                    }
                }
            }
        })
        .expect("spawning the thumbnail thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_has_no_thumbnail() {
        assert_eq!(generate("/nonexistent/pasted-test.mp4").unwrap(), None);
    }

    #[test]
    fn image_file_gets_a_png_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pic.png");
        // A 16x16 red PNG.
        let png = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP4z8BAEiJN9aiGUQ1DSgMAkPn/Afnh+ngAAAAASUVORK5CYII=",
        )
        .unwrap();
        std::fs::write(&path, png).unwrap();
        let thumb = generate(path.to_str().unwrap()).unwrap().expect("a thumbnail");
        assert!(thumb.starts_with(b"\x89PNG"));
    }
}
