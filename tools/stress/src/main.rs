pub mod assertions;
pub mod client;
pub mod config;
pub mod log_capture;
pub mod process_stats;
pub mod results;
pub mod runner;
pub mod scenarios;
pub mod server;

use clap::Parser;
use rand::RngCore;
use rand::SeedableRng;
use rand_chacha::ChaChaRng;

use crate::config::{ScaleConfig, TestContext};
use crate::process_stats::ProcessStatsCollector;
use crate::runner::{ScenarioRunner, probe_capabilities};

/// Wavis security stress test harness
#[derive(Parser, Debug)]
#[command(
    name = "stress-harness",
    about = "Security stress tests for the Wavis backend"
)]
struct Args {
    /// Use CI scale (50–200 concurrent clients)
    #[arg(long, conflicts_with = "local")]
    ci: bool,

    /// Use local scale (up to 1000 clients)
    #[arg(long, conflicts_with = "ci")]
    local: bool,

    /// Run a single named scenario
    #[arg(long, value_name = "NAME")]
    scenario: Option<String>,

    /// WebSocket URL of the backend
    #[arg(long, value_name = "WS_URL", default_value = "ws://127.0.0.1:3000/ws")]
    url: String,

    /// Metrics endpoint URL
    #[arg(
        long,
        value_name = "URL",
        default_value = "http://127.0.0.1:3001/test/metrics"
    )]
    metrics_url: String,

    /// Deterministic RNG seed (random if omitted)
    #[arg(long, value_name = "U64")]
    seed: Option<u64>,

    /// Force SFU scenarios to run even if capability probe is inconclusive
    #[arg(long)]
    enable_sfu_tests: bool,

    /// Start the backend in-process instead of connecting to an external one
    #[arg(long)]
    in_process: bool,

    /// WebSocket port for the in-process backend (only used with --in-process)
    #[arg(long, value_name = "PORT", default_value = "3000")]
    in_process_ws_port: u16,

    /// Admin/metrics port for the in-process backend (only used with --in-process)
    #[arg(long, value_name = "PORT", default_value = "3001")]
    in_process_admin_port: u16,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Determine scale: --ci takes priority, then --local, otherwise default to CI.
    let scale = if args.local {
        ScaleConfig::local()
    } else {
        ScaleConfig::ci()
    };

    // Generate or use provided seed.
    let seed = args.seed.unwrap_or_else(|| {
        let mut rng = rand::thread_rng();
        rng.next_u64()
    });
    println!("Seed: {seed}");

    // Read the metrics bearer token from the environment (same var the backend uses).
    // Default to a well-known dev token so `--in-process` works without manual env setup.
    let metrics_token = std::env::var("TEST_METRICS_TOKEN")
        .unwrap_or_else(|_| "stress-harness-default-token".to_string());

    // Optionally start the backend in-process.
    let (ws_url, metrics_url, app_state_opt, process_stats, log_capture_opt) = if args.in_process {
        // Install log capture BEFORE starting the backend so all backend log events
        // are captured from the very beginning.
        let log_capture = crate::log_capture::install_log_capture();

        let srv = server::start_in_process(
            args.in_process_ws_port,
            args.in_process_admin_port,
            &metrics_token,
        )
        .await;
        println!(
            "In-process backend started: ws={} metrics={}",
            srv.ws_url, srv.metrics_url
        );
        let ws_url = srv.ws_url.clone();
        let metrics_url = srv.metrics_url.clone();
        let stats = ProcessStatsCollector::for_self();
        (
            ws_url,
            metrics_url,
            Some(srv.app_state),
            Some(stats),
            Some(log_capture),
        )
    } else {
        let stats = ProcessStatsCollector::from_env();
        (args.url, args.metrics_url, None, stats, None)
    };

    // Probe backend capabilities.
    let capabilities =
        probe_capabilities(&metrics_url, &metrics_token, args.enable_sfu_tests).await;
    println!("Capabilities: {:?}", capabilities);

    // Build shared HTTP client.
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client");

    // Build TestContext.
    let mut ctx = TestContext {
        ws_url,
        metrics_url,
        http_client,
        scale,
        app_state: app_state_opt,
        rng_seed: seed,
        rng: std::sync::Mutex::new(ChaChaRng::seed_from_u64(seed)),
        capabilities,
        process_stats,
        log_capture: log_capture_opt,
    };

    // Build ScenarioRunner with registered scenarios.
    let runner = ScenarioRunner::new(vec![
        Box::new(scenarios::join_flood::JoinFloodScenario),
        Box::new(scenarios::brute_force_invite::BruteForceInviteScenario),
        Box::new(scenarios::join_leave_storm::JoinLeaveStormScenario),
        Box::new(scenarios::invite_exhaustion_race::InviteExhaustionRaceScenario),
        Box::new(scenarios::invite_revocation_race::InviteRevocationRaceScenario),
        Box::new(scenarios::invite_expiry_race::InviteExpiryRaceScenario),
        Box::new(scenarios::cross_room_invite::CrossRoomInviteScenario),
        Box::new(scenarios::authz_fuzz::AuthzFuzzScenario),
        Box::new(scenarios::replay_attack::ReplayAttackScenario),
        Box::new(scenarios::token_confusion::TokenConfusionScenario),
        Box::new(scenarios::screen_share_race::ScreenShareRaceScenario),
        Box::new(scenarios::stop_all_shares_authz::StopAllSharesAuthzScenario),
        Box::new(scenarios::host_directed_stop::HostDirectedStopScenario),
        Box::new(scenarios::share_state_consistency::ShareStateConsistencyScenario),
        Box::new(scenarios::multi_share_flood::MultiShareFloodScenario),
        Box::new(scenarios::message_flood::MessageFloodScenario),
        Box::new(scenarios::oversized_payload::OversizedPayloadScenario),
        Box::new(scenarios::profile_color_fuzz::ProfileColorFuzzScenario),
        Box::new(scenarios::turn_credential_abuse::TurnCredentialAbuseScenario),
        Box::new(scenarios::slowloris::SlowlorisScenario),
        Box::new(scenarios::idle_connection_flood::IdleConnectionFloodScenario),
        Box::new(scenarios::log_leak::LogLeakScenario),
        Box::new(scenarios::auth_brute_force::AuthBruteForceScenario),
        Box::new(scenarios::refresh_token_reuse::RefreshTokenReuseScenario),
        Box::new(scenarios::auth_state_machine_race::AuthStateMachineRaceScenario),
        Box::new(scenarios::cross_secret_token_confusion::CrossSecretTokenConfusionScenario),
        Box::new(scenarios::chat_flood::ChatFloodScenario),
        Box::new(scenarios::chat_authz_fuzz::ChatAuthzFuzzScenario),
    ]);

    // Run scenarios (optionally filtered to a single one).
    let results = runner.run_all(&mut ctx, args.scenario.as_deref()).await;

    if results.is_empty() {
        println!("No scenarios registered.");
        return;
    }

    print_summary(&results, seed);
}

/// Print the final summary report.
///
/// - Pass/fail/skipped counts
/// - Per-scenario: status, duration, throughput, p95/p99 latencies
/// - Per-failing scenario: each invariant violation with expected vs actual
/// - Skipped scenarios with reasons
/// - Seed used for this run
/// - Exits with code 1 if any scenario failed
fn print_summary(results: &[crate::results::ScenarioResult], seed: u64) {
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;

    // Categorise results.
    for r in results {
        if r.name.starts_with("SKIPPED:") {
            skipped += 1;
        } else if r.passed {
            passed += 1;
        } else {
            failed += 1;
        }
    }

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║          Wavis Security Stress Test — Final Report           ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    // --- Per-scenario results ---
    for result in results {
        let is_skipped = result.name.starts_with("SKIPPED:");
        let status = if is_skipped {
            "SKIP"
        } else if result.passed {
            "PASS"
        } else {
            "FAIL"
        };

        println!("[{status}] {name}", name = result.name,);

        if !is_skipped {
            println!(
                "       duration={:.3}s  throughput={:.0} actions/s  p95={:.1}ms  p99={:.1}ms",
                result.duration.as_secs_f64(),
                result.actions_per_second,
                result.p95_latency.as_secs_f64() * 1000.0,
                result.p99_latency.as_secs_f64() * 1000.0,
            );
        }

        for v in &result.violations {
            println!("       ✗ VIOLATION: {}", v.invariant);
            println!("           expected: {}", v.expected);
            println!("           actual:   {}", v.actual);
        }
    }

    // --- Summary counts ---
    println!();
    println!("──────────────────────────────────────────────────────────────");
    println!(
        "  Results:  {} passed  |  {} failed  |  {} skipped  |  {} total",
        passed,
        failed,
        skipped,
        results.len(),
    );
    println!("  Seed:     {seed}");
    println!("──────────────────────────────────────────────────────────────");

    if failed > 0 {
        println!(
            "\n  ✗ {} scenario(s) FAILED — see violations above.",
            failed
        );
        std::process::exit(1);
    } else {
        println!("\n  ✓ All required scenarios passed.");
    }
}
