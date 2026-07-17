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

/// Aggregate pass/fail counts across a full run's results.
///
/// Extracted out of `main::print_summary` (which also calls
/// `std::process::exit`, making the counting logic itself untestable in
/// place) so the pass/fail arithmetic and the process-exit decision it
/// drives can be verified independently of terminating the process.
pub struct RunSummary {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
}

impl RunSummary {
    pub fn should_exit_nonzero(&self) -> bool {
        self.failed > 0
    }
}

pub fn summarize(results: &[ScenarioResult]) -> RunSummary {
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.len() - passed;
    RunSummary {
        passed,
        failed,
        total: results.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(name: &str, passed: bool) -> ScenarioResult {
        ScenarioResult {
            name: name.to_string(),
            passed,
            duration: Duration::from_millis(0),
            failures: Vec::new(),
        }
    }

    #[test]
    fn empty_results_summarize_to_all_zero_and_do_not_fail_the_run() {
        let summary = summarize(&[]);
        assert_eq!(summary.passed, 0);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.total, 0);
        assert!(!summary.should_exit_nonzero());
    }

    #[test]
    fn all_passed_counts_correctly_and_does_not_fail_the_run() {
        let results = vec![result("a", true), result("b", true), result("c", true)];
        let summary = summarize(&results);
        assert_eq!(summary.passed, 3);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.total, 3);
        assert!(!summary.should_exit_nonzero());
    }

    #[test]
    fn a_single_failure_among_passes_is_counted_and_fails_the_run() {
        let results = vec![result("a", true), result("b", false), result("c", true)];
        let summary = summarize(&results);
        assert_eq!(summary.passed, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.total, 3);
        assert!(summary.should_exit_nonzero());
    }

    #[test]
    fn all_failed_counts_correctly_and_fails_the_run() {
        let results = vec![result("a", false), result("b", false)];
        let summary = summarize(&results);
        assert_eq!(summary.passed, 0);
        assert_eq!(summary.failed, 2);
        assert_eq!(summary.total, 2);
        assert!(summary.should_exit_nonzero());
    }
}
