pub mod channel_ops;
pub mod client;
pub mod harness_context;
pub mod results;
pub mod runner;
pub mod scenarios;

use clap::Parser;

use crate::harness_context::TestContext;
use crate::runner::ScenarioRunner;

/// Wavis GUI surface test harness
///
/// Exercises the backend REST endpoints that the GUI client depends on.
/// Requires a running backend with a Postgres database.
#[derive(Parser, Debug)]
#[command(
    name = "gui-surface-test",
    about = "GUI surface tests for the Wavis backend REST API"
)]
struct Args {
    /// Base HTTP URL of the backend
    #[arg(long, value_name = "URL", default_value = "http://127.0.0.1:3000")]
    url: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("gui_surface_test=info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client");

    let ctx = TestContext {
        base_url: args.url,
        metrics_token: std::env::var("TEST_METRICS_TOKEN").unwrap_or_default(),
        http_client,
    };

    let runner = ScenarioRunner::new(vec![
        Box::new(scenarios::auth_flow::AuthFlowScenario),
        Box::new(scenarios::channel_lifecycle::ChannelLifecycleScenario),
        Box::new(scenarios::channel_detail_read::ChannelDetailReadScenario),
        Box::new(scenarios::channel_detail_mutations::ChannelDetailMutationsScenario),
        Box::new(scenarios::channel_detail_role_matrix::ChannelDetailRoleMatrixScenario),
        Box::new(scenarios::channel_detail_concurrency::ChannelDetailConcurrencyScenario),
        Box::new(scenarios::voice_status::VoiceStatusScenario),
        Box::new(scenarios::voice_room_participants::VoiceRoomParticipantsScenario),
        Box::new(scenarios::voice_room_host::VoiceRoomHostScenario),
        Box::new(scenarios::voice_room_share::VoiceRoomShareScenario),
        Box::new(scenarios::voice_room_reconnect::VoiceRoomReconnectScenario),
        Box::new(scenarios::error_edges::ErrorEdgesScenario),
        Box::new(scenarios::media_token::MediaTokenScenario),
        Box::new(scenarios::mute_sync::MuteSyncScenario),
        Box::new(scenarios::screen_share_lifecycle::ScreenShareLifecycleScenario),
        Box::new(scenarios::volume_control::VolumeControlScenario),
        Box::new(scenarios::media_reconnect::MediaReconnectScenario),
    ]);

    let results = runner.run_all(&ctx).await;

    if results.is_empty() {
        println!("No scenarios registered.");
        return;
    }

    print_summary(&results);
}

fn print_summary(results: &[crate::results::ScenarioResult]) {
    let mut passed = 0usize;
    let mut failed = 0usize;

    for r in results {
        if r.passed {
            passed += 1;
        } else {
            failed += 1;
        }
    }

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  Wavis GUI Surface Test — Final Report");
    println!("══════════════════════════════════════════════════════════════\n");

    for result in results {
        let status = if result.passed { "PASS" } else { "FAIL" };
        println!(
            "[{status}] {name}  ({:.3}s)",
            result.duration.as_secs_f64(),
            name = result.name,
        );
        for f in &result.failures {
            println!("       ✗ {}", f.check);
            println!("           expected: {}", f.expected);
            println!("           actual:   {}", f.actual);
        }
    }

    println!();
    println!("──────────────────────────────────────────────────────────────");
    println!(
        "  Results:  {} passed  |  {} failed  |  {} total",
        passed,
        failed,
        results.len(),
    );
    println!("──────────────────────────────────────────────────────────────");

    if failed > 0 {
        println!("\n  ✗ {} scenario(s) FAILED — see failures above.", failed);
        std::process::exit(1);
    } else {
        println!("\n  ✓ All scenarios passed.");
    }
}
