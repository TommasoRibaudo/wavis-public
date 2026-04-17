//! Configuration for the GUI surface test harness.

/// Shared context passed to every scenario.
pub struct TestContext {
    /// Base HTTP URL of the backend, e.g. `"http://127.0.0.1:3000"`.
    pub base_url: String,
    /// Bearer token for test-only admin endpoints (TEST_METRICS_TOKEN).
    pub metrics_token: String,
    /// Shared HTTP client for all requests.
    pub http_client: reqwest::Client,
}
