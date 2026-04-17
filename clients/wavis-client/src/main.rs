mod commands;
mod output;
mod repl;

use tokio_tungstenite::{connect_async, connect_async_tls_with_config};
use wavis_cli_test::TungsteniteWs;

/// Parsed CLI arguments.
struct CliArgs {
    server_url: String,
    show_secrets: bool,
    danger_insecure_tls: bool,
}

/// Parse CLI args: `--server <url>` (required), `--show-secrets` and `--danger-insecure-tls` (optional).
fn parse_cli_args() -> CliArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut server_url: Option<String> = None;
    let mut show_secrets = false;
    let mut danger_insecure_tls = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--server" => {
                if let Some(url) = args.get(i + 1) {
                    server_url = Some(url.clone());
                    i += 1;
                }
            }
            "--show-secrets" => {
                show_secrets = true;
            }
            "--danger-insecure-tls" => {
                danger_insecure_tls = true;
            }
            _ => {}
        }
        i += 1;
    }
    let server_url = match server_url {
        Some(url) => url,
        None => {
            output::err("Missing required argument: --server <url>");
            eprintln!("Usage: wavis-client --server <websocket-url> [--show-secrets] [--danger-insecure-tls]");
            std::process::exit(1);
        }
    };
    CliArgs {
        server_url,
        show_secrets,
        danger_insecure_tls,
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let cli = parse_cli_args();

    // CLI flag takes priority; env var is the fallback.
    if cli.show_secrets {
        output::set_show_secrets(true);
    } else {
        output::init_show_secrets();
    }

    let server_url = cli.server_url;
    let ws_stream = if cli.danger_insecure_tls {
        eprintln!("WARNING: TLS certificate validation is DISABLED. Do not use in production.");
        let connector = wavis_cli_test::insecure_tls_connector();
        let (ws_stream, _) =
            connect_async_tls_with_config(&server_url, None, false, Some(connector))
                .await
                .unwrap_or_else(|e| {
                    output::err(&format!("WebSocket connection failed: {e}"));
                    std::process::exit(1);
                });
        ws_stream
    } else {
        let (ws_stream, _) = connect_async(&server_url).await.unwrap_or_else(|e| {
            output::err(&format!("WebSocket connection failed: {e}"));
            std::process::exit(1);
        });
        ws_stream
    };
    output::ok(&format!("Connected to {server_url}"));
    let (ws, incoming_rx) = TungsteniteWs::new(ws_stream);
    let exit_code = repl::run_repl(ws, incoming_rx, &server_url).await;
    std::process::exit(exit_code);
}
