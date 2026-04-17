use async_trait::async_trait;
use std::fmt;

use crate::redaction::Sensitive;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum LlmError {
    /// API returned a non-2xx status.
    ApiError(String),
    /// Response could not be parsed as expected JSON.
    ParseError,
    /// Network or timeout error.
    NetworkError,
    /// No API key configured (server-side).
    NotConfigured,
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmError::ApiError(msg) => write!(f, "LLM API error: {}", msg),
            LlmError::ParseError => write!(f, "LLM response parse error"),
            LlmError::NetworkError => write!(f, "LLM network error"),
            LlmError::NotConfigured => write!(f, "LLM not configured"),
        }
    }
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Captured diagnostic context sent by the client (redacted).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct BugReportContext {
    pub js_console_logs: Vec<String>,
    pub rust_logs: Vec<String>,
    pub ws_messages: Vec<String>,
    pub app_state: AppStateSnapshot,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AppStateSnapshot {
    pub route: String,
    pub ws_status: String,
    pub voice_room_state: Option<String>,
    pub platform: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct QaPair {
    pub question: String,
    pub answer: String,
}

/// A follow-up question, optionally with a predefined option list.
/// When `options` is present the client renders a combobox; otherwise a text input.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LlmQuestion {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

/// Result of LLM analysis of a bug report.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LlmAnalysis {
    pub category: String,
    pub questions: Vec<LlmQuestion>,
    pub needs_follow_up: bool,
}

/// Result of LLM issue body generation.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct GeneratedIssueBody {
    pub title: String,
    pub body: String,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstraction over LLM API operations for testability.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Analyze a bug report description + context. Returns category, follow-up questions.
    async fn analyze_bug_report(
        &self,
        description: &str,
        context: &BugReportContext,
        previous_answers: Option<&[QaPair]>,
    ) -> Result<LlmAnalysis, LlmError>;

    /// Generate a structured GitHub issue body from collected data.
    async fn generate_issue_body(
        &self,
        description: &str,
        context: &BugReportContext,
        qa_rounds: &[Vec<QaPair>],
        category: &str,
    ) -> Result<GeneratedIssueBody, LlmError>;
}

// ---------------------------------------------------------------------------
// Screen-specific steering for the LLM
// ---------------------------------------------------------------------------

/// Returns a short description of the screen the user is on and what kinds of
/// bugs are common there, so the LLM can ask targeted follow-up questions.
fn screen_context_for_route(route: &str) -> &'static str {
    // Strip hash prefix if present (e.g. "#/room" → "/room")
    let path = route.strip_prefix('#').unwrap_or(route);

    // Match exact routes first, then prefix patterns
    match path {
        "/" => {
            "\
Screen: Channel List (home). Shows the user's channels with voice status indicators.\n\
Common issues: channels not loading, voice status badges stuck/wrong, invite codes not working, \
channel creation failing, UI not refreshing after actions.\n\
Ask about: which channel, what action they tried, whether the list loaded at all, \
whether they see stale data or an error message."
        }

        "/setup" => {
            "\
Screen: Device Setup (first launch). User enters server URL and registers a new device.\n\
Common issues: server URL rejected, registration failing, TLS errors, connection timeouts.\n\
Ask about: the server URL they entered, whether they see a specific error, \
whether this is a fresh install or reinstall, their network environment (VPN, proxy)."
        }

        "/login" => {
            "\
Screen: Login (existing device). User logs in with existing credentials.\n\
Common issues: login rejected, token errors, server unreachable.\n\
Ask about: whether they previously registered, the error message shown, \
whether they changed servers or reinstalled."
        }

        "/recover" => {
            "\
Screen: Account Recovery. User recovers access via their recovery phrase.\n\
Common issues: phrase not accepted, recovery ID forgotten, rate limiting.\n\
Ask about: whether they're sure of the phrase, how many attempts they've made, \
whether they see a rate limit or generic error."
        }

        "/pair" => {
            "\
Screen: Device Pairing. User pairs a new device via QR code or pairing code.\n\
Common issues: code expired, approval not received, pairing timeout.\n\
Ask about: which step failed (code entry, waiting for approval, finishing), \
whether the approving device is online, how long they waited."
        }

        "/settings" => {
            "\
Screen: Settings. Audio device selection, TLS toggle, denoise toggle, hotkeys.\n\
Common issues: audio device not listed, settings not persisting, denoise toggle not working, \
hotkey conflicts.\n\
Ask about: which setting they changed, whether it reverts on restart, \
their OS and audio setup (external mic, headset, etc.)."
        }

        "/devices" => {
            "\
Screen: Device Management. Lists registered devices, allows revoking.\n\
Common issues: device list not loading, revoke not working, stale device entries.\n\
Ask about: which device they tried to revoke, whether the list loaded, \
whether they see an error or nothing happens."
        }

        "/phrase" => {
            "\
Screen: Change Recovery Phrase. User updates their recovery phrase.\n\
Common issues: old phrase not accepted, new phrase rejected, save failing.\n\
Ask about: whether the old phrase verification passed, the error shown, \
whether they're on the correct account."
        }

        "/room" => {
            "\
Screen: Active Voice Room (SFU). Live voice session with participants, chat, screen share.\n\
Common issues: no audio (can't hear or be heard), echo/feedback, high latency, \
participants not appearing, chat messages not sending, screen share not starting, \
getting kicked unexpectedly, reconnection loops, mute/deafen not working.\n\
Ask about: whether they can hear others or only can't be heard (one-way audio), \
how many participants are in the room, whether the issue started suddenly or from join, \
their audio device (headset vs speakers), whether denoise is on, \
whether they see any status indicator (LIVE, RECONNECTING, FAILED)."
        }

        "/screen-share" | "/watch-all" | "/share-indicator" => {
            "\
Screen: Screen Share viewer/overlay. Displays shared screens from other participants.\n\
Common issues: black screen, stream not appearing, lag/stuttering, window not closing, \
resolution too low, audio from share missing.\n\
Ask about: whether they see a black rectangle or nothing at all, \
whether the sharer's stream was working before, their platform (Linux has different capture)."
        }

        "/share-picker" => {
            "\
Screen: Share Picker. User selects which screen/window to share.\n\
Common issues: no sources listed, wrong preview, picker not closing after selection, \
permission denied.\n\
Ask about: their OS, whether they see any sources at all, \
whether they granted screen capture permission."
        }

        _ if path.starts_with("/channel/") => {
            "\
Screen: Channel Detail. Shows a single channel's members, roles, invites, and voice status.\n\
Common issues: member list not loading, role changes not applying, invite generation failing, \
ban not working, voice join button not responding, stale participant count.\n\
Ask about: which action failed (invite, ban, role change, voice join), \
their role in the channel (owner, admin, member), the error message shown."
        }

        _ => {
            "\
Screen: Unknown or navigation in progress.\n\
Ask about: what they were trying to do, which screen they expected to be on, \
whether they navigated from another screen."
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt builders (ported from llm-client.ts)
// ---------------------------------------------------------------------------

fn build_analysis_prompt(
    description: &str,
    context: &BugReportContext,
    previous_answers: Option<&[QaPair]>,
) -> String {
    let context_summary = format!(
        "## Console Logs (last {} lines)\n{}\n\n\
         ## Rust Logs (last {} lines)\n{}\n\n\
         ## WebSocket Messages (last {})\n{}\n\n\
         ## App State\n\
         Route: {}\n\
         WS Status: {}\n\
         Voice Room: {}\n\
         Platform: {}",
        context.js_console_logs.len(),
        context
            .js_console_logs
            .iter()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        context.rust_logs.len(),
        context
            .rust_logs
            .iter()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        context.ws_messages.len(),
        context
            .ws_messages
            .iter()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        context.app_state.route,
        context.app_state.ws_status,
        context
            .app_state
            .voice_room_state
            .as_deref()
            .unwrap_or("none"),
        context.app_state.platform,
    );

    let screen_steering = screen_context_for_route(&context.app_state.route);

    let mut prompt = format!(
        "You are a bug report assistant for Wavis, a desktop voice chat application.\n\n\
         The user encountered a bug and provided this description:\n\
         \"{description}\"\n\n\
         Here is the captured diagnostic context:\n\
         {context_summary}\n\n\
         ## Screen Context\n\
         {screen_steering}\n\n\
         Use the screen context above to guide your follow-up questions. \
         Prioritize questions relevant to the specific screen and its common failure modes. \
         Don't ask about features unrelated to this screen.\n\n\
         ## Log Source Guide\n\
         - Console Logs: JS-side events — API errors, React warnings, navigation, auth token refresh failures.\n\
         - Rust Logs: native layer — Tauri IPC calls, audio device errors, LiveKit SDK events, screen capture.\n\
         - WS Messages: signaling traffic — join/leave, offers/answers, ICE candidates, kicks, mutes, chat, errors.\n\
         Look for error/warn lines, repeated reconnect attempts, or timeout patterns. \
         If logs suggest a specific subsystem failure, tailor your questions to that subsystem.\n\n\
         Please respond with valid JSON only (no markdown fences). The JSON must have this shape:\n\
         {{\n  \
           \"category\": \"<one of: audio, ui, connectivity, crash, performance, other>\",\n  \
           \"questions\": [\n    \
             {{ \"text\": \"<question>\", \"options\": [\"<opt1>\", \"<opt2>\", \"<opt3>\"] }},\n    \
             {{ \"text\": \"<open-ended question when options are not practical>\" }}\n  \
           ],\n  \
           \"needsFollowUp\": <true if a second round of questions would help, false otherwise>\n\
         }}\n\n\
         Rules for questions:\n\
         - Generate 3 to 5 follow-up questions to help diagnose the issue.\n\
         - Most questions (at least 3) MUST include an \\\"options\\\" array with 2–4 short choices.\n\
         - Only omit \\\"options\\\" for questions where free text is essential (e.g. copying an error message).\n\
         - Keep option labels short (under 8 words each)."
    );

    if let Some(answers) = previous_answers
        && !answers.is_empty()
    {
        let qa_block: String = answers
            .iter()
            .map(|qa| format!("Q: {}\nA: {}", qa.question, qa.answer))
            .collect::<Vec<_>>()
            .join("\n\n");
        prompt.push_str(&format!(
            "\n\nThe user already answered these follow-up questions:\n{qa_block}\n\n\
             Based on these answers, provide additional follow-up questions if needed. \
             Set needsFollowUp to false (this is the final round)."
        ));
    }

    prompt
}

fn build_issue_body_prompt(
    description: &str,
    context: &BugReportContext,
    qa_rounds: &[Vec<QaPair>],
    category: &str,
) -> String {
    let qa_section: String = qa_rounds
        .iter()
        .enumerate()
        .map(|(i, round)| {
            round
                .iter()
                .map(|qa| {
                    format!(
                        "**Round {} — Q:** {}\n**A:** {}",
                        i + 1,
                        qa.question,
                        qa.answer
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let diagnostics = format!(
        "### Console Logs\n```\n{}\n```\n\n\
         ### Rust Logs\n```\n{}\n```\n\n\
         ### WebSocket Messages\n```\n{}\n```\n\n\
         ### App State\n\
         - Route: {}\n\
         - WS Status: {}\n\
         - Voice Room: {}\n\
         - Platform: {}",
        context
            .js_console_logs
            .iter()
            .rev()
            .take(30)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        context
            .rust_logs
            .iter()
            .rev()
            .take(30)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        context
            .ws_messages
            .iter()
            .rev()
            .take(15)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        context.app_state.route,
        context.app_state.ws_status,
        context
            .app_state
            .voice_room_state
            .as_deref()
            .unwrap_or("none"),
        context.app_state.platform,
    );

    format!(
        "You are a bug report formatter for Wavis, a desktop voice chat application.\n\n\
         Generate a structured GitHub issue body. Respond with valid JSON only (no markdown fences):\n\
         {{\n  \"title\": \"<concise bug title>\",\n  \"body\": \"<full markdown issue body>\"\n}}\n\n\
         The issue body must contain these sections:\n\
         1. ## Bug Report header\n\
         2. ### Category — \"{category}\"\n\
         3. ### Description — the user's description\n\
         4. ### Follow-Up Answers — the Q&A rounds (if any)\n\
         5. <details><summary>Diagnostics</summary> — collapsible diagnostics section\n\n\
         User description: \"{description}\"\n\
         Category: {category}\n\n\
         Follow-up Q&A:\n{qa}\n\n\
         Diagnostics:\n{diagnostics}",
        qa = if qa_section.is_empty() {
            "None (offline mode)".to_string()
        } else {
            qa_section
        },
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Strip markdown code fences from LLM output.
/// Models sometimes wrap JSON in ```json ... ``` despite being told not to.
fn strip_markdown_fences(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip optional language tag on the first line (e.g. "```json\n")
        let after_tag = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        // Strip trailing fence
        after_tag.strip_suffix("```").unwrap_or(after_tag).trim()
    } else {
        trimmed
    }
}

// ---------------------------------------------------------------------------
// RealLlmClient — production implementation (Anthropic Messages API)
// ---------------------------------------------------------------------------

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 1024;
const TIMEOUT_SECS: u64 = 30;

pub struct RealLlmClient {
    client: reqwest::Client,
    api_key: Sensitive<String>,
    model: String,
}

impl RealLlmClient {
    pub fn new(api_key: Sensitive<String>, model: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self {
            client,
            api_key,
            model,
        }
    }

    async fn call_api(&self, prompt: &str) -> Result<String, LlmError> {
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "messages": [{ "role": "user", "content": prompt }],
        });

        let resp = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("Content-Type", "application/json")
            .header("x-api-key", self.api_key.inner())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|_| LlmError::NetworkError)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(LlmError::ApiError(format!(
                "{}: {}",
                status,
                &body_text[..body_text.len().min(200)]
            )));
        }

        let json: serde_json::Value = resp.json().await.map_err(|_| LlmError::ParseError)?;

        json["content"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|item| item["text"].as_str())
            .map(|s| s.to_string())
            .ok_or(LlmError::ParseError)
    }
}

#[async_trait]
impl LlmClient for RealLlmClient {
    async fn analyze_bug_report(
        &self,
        description: &str,
        context: &BugReportContext,
        previous_answers: Option<&[QaPair]>,
    ) -> Result<LlmAnalysis, LlmError> {
        let prompt = build_analysis_prompt(description, context, previous_answers);
        let text = self.call_api(&prompt).await?;

        let cleaned = strip_markdown_fences(&text);
        let parsed: serde_json::Value =
            serde_json::from_str(cleaned).map_err(|_| LlmError::ParseError)?;

        Ok(LlmAnalysis {
            category: parsed["category"].as_str().unwrap_or("other").to_string(),
            questions: parsed["questions"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| {
                            let text = v["text"].as_str()?.to_string();
                            let options = v["options"].as_array().map(|opts| {
                                opts.iter()
                                    .filter_map(|o| o.as_str().map(|s| s.to_string()))
                                    .collect::<Vec<_>>()
                            });
                            Some(LlmQuestion { text, options })
                        })
                        .take(5)
                        .collect()
                })
                .unwrap_or_default(),
            needs_follow_up: parsed["needsFollowUp"].as_bool().unwrap_or(false),
        })
    }

    async fn generate_issue_body(
        &self,
        description: &str,
        context: &BugReportContext,
        qa_rounds: &[Vec<QaPair>],
        category: &str,
    ) -> Result<GeneratedIssueBody, LlmError> {
        let prompt = build_issue_body_prompt(description, context, qa_rounds, category);
        let text = self.call_api(&prompt).await?;

        let cleaned = strip_markdown_fences(&text);
        let parsed: serde_json::Value =
            serde_json::from_str(cleaned).map_err(|_| LlmError::ParseError)?;

        let title = parsed["title"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                format!("Bug Report: {}", &description[..description.len().min(80)])
            });

        let body = parsed["body"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("## Bug Report\n\n### Description\n{description}"));

        Ok(GeneratedIssueBody { title, body })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── screen_context_for_route ─────────────────────────────────────────────

    #[test]
    fn route_home_maps_to_channel_list() {
        let ctx = screen_context_for_route("/");
        assert!(
            ctx.contains("Channel List"),
            "expected Channel List for '/'"
        );
    }

    #[test]
    fn route_room_maps_to_voice_room() {
        let ctx = screen_context_for_route("/room");
        assert!(
            ctx.contains("Voice Room"),
            "expected Voice Room for '/room'"
        );
    }

    #[test]
    fn route_setup_maps_to_device_setup() {
        let ctx = screen_context_for_route("/setup");
        assert!(
            ctx.contains("Device Setup"),
            "expected Device Setup for '/setup'"
        );
    }

    #[test]
    fn route_screen_share_maps_to_screen_share_context() {
        for route in ["/screen-share", "/watch-all", "/share-indicator"] {
            let ctx = screen_context_for_route(route);
            assert!(
                ctx.contains("Screen Share"),
                "expected Screen Share context for '{route}'"
            );
        }
    }

    #[test]
    fn route_channel_prefix_matches_channel_detail() {
        for route in ["/channel/abc-123", "/channel/room-99", "/channel/"] {
            let ctx = screen_context_for_route(route);
            assert!(
                ctx.contains("Channel Detail"),
                "expected Channel Detail for '{route}'"
            );
        }
    }

    #[test]
    fn unknown_route_falls_back_to_unknown_context() {
        for route in ["/nonexistent", "/room/extra", "/settings/foo"] {
            let ctx = screen_context_for_route(route);
            assert!(
                ctx.contains("Unknown"),
                "expected Unknown fallback for '{route}'"
            );
        }
    }

    /// Hash-prefixed routes (e.g. from client-side routing) must match the
    /// same context as the bare path.
    #[test]
    fn hash_prefixed_routes_match_same_context_as_bare_path() {
        for path in ["/", "/room", "/setup", "/login", "/settings"] {
            let bare = screen_context_for_route(path);
            let hashed = screen_context_for_route(&format!("#{path}"));
            assert_eq!(
                bare, hashed,
                "hash-prefixed '#{path}' should match bare '{path}'"
            );
        }
    }

    // ── strip_markdown_fences ────────────────────────────────────────────────

    #[test]
    fn strip_plain_json_is_unchanged() {
        let raw = r#"{"category":"audio","questions":[],"needsFollowUp":false}"#;
        assert_eq!(strip_markdown_fences(raw), raw);
    }

    #[test]
    fn strip_json_fence_with_lowercase_language_tag() {
        // LLMs often emit ```json despite being told not to
        let raw = "```json\n{\"key\":\"value\"}\n```";
        assert_eq!(strip_markdown_fences(raw), r#"{"key":"value"}"#);
    }

    #[test]
    fn strip_json_fence_with_uppercase_language_tag() {
        // Some models emit ```JSON
        let raw = "```JSON\n{\"key\":\"value\"}\n```";
        assert_eq!(strip_markdown_fences(raw), r#"{"key":"value"}"#);
    }

    #[test]
    fn strip_fence_without_language_tag() {
        // Bare triple-backtick fence
        let raw = "```\n{\"key\":\"value\"}\n```";
        assert_eq!(strip_markdown_fences(raw), r#"{"key":"value"}"#);
    }

    #[test]
    fn strip_fence_with_surrounding_whitespace() {
        // Leading/trailing whitespace is trimmed before matching
        let raw = "  ```json\n{\"key\":\"value\"}\n```  ";
        assert_eq!(strip_markdown_fences(raw), r#"{"key":"value"}"#);
    }

    #[test]
    fn strip_fence_result_is_valid_json() {
        // Round-trip: fence-wrapped JSON can be parsed after stripping
        let raw = "```json\n{\"category\":\"audio\",\"needsFollowUp\":true}\n```";
        let cleaned = strip_markdown_fences(raw);
        let parsed: serde_json::Value =
            serde_json::from_str(cleaned).expect("stripped output must be valid JSON");
        assert_eq!(parsed["category"].as_str(), Some("audio"));
        assert_eq!(parsed["needsFollowUp"].as_bool(), Some(true));
    }

    #[test]
    fn strip_fence_without_trailing_fence_returns_inner_content() {
        // Malformed: opening fence present but no closing fence
        let raw = "```json\n{\"key\":\"value\"}";
        let result = strip_markdown_fences(raw);
        // unwrap_or in the implementation returns the content without the suffix
        assert!(result.contains(r#"{"key":"value"}"#));
    }

    #[test]
    fn strip_bare_backtick_inline_fence_is_handled() {
        // Edge case: no newline between tag and content
        let raw = "```{\"key\":\"value\"}```";
        let result = strip_markdown_fences(raw);
        // No newline → find('\n') returns None → after_tag == rest → strip suffix
        assert_eq!(result, r#"{"key":"value"}"#);
    }

    #[test]
    fn strip_plain_text_with_preamble_is_returned_as_is() {
        // When the LLM adds prose before the fence the function cannot strip it;
        // the whole string is returned (the caller will get a ParseError, which
        // is the documented failure mode for this case).
        let raw = "Here you go:\n```json\n{\"key\":\"value\"}\n```";
        let result = strip_markdown_fences(raw);
        // Starts with 'H', not '{' — fence is not at position 0
        assert!(result.starts_with("Here you go:"));
    }
}

// ---------------------------------------------------------------------------
// NoOpLlmClient — used when BUG_REPORT_LLM_API_KEY is not set
// ---------------------------------------------------------------------------

pub struct NoOpLlmClient;

#[async_trait]
impl LlmClient for NoOpLlmClient {
    async fn analyze_bug_report(
        &self,
        _description: &str,
        _context: &BugReportContext,
        _previous_answers: Option<&[QaPair]>,
    ) -> Result<LlmAnalysis, LlmError> {
        Err(LlmError::NotConfigured)
    }

    async fn generate_issue_body(
        &self,
        _description: &str,
        _context: &BugReportContext,
        _qa_rounds: &[Vec<QaPair>],
        _category: &str,
    ) -> Result<GeneratedIssueBody, LlmError> {
        Err(LlmError::NotConfigured)
    }
}

// ---------------------------------------------------------------------------
// MockLlmClient — test implementation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum MockLlmCall {
    Analyze {
        description: String,
    },
    GenerateBody {
        description: String,
        category: String,
    },
}

pub struct MockLlmClient {
    pub calls: std::sync::Arc<std::sync::Mutex<Vec<MockLlmCall>>>,
    pub analyze_result: std::sync::Arc<std::sync::Mutex<Result<LlmAnalysis, LlmError>>>,
    pub generate_result: std::sync::Arc<std::sync::Mutex<Result<GeneratedIssueBody, LlmError>>>,
}

#[allow(dead_code)]
impl MockLlmClient {
    pub fn new() -> Self {
        Self {
            calls: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            analyze_result: std::sync::Arc::new(std::sync::Mutex::new(Ok(LlmAnalysis {
                category: "other".to_string(),
                questions: vec![LlmQuestion {
                    text: "What happened?".to_string(),
                    options: Some(vec![
                        "App crashed".to_string(),
                        "UI froze".to_string(),
                        "Audio stopped".to_string(),
                    ]),
                }],
                needs_follow_up: false,
            }))),
            generate_result: std::sync::Arc::new(std::sync::Mutex::new(Ok(GeneratedIssueBody {
                title: "Bug Report: test".to_string(),
                body: "## Bug Report\n\nTest body".to_string(),
            }))),
        }
    }

    pub fn get_calls(&self) -> Vec<MockLlmCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Default for MockLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn analyze_bug_report(
        &self,
        description: &str,
        _context: &BugReportContext,
        _previous_answers: Option<&[QaPair]>,
    ) -> Result<LlmAnalysis, LlmError> {
        self.calls.lock().unwrap().push(MockLlmCall::Analyze {
            description: description.to_string(),
        });
        self.analyze_result.lock().unwrap().clone()
    }

    async fn generate_issue_body(
        &self,
        description: &str,
        _context: &BugReportContext,
        _qa_rounds: &[Vec<QaPair>],
        category: &str,
    ) -> Result<GeneratedIssueBody, LlmError> {
        self.calls.lock().unwrap().push(MockLlmCall::GenerateBody {
            description: description.to_string(),
            category: category.to_string(),
        });
        self.generate_result.lock().unwrap().clone()
    }
}
