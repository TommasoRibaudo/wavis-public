use async_trait::async_trait;
use base64::Engine;
use std::fmt;
use uuid::Uuid;

use crate::redaction::Sensitive;

/// Errors from bug report domain operations.
#[derive(Debug, Clone)]
pub enum BugReportError {
    /// Not valid PNG magic bytes.
    InvalidScreenshot,
    /// GitHub Contents API error.
    ScreenshotUploadFailed,
    /// GitHub Issues API error.
    IssueCreationFailed,
    /// Decoded size exceeds limit.
    PayloadTooLarge,
}

impl fmt::Display for BugReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BugReportError::InvalidScreenshot => write!(f, "invalid screenshot: not valid PNG"),
            BugReportError::ScreenshotUploadFailed => write!(f, "screenshot upload failed"),
            BugReportError::IssueCreationFailed => write!(f, "issue creation failed"),
            BugReportError::PayloadTooLarge => write!(f, "payload too large"),
        }
    }
}

/// Abstraction over GitHub API operations for testability.
#[async_trait]
pub trait GitHubClient: Send + Sync {
    /// Upload file contents via GitHub Contents API. Returns the raw content URL.
    async fn upload_contents(
        &self,
        repo: &str,
        path: &str,
        content_base64: &str,
        message: &str,
    ) -> Result<String, BugReportError>;

    /// Create a GitHub issue via Issues API. Returns the issue HTML URL.
    async fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: Vec<&str>,
    ) -> Result<String, BugReportError>;
}

// ---------------------------------------------------------------------------
// RealGitHubClient — production implementation
// ---------------------------------------------------------------------------

/// Production GitHub client using `reqwest::Client` with a `Sensitive<String>` token.
pub struct RealGitHubClient {
    client: reqwest::Client,
    token: Sensitive<String>,
}

impl RealGitHubClient {
    pub fn new(token: Sensitive<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            token,
        }
    }
}

fn github_blob_html_url_to_renderable_url(html_url: &str) -> String {
    if html_url.contains('?') {
        format!("{html_url}&raw=1")
    } else {
        format!("{html_url}?raw=1")
    }
}

#[async_trait]
impl GitHubClient for RealGitHubClient {
    async fn upload_contents(
        &self,
        repo: &str,
        path: &str,
        content_base64: &str,
        message: &str,
    ) -> Result<String, BugReportError> {
        let url = format!("https://api.github.com/repos/{repo}/contents/{path}");

        let body = serde_json::json!({
            "message": message,
            "content": content_base64,
        });

        let resp = self
            .client
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.token.inner()))
            .header("User-Agent", "wavis-backend")
            .header("Accept", "application/vnd.github+json")
            .json(&body)
            .send()
            .await
            .map_err(|_| BugReportError::ScreenshotUploadFailed)?;

        if !resp.status().is_success() {
            return Err(BugReportError::ScreenshotUploadFailed);
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|_| BugReportError::ScreenshotUploadFailed)?;

        let html_url = json["content"]["html_url"]
            .as_str()
            .ok_or(BugReportError::ScreenshotUploadFailed)?;

        // Use the GitHub blob URL rather than raw.githubusercontent.com.
        // Raw URLs 404 for private repos, while blob URLs with `?raw=1`
        // render for users who can view the issue.
        Ok(github_blob_html_url_to_renderable_url(html_url))
    }

    async fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: Vec<&str>,
    ) -> Result<String, BugReportError> {
        let url = format!("https://api.github.com/repos/{repo}/issues");

        let req_body = serde_json::json!({
            "title": title,
            "body": body,
            "labels": labels,
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token.inner()))
            .header("User-Agent", "wavis-backend")
            .header("Accept", "application/vnd.github+json")
            .json(&req_body)
            .send()
            .await
            .map_err(|_| BugReportError::IssueCreationFailed)?;

        if !resp.status().is_success() {
            return Err(BugReportError::IssueCreationFailed);
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|_| BugReportError::IssueCreationFailed)?;

        json["html_url"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or(BugReportError::IssueCreationFailed)
    }
}

// ---------------------------------------------------------------------------
// Domain types and submit_bug_report orchestrator
// ---------------------------------------------------------------------------

/// PNG magic bytes: `\x89PNG\r\n\x1a\n`.
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Identity information extracted from the validated JWT (if authenticated).
pub struct AuthenticatedIdentity {
    #[allow(dead_code)]
    pub user_id: Uuid,
    pub device_id: Uuid,
}

/// Validated bug report request (after handler validation).
pub struct ValidatedBugReport {
    pub title: String,
    pub body: String,
    #[allow(dead_code)]
    pub category: String,
    pub screenshot: Option<Vec<u8>>, // raw PNG bytes (already decoded from base64)
}

/// Submit a complete bug report: validate screenshot, upload if present, create GitHub issue.
pub async fn submit_bug_report(
    github_client: &dyn GitHubClient,
    repo: &str,
    request: ValidatedBugReport,
    identity: Option<AuthenticatedIdentity>,
) -> Result<String, BugReportError> {
    let mut body = request.body;

    // 1. If screenshot present: validate PNG magic bytes, upload, embed in body.
    if let Some(ref png_bytes) = request.screenshot {
        if png_bytes.len() < 8 || png_bytes[..8] != PNG_MAGIC {
            return Err(BugReportError::InvalidScreenshot);
        }

        // Base64-encode the screenshot bytes for the GitHub Contents API.
        let content_base64 = base64::engine::general_purpose::STANDARD.encode(png_bytes);

        // Server-generated UUID filename.
        let filename = format!("bug-reports/{}.png", Uuid::new_v4());

        let image_url = github_client
            .upload_contents(
                repo,
                &filename,
                &content_base64,
                "upload bug report screenshot",
            )
            .await?;

        // Embed as Markdown image in the issue body.
        body.push_str(&format!("\n\n![Screenshot]({})\n", image_url));
    }

    // 2. If authenticated, append device_id metadata.
    if let Some(ref id) = identity {
        body.push_str(&format!("\n\n---\n_Device ID: {}_", id.device_id));
    }

    // 3. Build label list: always "bug-report", add "anonymous" if unauthenticated.
    let mut labels: Vec<&str> = vec!["bug-report"];
    if identity.is_none() {
        labels.push("anonymous");
    }

    // 4. Create the GitHub issue.
    let issue_url = github_client
        .create_issue(repo, &request.title, &body, labels)
        .await?;

    Ok(issue_url)
}

// ---------------------------------------------------------------------------
// MockGitHubClient — test implementation
// ---------------------------------------------------------------------------

/// Records what method was called on the mock.
#[derive(Debug, Clone, PartialEq)]
pub enum MockGitHubCall {
    UploadContents {
        repo: String,
        path: String,
        content_base64: String,
        message: String,
    },
    CreateIssue {
        repo: String,
        title: String,
        body: String,
        labels: Vec<String>,
    },
}

/// Configurable responses for the mock GitHub client.
#[derive(Debug, Clone)]
pub struct MockGitHubConfig {
    pub upload_contents_result: Result<String, BugReportError>,
    pub create_issue_result: Result<String, BugReportError>,
}

impl Default for MockGitHubConfig {
    fn default() -> Self {
        Self {
            upload_contents_result: Ok(
                "https://github.com/test/repo/blob/main/bug-reports/test.png?raw=1".to_string(),
            ),
            create_issue_result: Ok("https://github.com/test/repo/issues/1".to_string()),
        }
    }
}

/// Mock GitHub client that records calls for test assertions.
/// Returns configurable success/error responses.
pub struct MockGitHubClient {
    pub calls: std::sync::Arc<std::sync::Mutex<Vec<MockGitHubCall>>>,
    pub config: std::sync::Arc<std::sync::Mutex<MockGitHubConfig>>,
}

#[allow(dead_code)]
impl MockGitHubClient {
    pub fn new() -> Self {
        Self {
            calls: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            config: std::sync::Arc::new(std::sync::Mutex::new(MockGitHubConfig::default())),
        }
    }

    pub fn with_config(config: MockGitHubConfig) -> Self {
        Self {
            calls: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            config: std::sync::Arc::new(std::sync::Mutex::new(config)),
        }
    }

    pub fn get_calls(&self) -> Vec<MockGitHubCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Default for MockGitHubClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GitHubClient for MockGitHubClient {
    async fn upload_contents(
        &self,
        repo: &str,
        path: &str,
        content_base64: &str,
        message: &str,
    ) -> Result<String, BugReportError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockGitHubCall::UploadContents {
                repo: repo.to_string(),
                path: path.to_string(),
                content_base64: content_base64.to_string(),
                message: message.to_string(),
            });
        let config = self.config.lock().unwrap();
        match &config.upload_contents_result {
            Ok(url) => Ok(url.clone()),
            Err(_) => Err(BugReportError::ScreenshotUploadFailed),
        }
    }

    async fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: Vec<&str>,
    ) -> Result<String, BugReportError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockGitHubCall::CreateIssue {
                repo: repo.to_string(),
                title: title.to_string(),
                body: body.to_string(),
                labels: labels.into_iter().map(|s| s.to_string()).collect(),
            });
        let config = self.config.lock().unwrap();
        match &config.create_issue_result {
            Ok(url) => Ok(url.clone()),
            Err(_) => Err(BugReportError::IssueCreationFailed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_github_client_records_upload_contents() {
        let mock = MockGitHubClient::new();
        let result = mock
            .upload_contents(
                "owner/repo",
                "bug-reports/test.png",
                "base64data",
                "upload screenshot",
            )
            .await;

        assert!(result.is_ok());
        let calls = mock.get_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            MockGitHubCall::UploadContents {
                repo: "owner/repo".to_string(),
                path: "bug-reports/test.png".to_string(),
                content_base64: "base64data".to_string(),
                message: "upload screenshot".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn mock_github_client_records_create_issue() {
        let mock = MockGitHubClient::new();
        let result = mock
            .create_issue("owner/repo", "Bug: test", "body text", vec!["bug-report"])
            .await;

        assert!(result.is_ok());
        let calls = mock.get_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            MockGitHubCall::CreateIssue {
                repo: "owner/repo".to_string(),
                title: "Bug: test".to_string(),
                body: "body text".to_string(),
                labels: vec!["bug-report".to_string()],
            }
        );
    }

    #[tokio::test]
    async fn mock_github_client_returns_configured_error_on_upload() {
        let config = MockGitHubConfig {
            upload_contents_result: Err(BugReportError::ScreenshotUploadFailed),
            ..MockGitHubConfig::default()
        };
        let mock = MockGitHubClient::with_config(config);
        let result = mock
            .upload_contents("owner/repo", "path", "data", "msg")
            .await;

        assert!(result.is_err());
        // Call is still recorded even on error
        assert_eq!(mock.get_calls().len(), 1);
    }

    #[tokio::test]
    async fn mock_github_client_returns_configured_error_on_create_issue() {
        let config = MockGitHubConfig {
            create_issue_result: Err(BugReportError::IssueCreationFailed),
            ..MockGitHubConfig::default()
        };
        let mock = MockGitHubClient::with_config(config);
        let result = mock
            .create_issue("owner/repo", "title", "body", vec!["bug-report"])
            .await;

        assert!(result.is_err());
        assert_eq!(mock.get_calls().len(), 1);
    }

    #[test]
    fn bug_report_error_display() {
        assert_eq!(
            format!("{}", BugReportError::InvalidScreenshot),
            "invalid screenshot: not valid PNG"
        );
        assert_eq!(
            format!("{}", BugReportError::ScreenshotUploadFailed),
            "screenshot upload failed"
        );
        assert_eq!(
            format!("{}", BugReportError::IssueCreationFailed),
            "issue creation failed"
        );
        assert_eq!(
            format!("{}", BugReportError::PayloadTooLarge),
            "payload too large"
        );
    }

    #[test]
    fn real_github_client_wraps_sensitive_token() {
        let client = RealGitHubClient::new(Sensitive("ghp_test_token_123".to_string()));
        // Token is wrapped in Sensitive — Debug/Display must redact
        assert_eq!(format!("{:?}", client.token), "[REDACTED]");
        assert_eq!(format!("{}", client.token), "[REDACTED]");
        // Inner value is still accessible for API calls
        assert_eq!(client.token.inner(), "ghp_test_token_123");
    }

    #[test]
    fn github_blob_html_url_is_converted_to_renderable_url() {
        assert_eq!(
            github_blob_html_url_to_renderable_url(
                "https://github.com/test/repo/blob/main/bug-reports/test.png",
            ),
            "https://github.com/test/repo/blob/main/bug-reports/test.png?raw=1"
        );
    }

    // -----------------------------------------------------------------------
    // submit_bug_report() unit tests
    // -----------------------------------------------------------------------

    /// Valid PNG header for test fixtures.
    fn valid_png_bytes() -> Vec<u8> {
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.extend_from_slice(&[0x00; 32]); // dummy IHDR-ish payload
        bytes
    }

    #[tokio::test]
    async fn submit_bug_report_authenticated_with_screenshot() {
        let mock = MockGitHubClient::new();
        let identity = Some(AuthenticatedIdentity {
            user_id: Uuid::nil(),
            device_id: Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap(),
        });
        let request = ValidatedBugReport {
            title: "Audio cuts out".to_string(),
            body: "Steps to reproduce...".to_string(),
            category: "audio".to_string(),
            screenshot: Some(valid_png_bytes()),
        };

        let result = submit_bug_report(&mock, "owner/repo", request, identity).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://github.com/test/repo/issues/1");

        let calls = mock.get_calls();
        assert_eq!(calls.len(), 2);

        // First call: upload_contents with server-generated UUID path
        match &calls[0] {
            MockGitHubCall::UploadContents { repo, path, .. } => {
                assert_eq!(repo, "owner/repo");
                assert!(path.starts_with("bug-reports/"));
                assert!(path.ends_with(".png"));
            }
            other => panic!("expected UploadContents, got {:?}", other),
        }

        // Second call: create_issue with screenshot embedded + device_id + bug-report label (no anonymous)
        match &calls[1] {
            MockGitHubCall::CreateIssue { body, labels, .. } => {
                assert!(body.contains("![Screenshot]("));
                assert!(body.contains("_Device ID: aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee_"));
                assert_eq!(labels, &vec!["bug-report".to_string()]);
                assert!(!labels.contains(&"anonymous".to_string()));
            }
            other => panic!("expected CreateIssue, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn submit_bug_report_anonymous_no_screenshot() {
        let mock = MockGitHubClient::new();
        let request = ValidatedBugReport {
            title: "UI glitch".to_string(),
            body: "Something is wrong".to_string(),
            category: "ui".to_string(),
            screenshot: None,
        };

        let result = submit_bug_report(&mock, "owner/repo", request, None).await;
        assert!(result.is_ok());

        let calls = mock.get_calls();
        // No upload_contents call — only create_issue
        assert_eq!(calls.len(), 1);

        match &calls[0] {
            MockGitHubCall::CreateIssue { body, labels, .. } => {
                assert!(!body.contains("![Screenshot]("));
                assert!(!body.contains("_Device ID:"));
                assert!(labels.contains(&"bug-report".to_string()));
                assert!(labels.contains(&"anonymous".to_string()));
            }
            other => panic!("expected CreateIssue, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn submit_bug_report_invalid_png_magic_bytes() {
        let mock = MockGitHubClient::new();
        let request = ValidatedBugReport {
            title: "Bug".to_string(),
            body: "body".to_string(),
            category: "other".to_string(),
            screenshot: Some(vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]),
        };

        let result = submit_bug_report(&mock, "owner/repo", request, None).await;
        assert!(matches!(result, Err(BugReportError::InvalidScreenshot)));
        // No GitHub API calls should have been made
        assert_eq!(mock.get_calls().len(), 0);
    }

    #[tokio::test]
    async fn submit_bug_report_screenshot_too_short() {
        let mock = MockGitHubClient::new();
        let request = ValidatedBugReport {
            title: "Bug".to_string(),
            body: "body".to_string(),
            category: "other".to_string(),
            screenshot: Some(vec![0x89, 0x50, 0x4E]), // only 3 bytes
        };

        let result = submit_bug_report(&mock, "owner/repo", request, None).await;
        assert!(matches!(result, Err(BugReportError::InvalidScreenshot)));
        assert_eq!(mock.get_calls().len(), 0);
    }

    #[tokio::test]
    async fn submit_bug_report_upload_failure_propagates() {
        let config = MockGitHubConfig {
            upload_contents_result: Err(BugReportError::ScreenshotUploadFailed),
            ..MockGitHubConfig::default()
        };
        let mock = MockGitHubClient::with_config(config);
        let request = ValidatedBugReport {
            title: "Bug".to_string(),
            body: "body".to_string(),
            category: "other".to_string(),
            screenshot: Some(valid_png_bytes()),
        };

        let result = submit_bug_report(&mock, "owner/repo", request, None).await;
        assert!(matches!(
            result,
            Err(BugReportError::ScreenshotUploadFailed)
        ));
        // upload_contents was called (and failed), create_issue was NOT called
        let calls = mock.get_calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], MockGitHubCall::UploadContents { .. }));
    }

    #[tokio::test]
    async fn submit_bug_report_issue_creation_failure_propagates() {
        let config = MockGitHubConfig {
            create_issue_result: Err(BugReportError::IssueCreationFailed),
            ..MockGitHubConfig::default()
        };
        let mock = MockGitHubClient::with_config(config);
        let request = ValidatedBugReport {
            title: "Bug".to_string(),
            body: "body".to_string(),
            category: "other".to_string(),
            screenshot: None,
        };

        let result = submit_bug_report(&mock, "owner/repo", request, None).await;
        assert!(matches!(result, Err(BugReportError::IssueCreationFailed)));
        // create_issue was called (and failed)
        let calls = mock.get_calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], MockGitHubCall::CreateIssue { .. }));
    }

    // -----------------------------------------------------------------------
    // Property-based tests
    // -----------------------------------------------------------------------

    use proptest::prelude::*;

    /// Strategy: generate a valid PNG byte vector (PNG magic + random payload).
    fn arb_valid_png_bytes() -> impl Strategy<Value = Vec<u8>> {
        proptest::collection::vec(any::<u8>(), 0..256).prop_map(|extra| {
            let mut bytes = PNG_MAGIC.to_vec();
            bytes.extend_from_slice(&extra);
            bytes
        })
    }

    /// Strategy: generate a non-empty arbitrary string (4..128 chars).
    /// Minimum 4 chars avoids false-positive substring matches against UUIDs
    /// (e.g., a 1-char title like "2" trivially appears inside any UUID hex).
    fn arb_nonempty_string() -> impl Strategy<Value = String> {
        "[a-zA-Z][a-zA-Z0-9 _\\-]{3,127}"
    }

    // -----------------------------------------------------------------------
    // Feature: in-app-bug-report, Property 14: Screenshot filename is
    // server-generated
    // **Validates: Requirements 12.5**
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any bug report request containing a screenshot, the filename
        /// used in the GitHub Contents API upload should be a server-generated
        /// UUID — it should not contain or be derived from any field in the
        /// client request payload.
        #[test]
        fn prop_screenshot_filename_is_server_generated(
            title in arb_nonempty_string(),
            body in arb_nonempty_string(),
            category in arb_nonempty_string(),
            screenshot in arb_valid_png_bytes(),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let mock = MockGitHubClient::new();
                let request = ValidatedBugReport {
                    title: title.clone(),
                    body: body.clone(),
                    category: category.clone(),
                    screenshot: Some(screenshot),
                };

                let result = submit_bug_report(&mock, "owner/repo", request, None).await;
                prop_assert!(result.is_ok(), "submit_bug_report should succeed");

                let calls = mock.get_calls();
                // First call must be UploadContents
                let upload_call = calls.iter().find(|c| matches!(c, MockGitHubCall::UploadContents { .. }));
                prop_assert!(upload_call.is_some(), "Expected an UploadContents call");

                if let Some(MockGitHubCall::UploadContents { path, .. }) = upload_call {
                    // Path must be bug-reports/{uuid}.png
                    prop_assert!(
                        path.starts_with("bug-reports/"),
                        "Path should start with 'bug-reports/', got: {}",
                        path,
                    );
                    prop_assert!(
                        path.ends_with(".png"),
                        "Path should end with '.png', got: {}",
                        path,
                    );

                    // Extract the UUID portion between "bug-reports/" and ".png"
                    let uuid_str = &path["bug-reports/".len()..path.len() - ".png".len()];
                    prop_assert!(
                        Uuid::parse_str(uuid_str).is_ok(),
                        "Filename should be a valid UUID, got: {}",
                        uuid_str,
                    );

                    // The filename must NOT contain any client-supplied field
                    let path_lower = path.to_lowercase();
                    prop_assert!(
                        !path_lower.contains(&title.to_lowercase()),
                        "Path should not contain the title '{}', got: {}",
                        title,
                        path,
                    );
                    prop_assert!(
                        !path_lower.contains(&body.to_lowercase()),
                        "Path should not contain the body '{}', got: {}",
                        body,
                        path,
                    );
                    prop_assert!(
                        !path_lower.contains(&category.to_lowercase()),
                        "Path should not contain the category '{}', got: {}",
                        category,
                        path,
                    );
                }

                Ok(())
            })?;
        }
    }

    // -----------------------------------------------------------------------
    // Feature: in-app-bug-report, Property 17: Identity handling —
    // authenticated vs anonymous
    // **Validates: Requirements 13.1, 13.2, 13.3**
    // -----------------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any authenticated bug report submission (valid identity), the
        /// backend should include device_id in the issue metadata. For any
        /// unauthenticated submission (None identity), the backend should
        /// apply an "anonymous" label.
        #[test]
        fn prop_identity_handling_authenticated_vs_anonymous(
            title in arb_nonempty_string(),
            body in arb_nonempty_string(),
            category in arb_nonempty_string(),
            user_id_bytes in prop::array::uniform16(any::<u8>()),
            device_id_bytes in prop::array::uniform16(any::<u8>()),
            is_authenticated in any::<bool>(),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let mock = MockGitHubClient::new();

                let identity = if is_authenticated {
                    Some(AuthenticatedIdentity {
                        user_id: Uuid::from_bytes(user_id_bytes),
                        device_id: Uuid::from_bytes(device_id_bytes),
                    })
                } else {
                    None
                };

                let expected_device_id = Uuid::from_bytes(device_id_bytes).to_string();

                let request = ValidatedBugReport {
                    title,
                    body,
                    category,
                    screenshot: None,
                };

                let result = submit_bug_report(&mock, "owner/repo", request, identity).await;
                prop_assert!(result.is_ok(), "submit_bug_report should succeed");

                let calls = mock.get_calls();
                let issue_call = calls.iter().find(|c| matches!(c, MockGitHubCall::CreateIssue { .. }));
                prop_assert!(issue_call.is_some(), "Expected a CreateIssue call");

                if let Some(MockGitHubCall::CreateIssue { body, labels, .. }) = issue_call {
                    if is_authenticated {
                        // Authenticated: labels contain "bug-report" but NOT "anonymous"
                        prop_assert!(
                            labels.contains(&"bug-report".to_string()),
                            "Authenticated: labels should contain 'bug-report', got: {:?}",
                            labels,
                        );
                        prop_assert!(
                            !labels.contains(&"anonymous".to_string()),
                            "Authenticated: labels should NOT contain 'anonymous', got: {:?}",
                            labels,
                        );
                        // Body should contain the device_id
                        prop_assert!(
                            body.contains(&expected_device_id),
                            "Authenticated: body should contain device_id '{}', got body: {}",
                            expected_device_id,
                            body,
                        );
                    } else {
                        // Anonymous: labels contain both "bug-report" AND "anonymous"
                        prop_assert!(
                            labels.contains(&"bug-report".to_string()),
                            "Anonymous: labels should contain 'bug-report', got: {:?}",
                            labels,
                        );
                        prop_assert!(
                            labels.contains(&"anonymous".to_string()),
                            "Anonymous: labels should contain 'anonymous', got: {:?}",
                            labels,
                        );
                        // Body should NOT contain "Device ID"
                        prop_assert!(
                            !body.contains("Device ID"),
                            "Anonymous: body should NOT contain 'Device ID', got body: {}",
                            body,
                        );
                    }
                }

                Ok(())
            })?;
        }
    }
}
