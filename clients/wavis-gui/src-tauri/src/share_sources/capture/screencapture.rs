//! ScreenCaptureKit share-source enumeration and thumbnail capture (macOS).
//!
//! This module reserves the platform boundary for a future ScreenCaptureKit
//! implementation. Until then, enumeration reports that native listing is not
//! available and thumbnail capture returns no preview.

use super::super::EnumerationResult;

pub(super) async fn list_sources() -> Result<EnumerationResult, String> {
    Err("source enumeration is not yet supported on macOS".to_string())
}

pub(super) async fn fetch_thumbnail(_source_id: &str) -> Result<Option<String>, String> {
    Ok(None)
}
