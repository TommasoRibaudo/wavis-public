//! Share-source facade for the Tauri GUI.
//!
//! This module owns the stable cross-platform DTOs, pure filtering helpers,
//! shared thumbnail encoding, and the Tauri commands used by the frontend.
//! Platform-specific enumeration and capture details live under
//! `share_sources/capture/`.

use serde::{Deserialize, Serialize};

mod capture;

/// Source type discriminant.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ShareSourceType {
    Screen,
    Window,
    SystemAudio,
}

/// A single shareable source returned by enumeration.
#[derive(Debug, Clone, Serialize)]
pub struct ShareSource {
    /// Opaque platform identifier (for example a PipeWire node ID or HWND/HMONITOR handle).
    pub id: String,
    /// Human-readable name shown in the picker.
    pub name: String,
    /// Source category.
    pub source_type: ShareSourceType,
    /// Base64-encoded JPEG thumbnail for screen or window sources.
    pub thumbnail: Option<String>,
    /// Application name for window-like sources when the platform can determine it.
    pub app_name: Option<String>,
}

/// Why enumeration returned zero usable sources and which fallback the frontend should use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    /// Linux: PipeWire direct access unavailable, use portal-based system picker.
    Portal,
    /// Windows/macOS: use browser-native getDisplayMedia() via LiveKit JS SDK.
    GetDisplayMedia,
}

/// Enumeration result with optional warnings and structured fallback routing.
#[derive(Debug, Clone, Serialize)]
pub struct EnumerationResult {
    pub sources: Vec<ShareSource>,
    pub warnings: Vec<String>,
    pub fallback_reason: Option<FallbackReason>,
}

/// Descriptor for a top-level window used by the shared picker filter.
#[derive(Debug)]
#[allow(dead_code)]
struct WindowDescriptor {
    is_visible: bool,
    is_minimized: bool,
    client_area: u32,
    process_id: u32,
}

/// Pure filter for whether a window should appear in the picker.
#[allow(dead_code)]
fn should_include_window(desc: &WindowDescriptor, self_pid: u32) -> bool {
    desc.is_visible && !desc.is_minimized && desc.client_area > 0 && desc.process_id != self_pid
}

/// Compute fallback routing from the presence or absence of video sources.
fn compute_fallback_reason(sources: &[ShareSource]) -> Option<FallbackReason> {
    let has_video = sources.iter().any(|source| {
        matches!(
            source.source_type,
            ShareSourceType::Screen | ShareSourceType::Window
        )
    });
    if has_video {
        None
    } else {
        #[cfg(target_os = "linux")]
        {
            Some(FallbackReason::Portal)
        }

        #[cfg(not(target_os = "linux"))]
        {
            Some(FallbackReason::GetDisplayMedia)
        }
    }
}

/// Timeout for thumbnail capture before the picker falls back to a placeholder preview.
const THUMBNAIL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(800);

/// Maximum thumbnail dimension. Larger frames are downscaled before encoding.
const THUMBNAIL_MAX_DIM: u32 = 320;

/// JPEG quality for picker thumbnails.
const THUMBNAIL_JPEG_QUALITY: u8 = 75;

/// Encode RGBA pixel data as a base64 JPEG thumbnail string.
fn encode_thumbnail_jpeg(rgba: &[u8], width: u32, height: u32) -> Result<Option<String>, String> {
    use base64::Engine;
    use image::{ImageBuffer, RgbaImage};

    let img: RgbaImage = ImageBuffer::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| "failed to create image from frame data".to_string())?;

    let img = if width > THUMBNAIL_MAX_DIM || height > THUMBNAIL_MAX_DIM {
        image::imageops::resize(
            &img,
            THUMBNAIL_MAX_DIM,
            THUMBNAIL_MAX_DIM,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };

    let mut jpeg_buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut jpeg_buf);

    let encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, THUMBNAIL_JPEG_QUALITY);
    img.write_with_encoder(encoder)
        .map_err(|e| format!("JPEG encoding failed: {e}"))?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg_buf);
    Ok(Some(b64))
}

/// Tauri command: fetch a thumbnail for a specific source.
#[tauri::command]
pub async fn fetch_source_thumbnail(source_id: String) -> Result<Option<String>, String> {
    capture::fetch_thumbnail(&source_id).await
}

/// Tauri command: enumerate all shareable sources.
#[tauri::command]
pub async fn list_share_sources() -> Result<EnumerationResult, String> {
    crate::debug_eprintln!("wavis: share_sources: list_share_sources entered");
    capture::list_sources().await
}

/// Payload from the share picker when the user confirms a selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareSelection {
    pub mode: String,
    pub source_id: String,
    pub source_name: String,
    pub with_audio: bool,
}

/// Relay the share picker selection to the main window.
#[tauri::command]
pub fn share_picker_select(app: tauri::AppHandle, selection: ShareSelection) -> Result<(), String> {
    use tauri::Emitter;

    app.emit_to("main", "share-picker:selected", &selection)
        .map_err(|e| format!("failed to emit share-picker:selected: {e}"))
}

/// Relay the share picker cancellation to the main window.
#[tauri::command]
pub fn share_picker_cancel(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Emitter;

    app.emit_to("main", "share-picker:cancelled", ())
        .map_err(|e| format!("failed to emit share-picker:cancelled: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Strategy to generate a random `ShareSourceType`.
    fn arb_source_type() -> impl Strategy<Value = ShareSourceType> {
        prop::sample::select(vec![
            ShareSourceType::Screen,
            ShareSourceType::Window,
            ShareSourceType::SystemAudio,
        ])
    }

    /// Strategy to generate a non-empty string (1..=100 chars).
    fn non_empty_string() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9_ ]{1,100}".prop_map(|s| s.to_string())
    }

    /// Strategy to generate a random `ShareSource` with non-empty id and name.
    fn arb_share_source() -> impl Strategy<Value = ShareSource> {
        (
            non_empty_string(),
            non_empty_string(),
            arb_source_type(),
            proptest::option::of("[a-zA-Z0-9+/]{10,200}"),
            proptest::option::of("[a-zA-Z0-9_ ]{1,50}"),
        )
            .prop_map(|(id, name, source_type, thumbnail, app_name)| ShareSource {
                id,
                name,
                source_type,
                thumbnail,
                app_name,
            })
    }

    /// Strategy to generate a well-formed `ShareSource` that conforms to conditional field rules:
    /// - SystemAudio: thumbnail=None, app_name=None
    /// - Window: thumbnail=Some|None, app_name=Some (non-empty)
    /// - Screen: thumbnail=Some|None, app_name=None
    fn arb_well_formed_share_source() -> impl Strategy<Value = ShareSource> {
        arb_source_type().prop_flat_map(|st| {
            let thumbnail_strategy = match st {
                ShareSourceType::SystemAudio => Just(None).boxed(),
                _ => proptest::option::of("[a-zA-Z0-9+/]{10,200}").boxed(),
            };
            let app_name_strategy = match st {
                ShareSourceType::Window => non_empty_string().prop_map(Some).boxed(),
                _ => Just(None).boxed(),
            };
            (
                non_empty_string(),
                non_empty_string(),
                Just(st),
                thumbnail_strategy,
                app_name_strategy,
            )
                .prop_map(|(id, name, source_type, thumbnail, app_name)| ShareSource {
                    id,
                    name,
                    source_type,
                    thumbnail,
                    app_name,
                })
        })
    }

    /// Strategy to generate a random `FallbackReason`.
    fn arb_fallback_reason() -> impl Strategy<Value = FallbackReason> {
        prop_oneof![
            Just(FallbackReason::Portal),
            Just(FallbackReason::GetDisplayMedia),
        ]
    }

    /// Strategy to generate a random `EnumerationResult` with random sources, warnings, and fallback_reason.
    fn arb_enumeration_result() -> impl Strategy<Value = EnumerationResult> {
        (
            proptest::collection::vec(arb_share_source(), 0..=5),
            proptest::collection::vec("[a-zA-Z0-9 ]{1,50}", 0..=3),
            proptest::option::of(arb_fallback_reason()),
        )
            .prop_map(|(sources, warnings, fallback_reason)| EnumerationResult {
                sources,
                warnings,
                fallback_reason,
            })
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_screen_drm() {
        assert_eq!(
            capture::pipewire::classify_video_node("Video/Source/DRM"),
            Some(ShareSourceType::Screen)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_screen_kms() {
        assert_eq!(
            capture::pipewire::classify_video_node("Video/Source/KMS"),
            Some(ShareSourceType::Screen)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_screen_generic() {
        assert_eq!(
            capture::pipewire::classify_video_node("Video/Source"),
            Some(ShareSourceType::Screen)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_window_explicit() {
        assert_eq!(
            capture::pipewire::classify_video_node("Video/Source/Window"),
            Some(ShareSourceType::Window)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_window_stream_output() {
        assert_eq!(
            capture::pipewire::classify_video_node("Stream/Output/Video"),
            Some(ShareSourceType::Window)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_window_toplevel_heuristic() {
        assert_eq!(
            capture::pipewire::classify_video_node("Video/Source/xdg_toplevel"),
            Some(ShareSourceType::Window)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_none_for_audio() {
        assert_eq!(capture::pipewire::classify_video_node("Audio/Source"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_none_for_sink() {
        assert_eq!(capture::pipewire::classify_video_node("Audio/Sink"), None);
    }

    /// Strategy to generate a random `WindowDescriptor` with varying fields.
    fn arb_window_descriptor() -> impl Strategy<Value = WindowDescriptor> {
        (any::<bool>(), any::<bool>(), 0..10000u32, 0..10000u32).prop_map(
            |(is_visible, is_minimized, client_area, process_id)| WindowDescriptor {
                is_visible,
                is_minimized,
                client_area,
                process_id,
            },
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn share_source_structural_invariants(source in arb_share_source()) {
            prop_assert!(!source.id.is_empty(), "id must be non-empty");
            prop_assert!(!source.name.is_empty(), "name must be non-empty");
            prop_assert!(
                matches!(
                    source.source_type,
                    ShareSourceType::Screen | ShareSourceType::Window | ShareSourceType::SystemAudio
                ),
                "source_type must be Screen, Window, or SystemAudio"
            );
        }

        #[test]
        fn share_source_conditional_field_presence(source in arb_well_formed_share_source()) {
            if source.source_type == ShareSourceType::SystemAudio {
                prop_assert!(
                    source.thumbnail.is_none(),
                    "SystemAudio source must not have a thumbnail"
                );
            }

            if source.source_type == ShareSourceType::Window {
                prop_assert!(
                    source.app_name.is_some(),
                    "Window source must have app_name"
                );
                prop_assert!(
                    !source.app_name.as_ref().unwrap().is_empty(),
                    "Window source app_name must be non-empty"
                );
            }

            if source.source_type != ShareSourceType::Window {
                prop_assert!(
                    source.app_name.is_none(),
                    "Non-Window source must not have app_name"
                );
            }
        }

        #[test]
        fn fallback_reason_serialization_round_trip(reason in arb_fallback_reason()) {
            let json = serde_json::to_string(&reason).unwrap();
            let deserialized: FallbackReason = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(&deserialized, &reason, "FallbackReason round trip must preserve value");
        }

        #[test]
        fn enumeration_result_fallback_reason_round_trip(result in arb_enumeration_result()) {
            let json = serde_json::to_string(&result).unwrap();
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();

            let fr_value = &value["fallback_reason"];
            match &result.fallback_reason {
                None => prop_assert!(fr_value.is_null(), "fallback_reason None must serialize to null"),
                Some(reason) => {
                    let expected_str = serde_json::to_value(reason).unwrap();
                    prop_assert_eq!(fr_value, &expected_str, "fallback_reason must round-trip through JSON Value");
                }
            }
        }

        #[test]
        fn fallback_reason_reflects_video_source_availability(
            sources in proptest::collection::vec(arb_share_source(), 0..=10)
        ) {
            let has_video = sources.iter().any(|s| {
                matches!(s.source_type, ShareSourceType::Screen | ShareSourceType::Window)
            });

            let expected = if has_video {
                None
            } else {
                #[cfg(target_os = "linux")]
                { Some(FallbackReason::Portal) }

                #[cfg(target_os = "windows")]
                { Some(FallbackReason::GetDisplayMedia) }

                #[cfg(not(any(target_os = "linux", target_os = "windows")))]
                { Some(FallbackReason::GetDisplayMedia) }
            };

            let actual = compute_fallback_reason(&sources);
            prop_assert_eq!(
                actual, expected,
                "fallback_reason must be None when video sources present, \
                 platform-appropriate fallback when absent (has_video={})",
                has_video
            );
        }

        #[test]
        fn windows_window_filter_correctness(
            desc in arb_window_descriptor(),
            self_pid in 0..10000u32,
        ) {
            let result = should_include_window(&desc, self_pid);

            if result {
                prop_assert!(desc.is_visible, "included window must be visible");
                prop_assert!(!desc.is_minimized, "included window must not be minimized");
                prop_assert!(desc.client_area > 0, "included window must have non-zero client area");
                prop_assert!(desc.process_id != self_pid, "included window must not belong to self process");
            } else {
                let not_visible = !desc.is_visible;
                let minimized = desc.is_minimized;
                let zero_area = desc.client_area == 0;
                let self_process = desc.process_id == self_pid;
                prop_assert!(
                    not_visible || minimized || zero_area || self_process,
                    "excluded window must violate at least one filter criterion: \
                     is_visible={}, is_minimized={}, client_area={}, process_id={}, self_pid={}",
                    desc.is_visible, desc.is_minimized, desc.client_area, desc.process_id, self_pid
                );
            }
        }
    }
}
