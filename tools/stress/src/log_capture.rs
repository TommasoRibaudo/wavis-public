//! Log capture infrastructure for the stress harness.
//!
//! Provides two capture modes:
//!
//! - **In-process mode**: A custom [`tracing_subscriber::Layer`] (`BufferingLayer`) that
//!   intercepts every log event and appends a formatted string to a shared
//!   `Arc<Mutex<Vec<String>>>` buffer.  Install it once before starting the in-process
//!   backend via [`install_log_capture`].
//!
//! - **External mode**: The caller is responsible for redirecting the backend process's
//!   stdout/stderr to a temp file.  After the scenario, call [`LogCapture::from_file`] to
//!   read the file into the buffer, then use the normal [`LogCapture`] API to grep it.
//!
//! # Sensitive patterns
//!
//! [`SENSITIVE_PATTERNS`] lists `(pattern, allowlist)` pairs that mirror the patterns
//! checked by `wavis-backend/tests/no_sensitive_logs.rs`.  [`LogCapture::grep_sensitive`]
//! uses these to scan captured lines and return any [`SensitiveMatch`] hits.

use std::fmt::Write as FmtWrite;
use std::sync::{Arc, Mutex};

use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

// ---------------------------------------------------------------------------
// Sensitive pattern definitions
// ---------------------------------------------------------------------------

/// A `(pattern, allowlist)` pair.
///
/// A log line matches if it contains `pattern` AND does NOT contain any of the
/// allowlist substrings.  This mirrors the logic in `no_sensitive_logs.rs`.
pub struct SensitivePatternRule {
    /// The substring that must be present for a line to be considered a match.
    pub pattern: &'static str,
    /// Substrings that, if present, exempt the line from being flagged.
    pub allowlist: &'static [&'static str],
}

/// All sensitive patterns the log-grep scenario checks for.
///
/// Mirrors `wavis-backend/tests/no_sensitive_logs.rs`.
pub static SENSITIVE_PATTERNS: &[SensitivePatternRule] = &[
    SensitivePatternRule {
        pattern: "token =",
        allowlist: &["token_issued_at", "token_ttl"],
    },
    SensitivePatternRule {
        pattern: "jwt =",
        allowlist: &[],
    },
    SensitivePatternRule {
        pattern: "invite_code =",
        allowlist: &["invite_code_count"],
    },
    SensitivePatternRule {
        pattern: "code =",
        allowlist: &["code_length"],
    },
    SensitivePatternRule {
        pattern: "sdp =",
        allowlist: &["sdp_length", "sdp_type"],
    },
    SensitivePatternRule {
        pattern: "candidate =",
        allowlist: &["candidate_count", "candidate_type"],
    },
];

// ---------------------------------------------------------------------------
// SensitiveMatch
// ---------------------------------------------------------------------------

/// A sensitive pattern found in a captured log line.
#[derive(Debug, Clone)]
pub struct SensitiveMatch {
    /// The full log line that triggered the match.
    pub line: String,
    /// The pattern that was matched (e.g. `"token ="`).
    pub pattern: String,
}

// ---------------------------------------------------------------------------
// LogCapture
// ---------------------------------------------------------------------------

/// Handle to the shared log buffer.
///
/// Cheap to clone — all clones share the same underlying buffer.
#[derive(Clone, Default)]
pub struct LogCapture {
    buffer: Arc<Mutex<Vec<String>>>,
}

impl LogCapture {
    /// Create a new, empty log capture buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a snapshot of all captured lines (cloned).
    pub fn lines(&self) -> Vec<String> {
        self.buffer.lock().unwrap().clone()
    }

    /// Clear all captured lines.
    pub fn clear(&self) {
        self.buffer.lock().unwrap().clear();
    }

    /// Append a single line to the buffer (used by [`BufferingLayer`] and
    /// [`LogCapture::from_file`]).
    pub(crate) fn push(&self, line: String) {
        self.buffer.lock().unwrap().push(line);
    }

    /// Read lines from a file and append them to the buffer.
    ///
    /// Used in external-process mode where the backend's log output has been
    /// redirected to a temp file.
    pub fn load_from_file(&self, path: &std::path::Path) -> std::io::Result<usize> {
        let content = std::fs::read_to_string(path)?;
        let mut buf = self.buffer.lock().unwrap();
        let before = buf.len();
        for line in content.lines() {
            buf.push(line.to_owned());
        }
        Ok(buf.len() - before)
    }

    /// Scan all captured lines for sensitive patterns.
    ///
    /// Returns one [`SensitiveMatch`] per offending line (first matching pattern
    /// wins per line).
    pub fn grep_sensitive(&self) -> Vec<SensitiveMatch> {
        let lines = self.buffer.lock().unwrap();
        let mut matches = Vec::new();

        for line in lines.iter() {
            for rule in SENSITIVE_PATTERNS {
                if line.contains(rule.pattern) {
                    // Check if any allowlist entry exempts this line.
                    let exempted = rule.allowlist.iter().any(|allow| line.contains(allow));
                    if !exempted {
                        matches.push(SensitiveMatch {
                            line: line.clone(),
                            pattern: rule.pattern.to_owned(),
                        });
                        break; // one match per line is enough
                    }
                }
            }
        }

        matches
    }
}

// ---------------------------------------------------------------------------
// BufferingLayer — tracing subscriber layer
// ---------------------------------------------------------------------------

/// A [`tracing_subscriber::Layer`] that formats each log event as a string and
/// appends it to a shared [`LogCapture`] buffer.
///
/// Used in in-process mode to intercept backend log output before it reaches
/// the normal stdout formatter.
pub struct BufferingLayer {
    capture: LogCapture,
}

impl BufferingLayer {
    /// Create a new `BufferingLayer` that writes into `capture`.
    pub fn new(capture: LogCapture) -> Self {
        Self { capture }
    }
}

impl<S: Subscriber> Layer<S> for BufferingLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // Format the event into a single string.
        let mut line = String::new();

        // Level
        let level = *event.metadata().level();
        let _ = write!(line, "{level} ");

        // Target (module path)
        let target = event.metadata().target();
        if !target.is_empty() {
            let _ = write!(line, "{target}: ");
        }

        // Fields — visit all key=value pairs.
        let mut visitor = FieldVisitor::new(&mut line);
        event.record(&mut visitor);

        self.capture.push(line);
    }
}

// ---------------------------------------------------------------------------
// FieldVisitor — formats tracing fields into "key=value" pairs
// ---------------------------------------------------------------------------

struct FieldVisitor<'a> {
    buf: &'a mut String,
    first: bool,
}

impl<'a> FieldVisitor<'a> {
    fn new(buf: &'a mut String) -> Self {
        Self { buf, first: true }
    }

    fn write_sep(&mut self) {
        if !self.first {
            self.buf.push(' ');
        }
        self.first = false;
    }
}

impl tracing::field::Visit for FieldVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.write_sep();
        if field.name() == "message" {
            let _ = write!(self.buf, "{value}");
        } else {
            let _ = write!(self.buf, "{}={:?}", field.name(), value);
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.write_sep();
        if field.name() == "message" {
            let _ = write!(self.buf, "{value:?}");
        } else {
            let _ = write!(self.buf, "{}={:?}", field.name(), value);
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.write_sep();
        let _ = write!(self.buf, "{}={value}", field.name());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.write_sep();
        let _ = write!(self.buf, "{}={value}", field.name());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.write_sep();
        let _ = write!(self.buf, "{}={value}", field.name());
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.write_sep();
        let _ = write!(self.buf, "{}={value}", field.name());
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.write_sep();
        let _ = write!(self.buf, "{}={value}", field.name());
    }
}

// ---------------------------------------------------------------------------
// install_log_capture
// ---------------------------------------------------------------------------

/// Install the [`BufferingLayer`] as part of the global tracing subscriber.
///
/// This must be called **before** starting the in-process backend (which calls
/// `tracing_subscriber::fmt().init()` internally — or rather, the harness
/// controls subscriber initialisation and must NOT call `init()` elsewhere).
///
/// Returns a [`LogCapture`] handle that shares the buffer with the installed layer.
///
/// # Panics
///
/// Panics if a global subscriber has already been set (i.e. this is called more
/// than once, or after `tracing_subscriber::fmt().init()` has been called).
pub fn install_log_capture() -> LogCapture {
    let capture = LogCapture::new();
    let buffering = BufferingLayer::new(capture.clone());

    // Compose: buffering layer + fmt layer (for human-readable stdout output) +
    // env-filter (respects RUST_LOG).
    let env_filter = tracing_subscriber::EnvFilter::from_default_env();

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(buffering)
        .init();

    capture
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_capture_with_lines(lines: &[&str]) -> LogCapture {
        let cap = LogCapture::new();
        for line in lines {
            cap.push(line.to_string());
        }
        cap
    }

    #[test]
    fn new_capture_is_empty() {
        let cap = LogCapture::new();
        assert!(cap.lines().is_empty());
    }

    #[test]
    fn push_and_lines_roundtrip() {
        let cap = LogCapture::new();
        cap.push("hello".into());
        cap.push("world".into());
        assert_eq!(cap.lines(), vec!["hello", "world"]);
    }

    #[test]
    fn clear_empties_buffer() {
        let cap = LogCapture::new();
        cap.push("line".into());
        cap.clear();
        assert!(cap.lines().is_empty());
    }

    #[test]
    fn clone_shares_buffer() {
        let cap = LogCapture::new();
        let cap2 = cap.clone();
        cap.push("shared".into());
        assert_eq!(cap2.lines(), vec!["shared"]);
    }

    // --- grep_sensitive tests ---

    #[test]
    fn grep_sensitive_no_matches_on_clean_lines() {
        let cap = make_capture_with_lines(&[
            "INFO wavis_backend::domain::invite: invite created",
            "DEBUG wavis_backend: room joined peer_id=peer-1",
        ]);
        assert!(cap.grep_sensitive().is_empty());
    }

    #[test]
    fn grep_sensitive_detects_token_eq() {
        let cap = make_capture_with_lines(&["DEBUG handler: token = eyJhbGciOiJIUzI1NiJ9..."]);
        let matches = cap.grep_sensitive();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern, "token =");
    }

    #[test]
    fn grep_sensitive_allows_token_issued_at() {
        let cap = make_capture_with_lines(&["INFO: token_issued_at=1234567890"]);
        assert!(cap.grep_sensitive().is_empty());
    }

    #[test]
    fn grep_sensitive_allows_token_ttl() {
        let cap = make_capture_with_lines(&["INFO: token_ttl=600"]);
        assert!(cap.grep_sensitive().is_empty());
    }

    #[test]
    fn grep_sensitive_detects_jwt_eq() {
        let cap = make_capture_with_lines(&["WARN: jwt = some.jwt.value"]);
        let matches = cap.grep_sensitive();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern, "jwt =");
    }

    #[test]
    fn grep_sensitive_detects_invite_code_eq() {
        let cap = make_capture_with_lines(&["DEBUG: invite_code = ABCD1234"]);
        let matches = cap.grep_sensitive();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern, "invite_code =");
    }

    #[test]
    fn grep_sensitive_allows_invite_code_count() {
        let cap = make_capture_with_lines(&["DEBUG: invite_code_count=5"]);
        assert!(cap.grep_sensitive().is_empty());
    }

    #[test]
    fn grep_sensitive_detects_code_eq() {
        let cap = make_capture_with_lines(&["DEBUG: code = XYZW"]);
        let matches = cap.grep_sensitive();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern, "code =");
    }

    #[test]
    fn grep_sensitive_allows_code_length() {
        let cap = make_capture_with_lines(&["DEBUG: code_length=8"]);
        assert!(cap.grep_sensitive().is_empty());
    }

    #[test]
    fn grep_sensitive_detects_sdp_eq() {
        let cap = make_capture_with_lines(&["DEBUG: sdp = v=0\r\no=..."]);
        let matches = cap.grep_sensitive();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern, "sdp =");
    }

    #[test]
    fn grep_sensitive_allows_sdp_length_and_sdp_type() {
        let cap = make_capture_with_lines(&["DEBUG: sdp_length=1024", "DEBUG: sdp_type=offer"]);
        assert!(cap.grep_sensitive().is_empty());
    }

    #[test]
    fn grep_sensitive_detects_candidate_eq() {
        let cap = make_capture_with_lines(&["DEBUG: candidate = candidate:1 1 UDP ..."]);
        let matches = cap.grep_sensitive();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern, "candidate =");
    }

    #[test]
    fn grep_sensitive_allows_candidate_count_and_type() {
        let cap =
            make_capture_with_lines(&["DEBUG: candidate_count=3", "DEBUG: candidate_type=srflx"]);
        assert!(cap.grep_sensitive().is_empty());
    }

    #[test]
    fn grep_sensitive_returns_one_match_per_line() {
        // A line that matches multiple patterns should only produce one SensitiveMatch.
        let cap = make_capture_with_lines(&["DEBUG: token = abc jwt = xyz"]);
        let matches = cap.grep_sensitive();
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn load_from_file_reads_lines() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "line one").unwrap();
        writeln!(tmp, "line two").unwrap();

        let cap = LogCapture::new();
        let count = cap.load_from_file(tmp.path()).unwrap();
        assert_eq!(count, 2);
        assert_eq!(cap.lines(), vec!["line one", "line two"]);
    }
}
