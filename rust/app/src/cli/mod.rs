pub mod bench;
pub mod common;
pub mod daemon;
pub mod media;
pub mod mitm;
pub mod models;
pub mod service;
pub mod tokens;
pub mod tools;
pub mod update;

use daemon::DaemonManager;
use service::ServiceManager;

pub fn print_help() {
    println!(
        r#"9Router (9r) - Single-Binary AI Gateway, Token Saver & CLI Daemon Manager

Usage:
  9r [command] [options]

Core Commands:
  run                      Run 9Router AI gateway in foreground (default without arguments)
  start [-d] [--port <p>]  Start gateway in background daemon mode (-d) or foreground
  status                   Show running gateway daemon status (PID, port, memory RSS, logs)
  stop                     Stop the background gateway daemon
  restart [-d]             Restart the gateway daemon
  logs [-n <lines>] [-f]   View gateway logs (with optional follow mode)

AI & Media Commands:
  chat [options] [prompt]  Send chat prompt to gateway, stream response in real-time
  image [options] <prompt> Generate image via /v1/images/generations (-o output.png)
  speech [options] <text>  Convert text to speech via /v1/audio/speech (-o speech.mp3)
  transcribe [opts] <file> Transcribe audio to text via /v1/audio/transcriptions
  video [options] <prompt> Generate video via /v1/videos/generations (-o video.mp4)

Secondary Commands & Tooling:
  models, ls [options]     Query local gateway catalog and display available models & routing
  tokens [options]         Inspect OAuth provider connections and token refresh status
  test, bench [options]    Benchmark local gateway latency (RTT) & verify zero-overhead routing
  update, upgrade [opts]   Check for updates, verify checksums, and update 9Router binary

Tool Configuration:
  connect [url] [options]  Auto-configure local AI coding tools (Claude, Codex, OpenCode, etc.)
  show [tools] [--json]    Display current 9Router configuration across local coding tools

OS Service Integration:
  service install          Install 9Router as native user autostart service (systemd / launchd)
  service uninstall        Remove native user autostart service
  service status           Inspect native user service status

MITM Transparent TLS Interception:
  mitm start [-d] [--port] Start transparent TLS interceptor (daemon or foreground)
  mitm stop                Stop background MITM interceptor & clean DNS
  mitm status              Show MITM interceptor, Root CA & DNS status
  mitm install-ca          Install 9Router Root CA certificate into OS trust store
  mitm uninstall-ca        Remove 9Router Root CA certificate from OS trust store
  mitm dns [enable|disable] Configure loopback DNS in /etc/hosts

Flags:
  -v, --version            Display version information
  -h, --help               Display this help text

Examples:
  9r                       # Run server foreground on port 20127
  9r start -d              # Launch background daemon
  9r status                # Check process liveness & RSS memory
  9r chat "Hello 9Router"  # Send a chat prompt to local gateway
  9r image "Robot cat"     # Generate an image and save to image.png
  9r speech "Welcome!"     # Synthesize speech to speech.mp3
  9r transcribe audio.mp3  # Transcribe audio file to stdout
  9r video "Ocean wave"    # Generate a video and save to video.mp4
  9r models                # List available models and provider routing table
  9r test                  # Benchmark gateway RTT and verify zero-overhead routing
  9r update --check        # Check if a new version is available on GitHub
  9r connect               # Point Claude Code & Codex to http://localhost:20127
  9r logs -f               # Tail gateway log output
  9r stop                  # Terminate background daemon
"#
    );
}

pub fn print_version() {
    println!("9Router (9r) v{}", env!("CARGO_PKG_VERSION"));
}

pub async fn dispatch(args: &[String]) -> anyhow::Result<bool> {
    if args.is_empty() {
        return Ok(false);
    }

    let first = &args[0];

    match first.as_str() {
        "-h" | "--help" | "help" => {
            print_help();
            Ok(true)
        }
        "-v" | "--version" | "version" => {
            print_version();
            Ok(true)
        }
        "run" => {
            // "run" explicitly requests running the server in foreground
            Ok(false)
        }
        "start" => {
            let mut daemon = true;
            let mut port = None;
            let mut host = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "-d" | "--daemon" => daemon = true,
                    "-f" | "--foreground" | "--no-daemon" => daemon = false,
                    "-p" | "--port" => {
                        i += 1;
                        if i < args.len() {
                            port = args[i].parse().ok();
                        }
                    }
                    "--host" => {
                        i += 1;
                        if i < args.len() {
                            host = Some(args[i].clone());
                        }
                    }
                    _ => {}
                }
                i += 1;
            }

            let mgr = DaemonManager::new();
            if !daemon {
                // If foreground explicitly requested, check if port is already running
                let target_port = port.unwrap_or_else(|| {
                    std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(20127)
                });
                let check_addr: std::net::SocketAddr = ([127, 0, 0, 1], target_port).into();
                if std::net::TcpStream::connect_timeout(&check_addr, std::time::Duration::from_millis(100)).is_ok() {
                    eprintln!("\x1b[31mError:\x1b[0m Port {} is already in use by an active 9Router instance or service.", target_port);
                    return Ok(true);
                }
                Ok(false)
            } else {
                mgr.start(true, host, port)?;
                Ok(true)
            }
        }
        "status" => {
            let mgr = DaemonManager::new();
            mgr.status()?;
            Ok(true)
        }
        "stop" => {
            let mgr = DaemonManager::new();
            mgr.stop()?;
            Ok(true)
        }
        "restart" => {
            let mut daemon = true;
            let mut port = None;
            let mut host = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--no-daemon" => daemon = false,
                    "-p" | "--port" => {
                        i += 1;
                        if i < args.len() {
                            port = args[i].parse().ok();
                        }
                    }
                    "--host" => {
                        i += 1;
                        if i < args.len() {
                            host = Some(args[i].clone());
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let mgr = DaemonManager::new();
            mgr.restart(daemon, host, port)?;
            Ok(true)
        }
        "logs" => {
            let mut lines_count = 50;
            let mut follow = false;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "-f" | "--follow" => follow = true,
                    "-n" | "--lines" => {
                        i += 1;
                        if i < args.len() {
                            if let Ok(n) = args[i].parse() {
                                lines_count = n;
                            }
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let mgr = DaemonManager::new();
            mgr.logs(lines_count, follow)?;
            Ok(true)
        }
        "connect" => {
            let mut url = None;
            let mut tools_arg = None;
            let mut api_key = None;
            let mut model = None;
            let mut reset = false;
            let mut i = 1;
            while i < args.len() {
                let a = &args[i];
                if a == "--tools" {
                    i += 1;
                    if i < args.len() { tools_arg = Some(args[i].clone()); }
                } else if a == "--api-key" {
                    i += 1;
                    if i < args.len() { api_key = Some(args[i].clone()); }
                } else if a == "--model" {
                    i += 1;
                    if i < args.len() { model = Some(args[i].clone()); }
                } else if a == "--reset" {
                    reset = true;
                } else if a == "-h" || a == "--help" {
                    println!("Usage: 9r connect [url] [--tools <list>] [--api-key <key>] [--model <model>] [--reset]");
                    return Ok(true);
                } else if !a.starts_with('-') && url.is_none() {
                    url = Some(a.clone());
                }
                i += 1;
            }

            tools::run_connect(url, tools_arg, api_key, model, reset)?;
            Ok(true)
        }
        "show" => {
            let mut tools_arg = None;
            let mut json = false;
            let mut i = 1;
            while i < args.len() {
                let a = &args[i];
                if a == "--tools" {
                    i += 1;
                    if i < args.len() { tools_arg = Some(args[i].clone()); }
                } else if a == "--json" {
                    json = true;
                } else if a == "-h" || a == "--help" {
                    println!("Usage: 9r show [tools] [--tools <list>] [--json]");
                    return Ok(true);
                } else if !a.starts_with('-') {
                    tools_arg = Some(a.clone());
                }
                i += 1;
            }

            tools::run_show(tools_arg, json)?;
            Ok(true)
        }
        "models" | "ls" => {
            models::run_models(&args[1..]).await?;
            Ok(true)
        }
        "chat" => {
            media::run_chat(&args[1..]).await?;
            Ok(true)
        }
        "image" => {
            media::run_image(&args[1..]).await?;
            Ok(true)
        }
        "speech" => {
            media::run_speech(&args[1..]).await?;
            Ok(true)
        }
        "transcribe" => {
            media::run_transcribe(&args[1..]).await?;
            Ok(true)
        }
        "video" => {
            media::run_video(&args[1..]).await?;
            Ok(true)
        }
        "tokens" | "refresh-tokens" => {
            tokens::run_tokens(&args[1..]).await?;
            Ok(true)
        }
        "test" | "bench" => {
            bench::run_bench(&args[1..]).await?;
            Ok(true)
        }
        "update" | "upgrade" => {
            update::run_update(&args[1..]).await?;
            Ok(true)
        }
        "service" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("status");
            match sub {
                "install" => ServiceManager::install()?,
                "uninstall" => ServiceManager::uninstall()?,
                "status" => ServiceManager::status()?,
                "-h" | "--help" => {
                    println!("Usage: 9r service [install|uninstall|status]");
                }
                other => {
                    eprintln!("Unknown service command: {}. Available: install, uninstall, status", other);
                }
            }
            Ok(true)
        }
        "mitm" => {
            mitm::run_mitm_command(&args[1..]).await?;
            Ok(true)
        }
        _ => {
            // If unknown argument starts with -, check if help or invalid
            if first.starts_with('-') {
                eprintln!("Unknown flag: {}. Run '9r --help' for usage.", first);
                std::process::exit(1);
            }
            eprintln!("Unknown command: '{}'. Run '9r --help' for a list of available commands.", first);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dispatch_help_and_version() {
        assert!(dispatch(&["--help".to_string()]).await.unwrap());
        assert!(dispatch(&["help".to_string()]).await.unwrap());
        assert!(dispatch(&["-v".to_string()]).await.unwrap());
        assert!(dispatch(&["version".to_string()]).await.unwrap());
    }

    #[tokio::test]
    async fn test_dispatch_models_help() {
        assert!(dispatch(&["models".to_string(), "--help".to_string()]).await.unwrap());
        assert!(dispatch(&["ls".to_string(), "-h".to_string()]).await.unwrap());
    }

    #[tokio::test]
    async fn test_dispatch_tokens_help() {
        assert!(dispatch(&["tokens".to_string(), "--help".to_string()]).await.unwrap());
        assert!(dispatch(&["refresh-tokens".to_string(), "-h".to_string()]).await.unwrap());
    }

    #[tokio::test]
    async fn test_dispatch_test_help() {
        assert!(dispatch(&["test".to_string(), "--help".to_string()]).await.unwrap());
        assert!(dispatch(&["bench".to_string(), "-h".to_string()]).await.unwrap());
    }

    #[tokio::test]
    async fn test_dispatch_mitm_help() {
        assert!(dispatch(&["mitm".to_string(), "--help".to_string()]).await.unwrap());
        assert!(dispatch(&["mitm".to_string(), "-h".to_string()]).await.unwrap());
    }

    #[tokio::test]
    async fn test_dispatch_update_help() {
        assert!(dispatch(&["update".to_string(), "--help".to_string()]).await.unwrap());
        assert!(dispatch(&["upgrade".to_string(), "-h".to_string()]).await.unwrap());
    }

    #[tokio::test]
    async fn test_dispatch_media_help() {
        assert!(dispatch(&["chat".to_string(), "--help".to_string()]).await.unwrap());
        assert!(dispatch(&["chat".to_string(), "-h".to_string()]).await.unwrap());
        assert!(dispatch(&["image".to_string(), "--help".to_string()]).await.unwrap());
        assert!(dispatch(&["speech".to_string(), "--help".to_string()]).await.unwrap());
        assert!(dispatch(&["transcribe".to_string(), "--help".to_string()]).await.unwrap());
        assert!(dispatch(&["video".to_string(), "--help".to_string()]).await.unwrap());
    }

    #[tokio::test]
    async fn test_dispatch_run_returns_false() {
        assert!(!dispatch(&["run".to_string()]).await.unwrap());
        assert!(!dispatch(&[]).await.unwrap());
    }

    #[tokio::test]
    async fn test_live_gateway_models_and_bench_integration() {
        use std::sync::Arc;
        use crate::db::{ApiKey, Database};
        use crate::config::Config;
        use crate::proxy::ProxyService;

        let db = Database::open_in_memory().unwrap();
        let test_key = ApiKey {
            id: "key-cli-test".to_string(),
            key: "sk-cli-integration-key".to_string(),
            name: Some("CLI Integration Test".to_string()),
            machine_id: None,
            is_active: true,
            created_at: chrono::Utc::now().to_rfc3339(),
            rate_limit_requests: 0,
            rate_limit_window_seconds: 60,
            token_limit_daily: 0,
            token_limit_monthly: 0,
            allowed_models: "*".to_string(),
            blocked_models: String::new(),
            allowed_ips: String::new(),
            blocked_ips: String::new(),
            expires_at: String::new(),
            description: String::new(),
            owner_ref: None,
            max_concurrent_requests: 0,
            allowed_providers: String::new(),
            blocked_providers: String::new(),
            force_provider: String::new(),
            force_model: String::new(),
            allowed_endpoints: String::new(),
            blocked_endpoints: String::new(),
            ..Default::default()
        };
        db.save_api_key(&test_key).unwrap();

        let config = Arc::new(Config {
            data_dir: std::path::PathBuf::from("/tmp"),
            db_path: std::path::PathBuf::from("/tmp/test-cli.sqlite"),
            port: 0,
            jwt_secret: "secret".to_string(),
            ..Default::default()
        });
        let proxy = Arc::new(ProxyService::new());
        let original = crate::original::Original::empty();
        let circuit_breaker = Arc::new(crate::circuit_breaker::CircuitBreakerRegistry::default());
        let combo_tracker = crate::combo_router::ComboTracker::new();
        let token_cache = crate::cache::TokenCache::default();
        let state = crate::routes::AppState {
            config: Arc::clone(&config),
            db,
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer: crate::routes::ConsoleLogBuffer::new(),
            rate_limiter: Arc::new(crate::ratelimit::RateLimiter::from_config(&config)),
        };
        let app = crate::routes::create_router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        // 1. Test models command against live test server
        assert!(dispatch(&[
            "models".to_string(),
            "--port".to_string(),
            port.to_string(),
            "--api-key".to_string(),
            "sk-cli-integration-key".to_string(),
        ]).await.unwrap());

        // 2. Test models --json command against live test server
        assert!(dispatch(&[
            "ls".to_string(),
            "--port".to_string(),
            port.to_string(),
            "--api-key".to_string(),
            "sk-cli-integration-key".to_string(),
            "--json".to_string(),
        ]).await.unwrap());

        // 3. Test test/bench command against live test server
        assert!(dispatch(&[
            "test".to_string(),
            "--port".to_string(),
            port.to_string(),
            "--count".to_string(),
            "3".to_string(),
            "--api-key".to_string(),
            "sk-cli-integration-key".to_string(),
        ]).await.unwrap());

        // 4. Test bench --json command against live test server
        assert!(dispatch(&[
            "bench".to_string(),
            "--port".to_string(),
            port.to_string(),
            "--count".to_string(),
            "3".to_string(),
            "--api-key".to_string(),
            "sk-cli-integration-key".to_string(),
            "--json".to_string(),
        ]).await.unwrap());
    }

    #[tokio::test]
    async fn test_live_gateway_media_commands_integration() {
        use axum::routing::post;
        use axum::Router;
        use axum::response::IntoResponse;
        use axum::http::header;
        use serde_json::json;

        let mock_app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|body: axum::body::Bytes| async move {
                    let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let stream = req.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
                    if stream {
                        (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello \"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"from stream!\"}}]}\n\ndata: [DONE]\n\n",
                        ).into_response()
                    } else {
                        (
                            [(header::CONTENT_TYPE, "application/json")],
                            json!({
                                "id": "chatcmpl-test",
                                "object": "chat.completion",
                                "created": 1234567890,
                                "model": "gpt-4o-mini",
                                "choices": [{
                                    "index": 0,
                                    "message": {
                                        "role": "assistant",
                                        "content": "Hello! How can I assist you today?"
                                    },
                                    "finish_reason": "stop"
                                }]
                            }).to_string(),
                        ).into_response()
                    }
                }),
            )
            .route(
                "/v1/images/generations",
                post(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({
                            "created": 1234567890,
                            "data": [{
                                "b64_json": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
                            }]
                        }).to_string(),
                    ).into_response()
                }),
            )
            .route(
                "/v1/audio/speech",
                post(|| async {
                    (
                        [(header::CONTENT_TYPE, "audio/mpeg")],
                        axum::body::Bytes::from_static(b"ID3\x04\x00\x00\x00\x00\x00#TSSE\x00\x00\x00\x0f\x00\x00\x03Lavf60.16.100\xff\xfb\x90d\x00\x00\x00\x00"),
                    ).into_response()
                }),
            )
            .route(
                "/v1/audio/transcriptions",
                post(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({
                            "text": "This is a test audio transcription."
                        }).to_string(),
                    ).into_response()
                }),
            )
            .route(
                "/v1/videos/generations",
                post(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({
                            "created": 1234567890,
                            "data": [{
                                "b64_json": "AAAAIGZ0eXBpc29tAAACAGlzb21pc28yYXZjMW1wNDEAAAAIZnJlZQAAAA8="
                            }]
                        }).to_string(),
                    ).into_response()
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let _ = axum::serve(listener, mock_app).await;
        });

        let temp_dir = std::env::temp_dir().join(format!("9r-cli-media-test-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();

        let img_path = temp_dir.join("test_img.png").to_string_lossy().to_string();
        let audio_path = temp_dir.join("test_speech.mp3").to_string_lossy().to_string();
        let video_path = temp_dir.join("test_video.mp4").to_string_lossy().to_string();

        // 1. Test chat non-streaming
        assert!(dispatch(&[
            "chat".to_string(),
            "--port".to_string(),
            port.to_string(),
            "What is the speed of light?".to_string(),
        ]).await.unwrap());

        // 2. Test chat streaming
        assert!(dispatch(&[
            "chat".to_string(),
            "--port".to_string(),
            port.to_string(),
            "--stream".to_string(),
            "Write a short poem".to_string(),
        ]).await.unwrap());

        // 3. Test image generation
        assert!(dispatch(&[
            "image".to_string(),
            "--port".to_string(),
            port.to_string(),
            "-o".to_string(),
            img_path.clone(),
            "-s".to_string(),
            "512x512".to_string(),
            "A cute puppy".to_string(),
        ]).await.unwrap());
        assert!(std::path::Path::new(&img_path).exists());

        // 4. Test speech synthesis
        assert!(dispatch(&[
            "speech".to_string(),
            "--port".to_string(),
            port.to_string(),
            "-o".to_string(),
            audio_path.clone(),
            "-v".to_string(),
            "alloy".to_string(),
            "Welcome to 9Router".to_string(),
        ]).await.unwrap());
        assert!(std::path::Path::new(&audio_path).exists());

        // 5. Test transcription (using the audio file generated in step 4)
        assert!(dispatch(&[
            "transcribe".to_string(),
            "--port".to_string(),
            port.to_string(),
            "-m".to_string(),
            "whisper-1".to_string(),
            audio_path.clone(),
        ]).await.unwrap());

        // 6. Test video generation
        assert!(dispatch(&[
            "video".to_string(),
            "--port".to_string(),
            port.to_string(),
            "-o".to_string(),
            video_path.clone(),
            "-d".to_string(),
            "5".to_string(),
            "-a".to_string(),
            "16:9".to_string(),
            "Sunset timelapse over mountains".to_string(),
        ]).await.unwrap());
        assert!(std::path::Path::new(&video_path).exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
