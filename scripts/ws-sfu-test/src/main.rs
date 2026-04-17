//! WebSocket test client for Wavis signaling.
//! Usage:
//!   cargo run -p ws-sfu-test
//!     Interactive mode. Type JSON messages and press Enter.
//!
//!   cargo run -p ws-sfu-test -- --url wss://.../ws \
//!     --send-json '{"type":"create_room","roomId":"x","roomType":"sfu"}' \
//!     --expect-type room_created --expect-type media_token
//!     Script mode for CI/smoke tests.

use futures_util::{SinkExt, StreamExt};
use std::collections::VecDeque;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct CliArgs {
    url: String,
    send_json: Vec<String>,
    expect_types: Vec<String>,
    stay_open_secs: u64,
    timeout_secs: u64,
    json_output: bool,
}

impl CliArgs {
    fn parse() -> Self {
        let mut args = std::env::args().skip(1);
        let mut url = std::env::var("WS_URL").unwrap_or_else(|_| "ws://localhost:3000/ws".into());
        let mut send_json = Vec::new();
        let mut expect_types = Vec::new();
        let mut stay_open_secs = 0_u64;
        let mut timeout_secs = 15_u64;
        let mut json_output = false;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--url" => {
                    url = args.next().expect("missing value for --url");
                }
                "--send-json" => {
                    send_json.push(args.next().expect("missing value for --send-json"));
                }
                "--expect-type" => {
                    expect_types.push(args.next().expect("missing value for --expect-type"));
                }
                "--stay-open-secs" => {
                    let raw = args.next().expect("missing value for --stay-open-secs");
                    stay_open_secs = raw.parse().expect("invalid integer for --stay-open-secs");
                }
                "--timeout-secs" => {
                    let raw = args.next().expect("missing value for --timeout-secs");
                    timeout_secs = raw.parse().expect("invalid integer for --timeout-secs");
                }
                "--json-output" => {
                    json_output = true;
                }
                other => {
                    eprintln!("Unknown argument: {other}");
                    std::process::exit(2);
                }
            }
        }

        Self {
            url,
            send_json,
            expect_types,
            stay_open_secs,
            timeout_secs,
            json_output,
        }
    }

    fn is_script_mode(&self) -> bool {
        !self.send_json.is_empty() || !self.expect_types.is_empty() || self.json_output
    }
}

fn print_text(text: &str, json_output: bool) {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) if json_output => println!("{}", serde_json::to_string(&v).unwrap()),
        Ok(v) => println!("<< {}", serde_json::to_string(&v).unwrap()),
        Err(_) if json_output => println!("{text}"),
        Err(_) => println!("<< {text}"),
    }
}

async fn drain_for(
    stream: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    duration: Duration,
    json_output: bool,
) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => print_text(&text, json_output),
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Err(_) => break,
            Ok(Some(Err(e))) => {
                eprintln!("websocket error during drain: {e}");
                break;
            }
            Ok(Some(_)) => {}
        }
    }
}

async fn run_script_mode(args: CliArgs) -> i32 {
    let (ws, _) = connect_async(&args.url)
        .await
        .unwrap_or_else(|e| panic!("Failed to connect to {}: {e}", args.url));
    let (mut sink, mut stream) = ws.split();

    for payload in &args.send_json {
        sink.send(Message::Text(payload.clone()))
            .await
            .expect("failed to send scripted message");
    }

    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let mut expected: VecDeque<String> = args.expect_types.into_iter().collect();

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let maybe_msg = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("timed out waiting for websocket response");

        match maybe_msg {
            Some(Ok(Message::Text(text))) => {
                print_text(&text, args.json_output);

                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    let msg_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if matches!(msg_type, "error" | "join_rejected") {
                        eprintln!("script mode received failure message type: {msg_type}");
                        return 1;
                    }
                    if expected.front().map(|t| t.as_str()) == Some(msg_type) {
                        expected.pop_front();
                        if expected.is_empty() {
                            if args.stay_open_secs > 0 {
                                drain_for(
                                    &mut stream,
                                    Duration::from_secs(args.stay_open_secs),
                                    args.json_output,
                                )
                                .await;
                            }
                            return 0;
                        }
                    }
                }
            }
            Some(Ok(Message::Close(frame))) => {
                eprintln!("server closed websocket early: {frame:?}");
                return 1;
            }
            Some(Err(e)) => {
                eprintln!("websocket error: {e}");
                return 1;
            }
            Some(_) => {}
            None => {
                eprintln!("websocket ended before expected responses arrived");
                return 1;
            }
        }
    }

    eprintln!("timed out waiting for expected websocket responses");
    1
}

#[tokio::main]
async fn main() {
    let args = CliArgs::parse();

    if args.is_script_mode() {
        std::process::exit(run_script_mode(args).await);
    }

    let url = args.url;

    println!("Connecting to {url} ...");
    let (ws, _) = connect_async(&url).await.expect("Failed to connect");
    println!("Connected! Type JSON and press Enter. Ctrl+C to quit.");
    println!("Examples:");
    println!(r#"  {{"type":"join","roomId":"test-room","roomType":"sfu"}}"#);
    println!(r#"  {{"type":"leave"}}"#);
    println!();

    let (mut sink, mut stream) = ws.split();

    // Spawn a task to print incoming messages
    let recv_handle = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    print_text(&text, false);
                }
                Ok(Message::Close(frame)) => {
                    println!("<< [Server closed: {frame:?}]");
                    break;
                }
                Err(e) => {
                    eprintln!("<< [Error: {e}]");
                    break;
                }
                _ => {}
            }
        }
    });

    // Read lines from stdin and send as WS text frames
    let stdin = BufReader::new(io::stdin());
    let mut lines = stdin.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        if let Err(e) = sink.send(Message::Text(line)).await {
            eprintln!("Send error: {e}");
            break;
        }
    }

    recv_handle.abort();
    println!("Disconnected.");
}
