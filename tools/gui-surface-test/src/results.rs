use std::time::Duration;

/// Describes a single assertion failure found during a scenario run.
pub struct AssertionFailure {
    pub check: String,
    pub expected: String,
    pub actual: String,
}

/// Summary result returned by each scenario after it completes.
pub struct ScenarioResult {
    pub name: String,
    pub passed: bool,
    pub duration: Duration,
    pub failures: Vec<AssertionFailure>,
}
