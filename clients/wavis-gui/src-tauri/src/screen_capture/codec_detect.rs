//! VA-API hardware encoder detection for codec preference on Linux.
//!
//! Probes at runtime whether a VA-API H.264 hardware encoder is available.
//! Used by the video publishing layer (task 6.2) to set `TrackPublishOptions`:
//! - H.264 preferred when hardware encoder detected (lower CPU, better quality)
//! - VP8 fallback when no hardware H.264 (LiveKit SDK default, software encode)
//!
//! Detection is lightweight: checks for DRI render nodes, then runs `vainfo`
//! and looks for H.264 encode entrypoints. No `libva` crate dependency — just
//! filesystem probes and a shell command. Result is cached for the process
//! lifetime via `OnceLock`.

use std::sync::OnceLock;

/// Cached result of VA-API H.264 hardware encoder detection.
#[allow(dead_code)]
static HAS_VAAPI_H264: OnceLock<bool> = OnceLock::new();

/// Check if a VA-API H.264 hardware encoder is available.
///
/// Result is cached for the process lifetime — the first call does the actual
/// probe, subsequent calls return the cached value instantly.
///
/// Detection strategy:
/// 1. Check if any `/dev/dri/renderD*` render node exists (GPU with DRI support)
/// 2. Run `vainfo` and look for H.264/H264 encode entrypoints (`VAEntrypointEncSlice`)
///
/// Returns `false` if either check fails (no render node, `vainfo` not installed,
/// no H.264 encode profile, or any I/O error).
#[allow(dead_code)]
pub fn has_vaapi_h264() -> bool {
    *HAS_VAAPI_H264.get_or_init(detect_vaapi_h264)
}

/// Probe for VA-API H.264 hardware encoder availability.
#[allow(dead_code)]
fn detect_vaapi_h264() -> bool {
    // Step 1: Check for DRI render nodes — fast filesystem check.
    if !has_dri_render_node() {
        log::debug!("No DRI render node found, skipping VA-API probe");
        return false;
    }

    // Step 2: Run `vainfo` and check for H.264 encode entrypoint.
    match probe_vainfo_h264() {
        Ok(has_h264) => {
            if has_h264 {
                log::info!("VA-API H.264 hardware encoder detected — preferring H.264 codec");
            } else {
                log::info!("VA-API available but no H.264 encode entrypoint — falling back to VP8");
            }
            has_h264
        }
        Err(reason) => {
            log::debug!("VA-API probe failed ({reason}) — falling back to VP8");
            false
        }
    }
}

/// Check if any `/dev/dri/renderD*` render node exists.
///
/// Render nodes (renderD128, renderD129, ...) indicate a GPU with DRI support.
/// This is a prerequisite for VA-API — no render node means no hardware encoder.
#[allow(dead_code)]
fn has_dri_render_node() -> bool {
    let dri_path = std::path::Path::new("/dev/dri");
    let Ok(entries) = std::fs::read_dir(dri_path) else {
        return false;
    };
    entries.filter_map(|e| e.ok()).any(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("renderD"))
    })
}

/// Run `vainfo` and check its output for H.264 encode entrypoints.
///
/// Looks for lines containing both an H.264 profile name (`VAProfileH264`
/// or `H264`) and the encode slice entrypoint (`VAEntrypointEncSlice`).
/// This combination confirms the GPU can encode H.264 via VA-API.
#[allow(dead_code)]
fn probe_vainfo_h264() -> Result<bool, String> {
    let output = std::process::Command::new("vainfo")
        .arg("--display")
        .arg("drm")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run vainfo: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "vainfo exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(vainfo_has_h264_encode(&stdout))
}

/// Parse `vainfo` output and return `true` if any H.264 encode entrypoint is present.
///
/// Matches lines containing both an H.264 profile identifier (`h264` or `h.264`,
/// case-insensitive) and an encode entrypoint (`VAEntrypointEncSlice` or
/// `VAEntrypointEncSliceLP`). Both regular and low-power encode paths are accepted.
///
/// Example matching lines from `vainfo` output:
/// ```text
/// VAProfileH264Main               : VAEntrypointEncSlice
/// VAProfileH264High               : VAEntrypointEncSliceLP
/// VAProfileH264ConstrainedBaseline : VAEntrypointEncSlice
/// ```
#[allow(dead_code)]
fn vainfo_has_h264_encode(vainfo_output: &str) -> bool {
    vainfo_output.lines().any(|line| {
        let lower = line.to_lowercase();
        (lower.contains("h264") || lower.contains("h.264"))
            && lower.contains("vaentrypointencslice")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── vainfo output parsing tests (cross-platform) ──────────────

    #[test]
    fn detects_h264_main_encode() {
        let output = "\
VAProfileH264Main               : VAEntrypointVLD
VAProfileH264Main               : VAEntrypointEncSlice
VAProfileH264High               : VAEntrypointVLD";
        assert!(vainfo_has_h264_encode(output));
    }

    #[test]
    fn detects_h264_high_encode() {
        let output = "      VAProfileH264High               : VAEntrypointEncSlice";
        assert!(vainfo_has_h264_encode(output));
    }

    #[test]
    fn detects_h264_constrained_baseline_encode() {
        let output = "      VAProfileH264ConstrainedBaseline: VAEntrypointEncSlice";
        assert!(vainfo_has_h264_encode(output));
    }

    #[test]
    fn detects_h264_low_power_encode() {
        // Some Intel GPUs expose VAEntrypointEncSliceLP for H.264.
        let output = "      VAProfileH264High               : VAEntrypointEncSliceLP";
        assert!(vainfo_has_h264_encode(output));
    }

    #[test]
    fn rejects_h264_decode_only() {
        // VLD is the decode entrypoint — should not match.
        let output = "\
VAProfileH264Main               : VAEntrypointVLD
VAProfileH264High               : VAEntrypointVLD";
        assert!(!vainfo_has_h264_encode(output));
    }

    #[test]
    fn rejects_non_h264_encode() {
        // VP9 and HEVC encode should not trigger H.264 detection.
        let output = "\
VAProfileVP9Profile0            : VAEntrypointEncSlice
VAProfileHEVCMain               : VAEntrypointEncSlice";
        assert!(!vainfo_has_h264_encode(output));
    }

    #[test]
    fn rejects_empty_output() {
        assert!(!vainfo_has_h264_encode(""));
    }

    #[test]
    fn handles_mixed_profiles() {
        // Realistic vainfo output with many profiles — only H.264 encode matters.
        let output = "\
vainfo: VA-API version: 1.20 (libva 2.20.1)
vainfo: Driver version: Intel iHD driver for Intel(R) Gen Graphics
vainfo: Supported profile and target pairs:
      VAProfileMPEG2Simple            : VAEntrypointVLD
      VAProfileH264Main               : VAEntrypointVLD
      VAProfileH264Main               : VAEntrypointEncSlice
      VAProfileH264High               : VAEntrypointVLD
      VAProfileH264High               : VAEntrypointEncSlice
      VAProfileHEVCMain               : VAEntrypointVLD
      VAProfileHEVCMain               : VAEntrypointEncSlice
      VAProfileVP9Profile0            : VAEntrypointVLD";
        assert!(vainfo_has_h264_encode(output));
    }

    #[test]
    fn case_insensitive_matching() {
        // Unlikely but defensive — handle odd casing.
        let output = "      vaprofileh264main               : vaentrypointencslice";
        assert!(vainfo_has_h264_encode(output));
    }

    // ─── Cached result stability ───────────────────────────────────

    #[test]
    fn cached_result_is_stable() {
        let first = has_vaapi_h264();
        let second = has_vaapi_h264();
        assert_eq!(first, second, "cached result must be stable across calls");
    }
}
