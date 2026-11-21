pub mod daemon;
pub mod service;
pub mod tools;

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
  9r connect               # Point Claude Code & Codex to http://localhost:20127
  9r logs -f               # Tail gateway log output
  9r stop                  # Terminate background daemon
"#
    );
}

pub fn print_version() {
    println!("9Router (9r) v{}", env!("CARGO_PKG_VERSION"));
}

pub fn dispatch(args: &[String]) -> anyhow::Result<bool> {
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
