//! On-device text recognition for image clips (Apple Vision), so text inside
//! screenshots is searchable. Runs on a background thread with its own
//! database connection.

use std::time::Duration;

use anyhow::{Result, anyhow};
use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2_foundation::{NSArray, NSData, NSDictionary};
use objc2_vision::{VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel};

use crate::model::IMAGE_UTIS;
use crate::store::Store;

const BATCH: usize = 8;
const IDLE: Duration = Duration::from_secs(15);
const BETWEEN_IMAGES: Duration = Duration::from_millis(150);

/// Recognized lines joined by newlines (empty when the image has no text).
pub fn recognize(image: &[u8]) -> Result<String> {
    autoreleasepool(|_| {
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &NSData::with_bytes(image),
            &NSDictionary::new(),
        );
        let as_request: Retained<VNRequest> = Retained::into_super(Retained::into_super(request.clone()));
        handler
            .performRequests_error(&NSArray::from_retained_slice(&[as_request]))
            .map_err(|e| anyhow!("Vision: {}", e.localizedDescription()))?;
        let lines: Vec<String> = request
            .results()
            .map(|obs| {
                obs.iter().filter_map(|o| o.topCandidates(1).firstObject().map(|t| t.string().to_string())).collect()
            })
            .unwrap_or_default();
        Ok(lines.join("\n"))
    })
}

fn process_batch(store: &Store) -> Result<usize> {
    let ids = store.items_needing_ocr(BATCH)?;
    for &id in &ids {
        let data = IMAGE_UTIS.iter().find_map(|uti| store.representation(id, uti).ok().flatten());
        // An empty result still marks the item as done so it is not retried forever.
        let text = match data.map(|d| recognize(&d)) {
            Some(Ok(text)) => text,
            Some(Err(e)) => {
                tracing::debug!("OCR failed for item {id}: {e:#}");
                String::new()
            }
            None => String::new(),
        };
        store.set_ocr(id, &text)?;
        std::thread::sleep(BETWEEN_IMAGES);
    }
    Ok(ids.len())
}

pub fn spawn_worker() {
    std::thread::Builder::new()
        .name("pasted-ocr".into())
        .spawn(|| {
            let store = match Store::open_default() {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("OCR worker could not open the database: {e:#}");
                    return;
                }
            };
            loop {
                match process_batch(&store) {
                    Ok(0) => std::thread::sleep(IDLE),
                    Ok(n) => tracing::debug!("OCR processed {n} images"),
                    Err(e) => {
                        tracing::warn!("OCR batch failed: {e:#}");
                        std::thread::sleep(IDLE);
                    }
                }
            }
        })
        .expect("spawning the OCR thread");
}
