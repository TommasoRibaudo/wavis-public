/// Regression test: no sensitive data in log macro arguments.
///
/// Scans all `.rs` files in `wavis-backend/src/` for `tracing::*!` / `info!` / `warn!` /
/// `debug!` / `error!` / `trace!` macro invocations whose arguments contain field names
/// that could leak sensitive data at runtime.
///
/// Sensitive patterns (disallowed):
///   - `token =`        — raw JWT / MediaToken value
///   - `jwt =`          — raw JWT string
///   - `media_token =`  — raw MediaToken value
///   - `invite_code =`  — raw invite code value
///   - `code =`         — raw invite code value
///   - `sdp =`          — raw SDP content
///   - `session_description =` — raw SDP content
///   - `candidate =`    — raw ICE candidate string
///
/// Allowlisted safe variants (explicitly permitted):
///   - `token_issued_at`, `token_ttl`   — metadata, not the token itself
///   - `invite_code_count`, `code_length` — counts/lengths, not values
///   - `sdp_length`, `sdp_type`         — metadata, not SDP content
///   - `candidate_count`, `candidate_type` — metadata, not candidate strings
///
/// Feature: token-and-signaling-auth
/// Validates: Requirements 5.1, 5.2, 5.3, 5.4, 6.2
use std::fs;
use std::path::Path;

/// Returns true if the line contains a log macro invocation.
fn is_log_line(line: &str) -> bool {
    let trimmed = line.trim();
    // Match tracing:: qualified macros and bare imported macros
    trimmed.contains("info!(")
        || trimmed.contains("warn!(")
        || trimmed.contains("debug!(")
        || trimmed.contains("error!(")
        || trimmed.contains("trace!(")
        || trimmed.contains("tracing::info!(")
        || trimmed.contains("tracing::warn!(")
        || trimmed.contains("tracing::debug!(")
        || trimmed.contains("tracing::error!(")
        || trimmed.contains("tracing::trace!(")
}

/// Sensitive field patterns that must NOT appear in log macro arguments.
/// Each entry is (disallowed_pattern, &[safe_prefixes_that_override_it]).
const SENSITIVE_PATTERNS: &[(&str, &[&str])] = &[
    // Token fields
    ("token =", &["token_issued_at =", "token_ttl ="]),
    ("jwt =", &[]),
    ("media_token =", &[]),
    // Invite code fields
    ("invite_code =", &["invite_code_count ="]),
    ("code =", &["code_length ="]),
    // SDP content fields
    ("sdp =", &["sdp_length =", "sdp_type ="]),
    ("session_description =", &[]),
    // ICE candidate fields
    ("candidate =", &["candidate_count =", "candidate_type ="]),
];

/// Check a single line for disallowed sensitive patterns.
/// Returns a list of (pattern, line_content) violations found.
fn check_line(line: &str) -> Vec<String> {
    if !is_log_line(line) {
        return vec![];
    }

    let mut violations = Vec::new();

    for (pattern, safe_overrides) in SENSITIVE_PATTERNS {
        if line.contains(pattern) {
            // Check if any safe override prefix is present — if so, it's allowed
            let is_safe = safe_overrides.iter().any(|safe| line.contains(safe));
            if !is_safe {
                violations.push(format!(
                    "  disallowed pattern {:?} found in log macro: {}",
                    pattern,
                    line.trim()
                ));
            }
        }
    }

    violations
}

/// Recursively collect all `.rs` files under `dir`.
fn collect_rs_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_rs_files(&path));
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files
}

#[test]
fn no_sensitive_fields_in_log_macros() {
    // Resolve path relative to this test file's crate root (wavis-backend/)
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    assert!(
        src_dir.exists(),
        "src dir not found at {}: check CARGO_MANIFEST_DIR",
        src_dir.display()
    );

    let rs_files = collect_rs_files(&src_dir);
    assert!(
        !rs_files.is_empty(),
        "no .rs files found under {}",
        src_dir.display()
    );

    let mut all_violations: Vec<String> = Vec::new();

    for file_path in &rs_files {
        let content = fs::read_to_string(file_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", file_path.display()));

        for (line_no, line) in content.lines().enumerate() {
            let violations = check_line(line);
            for v in violations {
                all_violations.push(format!(
                    "{}:{}: {}",
                    file_path
                        .strip_prefix(&src_dir)
                        .unwrap_or(file_path)
                        .display(),
                    line_no + 1,
                    v
                ));
            }
        }
    }

    if !all_violations.is_empty() {
        panic!(
            "Sensitive data found in log macro arguments ({} violation(s)):\n{}\n\n\
             Fix: replace raw sensitive values with safe metadata \
             (lengths, hashes, counts, types).\n\
             Allowlisted safe variants: token_issued_at, token_ttl, \
             invite_code_count, code_length, sdp_length, sdp_type, \
             candidate_count, candidate_type.",
            all_violations.len(),
            all_violations.join("\n")
        );
    }
}
