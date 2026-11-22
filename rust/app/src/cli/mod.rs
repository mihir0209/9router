pub mod bench;
pub mod common;
pub mod daemon;
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

Flags:
  -v, --version            Display version information
  -h, --help               Display this help text

Examples:
  9r                       # Run server foreground on port 20127
  9r start -d              # Launch background daemon
  9r status                # Check process liveness & RSS memory
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
            let mut daemon = false;
            let mut port = None;
            let mut host = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "-d" | "--daemon" => daemon = true,
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

            if !daemon {
                // If start is called without -d, it runs in foreground
                Ok(false)
            } else {
                let mgr = DaemonManager::new();
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
        _ => {
            // If unknown argument starts with -, check if help or invalid
            if first.starts_with('-') {
                eprintln!("Unknown flag: {}. Run '9r --help' for usage.", first);
                std::process::exit(1);
            }
            // Otherwise, treat as server startup or invalid command
            Ok(false)
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
    async fn test_dispatch_update_help() {
        assert!(dispatch(&["update".to_string(), "--help".to_string()]).await.unwrap());
        assert!(dispatch(&["upgrade".to_string(), "-h".to_string()]).await.unwrap());
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
            config,
            db,
            proxy,
            original,
            combo_tracker,
            token_cache,
            circuit_breaker,
            console_buffer: crate::routes::ConsoleLogBuffer::new(),
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
}
