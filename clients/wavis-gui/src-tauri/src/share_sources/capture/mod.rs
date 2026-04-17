//! Platform dispatch for share-source enumeration and thumbnail capture.
//!
//! This module owns compile-time routing only. Implementations live in
//! `pipewire` (Linux), `windows` (Windows), and `screencapture` (macOS).
//! The stable facade, shared types, and Tauri command wiring remain in
//! the parent `share_sources` module.

#[cfg(target_os = "linux")]
pub(super) mod pipewire;
#[cfg(target_os = "macos")]
pub(super) mod screencapture;
#[cfg(target_os = "windows")]
pub(super) mod windows;

use super::EnumerationResult;

#[cfg(target_os = "linux")]
pub(super) async fn list_sources() -> Result<EnumerationResult, String> {
    pipewire::list_sources().await
}

#[cfg(target_os = "windows")]
pub(super) async fn list_sources() -> Result<EnumerationResult, String> {
    tokio::task::spawn_blocking(windows::list_sources)
        .await
        .map_err(|e| format!("enumeration task panicked: {e}"))?
}

#[cfg(target_os = "macos")]
pub(super) async fn list_sources() -> Result<EnumerationResult, String> {
    screencapture::list_sources().await
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub(super) async fn list_sources() -> Result<EnumerationResult, String> {
    Err("source enumeration is not supported on this platform".to_string())
}

#[cfg(target_os = "linux")]
pub(super) async fn fetch_thumbnail(source_id: &str) -> Result<Option<String>, String> {
    pipewire::fetch_thumbnail(source_id).await
}

#[cfg(target_os = "windows")]
pub(super) async fn fetch_thumbnail(source_id: &str) -> Result<Option<String>, String> {
    let source_id = source_id.to_string();
    tokio::task::spawn_blocking(move || windows::fetch_thumbnail(&source_id))
        .await
        .map_err(|e| format!("thumbnail task panicked: {e}"))?
}

#[cfg(target_os = "macos")]
pub(super) async fn fetch_thumbnail(source_id: &str) -> Result<Option<String>, String> {
    screencapture::fetch_thumbnail(source_id).await
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub(super) async fn fetch_thumbnail(_source_id: &str) -> Result<Option<String>, String> {
    Ok(None)
}
