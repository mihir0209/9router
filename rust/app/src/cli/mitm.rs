use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mitm::ca::{
    check_ca_installed, get_cert_fingerprint_from_pem_file, install_ca_to_system,
    uninstall_ca_from_system, CertificateAuthority,
};
use crate::mitm::dns::{
    check_tool_dns_status, clean_all_mitm_dns, disable_dns_for_hosts, enable_dns_for_hosts,
    get_hosts_file_path, TOOL_HOSTS,
};
use crate::mitm::server::{MitmConfig, MitmServer};

#[derive(Debug, Serialize, Deserialize)]
pub struct MitmPidRecord {
    pub pid: u32,
    pub port: u16,
    pub started_at: String,
    pub exe: String,
}

pub struct MitmDaemonManager {
    run_dir: PathBuf,
    pid_file: PathBuf,
    log_file: PathBuf,
}

impl MitmDaemonManager {
    pub fn new() -> Self {
        let base_dir = crate::config::resolve_data_dir();
        let run_dir = base_dir.join("run");
        let pid_file = run_dir.join("mitm.pid");
        let log_file = run_dir.join("mitm.log");

        Self {
            run_dir,
            pid_file,
            log_file,
        }
    }

    pub fn is_pid_alive(pid: u32) -> bool {
        crate::cli::daemon::DaemonManager::is_pid_alive(pid)
    }

    pub fn read_pid_record(&self) -> Option<MitmPidRecord> {
        if !self.pid_file.exists() {
            return None;
        }
        let content = fs::read_to_string(&self.pid_file).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn write_pid_record(&self, record: &MitmPidRecord) -> Result<()> {
        fs::create_dir_all(&self.run_dir)?;
        let json = serde_json::to_string_pretty(record)?;
        fs::write(&self.pid_file, json)?;
        Ok(())
    }

    pub fn remove_pid_file(&self) {
        let _ = fs::remove_file(&self.pid_file);
    }
}

pub fn print_mitm_help() {
    println!(
        r#"9Router (9r) - Native Transparent MITM TLS Interceptor

Usage:
  9r mitm <command> [options]

Commands:
  start [-d] [--port <p>] [--dns]   Start transparent TLS MITM interceptor
  run [--port <p>]                  Run interceptor in foreground
  stop                              Stop background MITM interceptor & clean DNS
  status                            Show MITM interceptor, Root CA & DNS status
  install-ca                        Install Root CA into OS / browser trust store
  uninstall-ca                      Remove Root CA from OS / browser trust store
  dns [enable|disable|status]       Inspect or toggle loopback DNS entries

Options:
  -p, --port <port>                 MITM listen port (default: 8443, or 443)
  -d, --daemon                      Run in background daemon mode
  --dns                             Automatically configure loopback DNS on start
  --router-base <url>               Upstream 9Router URL (default: http://127.0.0.1:20127)
  -h, --help                        Show this help text
"#
    );
}

pub async fn run_mitm_command(args: &[String]) -> Result<bool> {
    if args.is_empty() {
        print_mitm_help();
        return Ok(true);
    }

    let sub = args[0].as_str();

    match sub {
        "-h" | "--help" | "help" => {
            print_mitm_help();
            Ok(true)
        }
        "start" => {
            let mut daemon = false;
            let mut port = None;
            let mut auto_dns = false;
            let mut router_base = None;

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
                    "--dns" => auto_dns = true,
                    "--router-base" => {
                        i += 1;
                        if i < args.len() {
                            router_base = Some(args[i].clone());
                        }
                    }
                    _ => {}
                }
                i += 1;
            }

            let port = port.unwrap_or_else(|| {
                std::env::var("MITM_PORT")
                    .ok()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(8443)
            });

            let mgr = MitmDaemonManager::new();
            if let Some(rec) = mgr.read_pid_record() {
                if MitmDaemonManager::is_pid_alive(rec.pid) {
                    println!(
                        "\x1b[33m!\x1b[0m 9Router MITM interceptor is already running (PID: {}, Port: {}).",
                        rec.pid, rec.port
                    );
                    println!("  Run '9r mitm status' to inspect or '9r mitm stop' to terminate.");
                    return Ok(true);
                } else {
                    mgr.remove_pid_file();
                }
            }

            let data_dir = crate::config::resolve_data_dir();
            let mitm_dir = data_dir.join("mitm");
            let ca = CertificateAuthority::load_or_generate(&mitm_dir)?;

            if auto_dns {
                let mut all_hosts = Vec::new();
                for &(_, hosts) in TOOL_HOSTS {
                    all_hosts.extend_from_slice(hosts);
                }
                if let Err(e) = enable_dns_for_hosts(&all_hosts) {
                    println!("\x1b[33m!\x1b[0m Warning: Failed to set loopback DNS: {}", e);
                } else {
                    println!("\x1b[32m✔\x1b[0m Configured loopback DNS entries for {} target hosts", all_hosts.len());
                }
            }

            if !daemon {
                let config = MitmConfig {
                    port,
                    router_base: router_base.unwrap_or_else(|| {
                        let p = std::env::var("PORT").unwrap_or_else(|_| "20127".to_string());
                        format!("http://127.0.0.1:{}", p)
                    }),
                    router_api_key: std::env::var("ROUTER_API_KEY").ok(),
                    mitm_dir,
                };
                let server = Arc::new(MitmServer::new(config, ca));
                println!("\x1b[36m>\x1b[0m Starting 9Router MITM TLS interceptor in foreground on port {}...", port);
                server.run(crate::cli::common::shutdown_signal()).await?;
                return Ok(true);
            }

            // Daemon mode
            fs::create_dir_all(&mgr.run_dir)?;
            let current_exe = std::env::current_exe()?;

            let log_out = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&mgr.log_file)?;
            let log_err = log_out.try_clone()?;

            println!("\x1b[36m>\x1b[0m Spawning 9Router MITM background daemon on port {}...", port);

            let mut cmd = Command::new(&current_exe);
            cmd.args(["mitm", "run", "--port", &port.to_string()]);
            if let Some(rb) = router_base {
                cmd.env("MITM_ROUTER_BASE", rb);
            }
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::from(log_out));
            cmd.stderr(Stdio::from(log_err));

            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                cmd.process_group(0);
            }

            let child = cmd.spawn()?;
            let child_pid = child.id();

            let record = MitmPidRecord {
                pid: child_pid,
                port,
                started_at: chrono::Utc::now().to_rfc3339(),
                exe: current_exe.to_string_lossy().to_string(),
            };
            mgr.write_pid_record(&record)?;

            let start_time = Instant::now();
            let target_addr = format!("127.0.0.1:{}", port);
            let mut ready = false;

            while start_time.elapsed() < Duration::from_millis(3000) {
                if !MitmDaemonManager::is_pid_alive(child_pid) {
                    break;
                }
                if TcpStream::connect_timeout(
                    &target_addr.parse().unwrap(),
                    Duration::from_millis(100),
                )
                .is_ok()
                {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            if ready {
                println!("\x1b[32m✔\x1b[0m 9Router MITM interceptor active!");
                println!("  PID:  {}", child_pid);
                println!("  Port: {}", port);
                println!("  Logs: {}", mgr.log_file.display());
            } else {
                println!("\x1b[31m✘\x1b[0m Interceptor process started (PID: {}) but port {} did not respond in time.", child_pid, port);
                println!("  Check logs: tail -n 20 {}", mgr.log_file.display());
            }

            Ok(true)
        }
        "run" => {
            let mut port = None;
            let mut i = 1;
            while i < args.len() {
                if (args[i] == "-p" || args[i] == "--port") && i + 1 < args.len() {
                    port = args[i + 1].parse().ok();
                    i += 1;
                }
                i += 1;
            }

            let port = port.unwrap_or_else(|| {
                std::env::var("MITM_PORT")
                    .ok()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(8443)
            });

            let data_dir = crate::config::resolve_data_dir();
            let mitm_dir = data_dir.join("mitm");
            let ca = CertificateAuthority::load_or_generate(&mitm_dir)?;

            let config = MitmConfig {
                port,
                router_base: std::env::var("MITM_ROUTER_BASE").unwrap_or_else(|_| {
                    let p = std::env::var("PORT").unwrap_or_else(|_| "20127".to_string());
                    format!("http://127.0.0.1:{}", p)
                }),
                router_api_key: std::env::var("ROUTER_API_KEY").ok(),
                mitm_dir,
            };

            let server = Arc::new(MitmServer::new(config, ca));
            server.run(crate::cli::common::shutdown_signal()).await?;
            Ok(true)
        }
        "stop" => {
            let mgr = MitmDaemonManager::new();
            if let Some(rec) = mgr.read_pid_record() {
                if MitmDaemonManager::is_pid_alive(rec.pid) {
                    println!("\x1b[36m>\x1b[0m Stopping 9Router MITM interceptor (PID: {})...", rec.pid);
                    #[cfg(unix)]
                    {
                        let _ = Command::new("kill").arg(rec.pid.to_string()).status();
                    }
                    #[cfg(windows)]
                    {
                        let _ = Command::new("taskkill")
                            .args(["/F", "/PID", &rec.pid.to_string()])
                            .status();
                    }
                }
                mgr.remove_pid_file();
            } else {
                println!("No active MITM daemon recorded.");
            }

            if let Err(e) = clean_all_mitm_dns() {
                println!("\x1b[33m!\x1b[0m Notice: Could not automatically clean /etc/hosts: {}", e);
            } else {
                println!("\x1b[32m✔\x1b[0m Cleaned 9Router loopback DNS entries from hosts file.");
            }

            println!("\x1b[32m✔\x1b[0m 9Router MITM interceptor stopped.");
            Ok(true)
        }
        "status" => {
            let mgr = MitmDaemonManager::new();
            println!("\x1b[1m=== 9Router MITM Interception Status ===\x1b[0m");

            // 1. Process Status
            if let Some(rec) = mgr.read_pid_record() {
                if MitmDaemonManager::is_pid_alive(rec.pid) {
                    println!("Daemon Status: \x1b[32mRunning\x1b[0m");
                    println!("  PID:         {}", rec.pid);
                    println!("  Port:        {}", rec.port);
                    println!("  Started At:  {}", rec.started_at);
                } else {
                    println!("Daemon Status: \x1b[31mDead (stale PID file)\x1b[0m");
                }
            } else {
                println!("Daemon Status: \x1b[90mNot running\x1b[0m");
            }

            // 2. Root CA Status
            let data_dir = crate::config::resolve_data_dir();
            let cert_path = data_dir.join("mitm").join("rootCA.crt");
            println!("\nRoot CA Certificate:");
            if cert_path.exists() {
                println!("  Certificate: \x1b[32mGenerated\x1b[0m ({})", cert_path.display());
                if let Ok(fp) = get_cert_fingerprint_from_pem_file(&cert_path) {
                    println!("  Fingerprint: {}", fp);
                }
                let installed = check_ca_installed(&cert_path);
                if installed {
                    println!("  Trust Store: \x1b[32mInstalled & Trusted\x1b[0m");
                } else {
                    println!("  Trust Store: \x1b[33mNot Installed in OS Store\x1b[0m (Run '9r mitm install-ca')");
                }
            } else {
                println!("  Certificate: \x1b[31mNot generated yet\x1b[0m (Run '9r mitm start' or '9r mitm install-ca')");
            }

            // 3. DNS Status
            let hosts_path = get_hosts_file_path();
            println!("\nLoopback DNS Status ({}):", hosts_path.display());
            if let Ok(content) = fs::read_to_string(&hosts_path) {
                let dns_map = check_tool_dns_status(&content);
                for &(tool, hosts) in TOOL_HOSTS {
                    let active = dns_map.get(tool).copied().unwrap_or(false);
                    let symbol = if active { "\x1b[32m[Active]\x1b[0m" } else { "\x1b[90m[Inactive]\x1b[0m" };
                    println!("  {:<12} {:<10} ({})", tool, symbol, hosts.join(", "));
                }
            } else {
                println!("  Unable to read hosts file.");
            }

            Ok(true)
        }
        "install-ca" => {
            let data_dir = crate::config::resolve_data_dir();
            let mitm_dir = data_dir.join("mitm");
            let _ = CertificateAuthority::load_or_generate(&mitm_dir)?;
            let cert_path = mitm_dir.join("rootCA.crt");

            println!("\x1b[36m>\x1b[0m Installing 9Router Root CA certificate into OS trust store...");
            install_ca_to_system(&cert_path)?;

            if let Ok(fp) = get_cert_fingerprint_from_pem_file(&cert_path) {
                println!("  SHA-1: {}", fp);
            }
            println!("\x1b[32m✔\x1b[0m Root CA successfully installed and trusted by system!");
            Ok(true)
        }
        "uninstall-ca" => {
            let data_dir = crate::config::resolve_data_dir();
            let cert_path = data_dir.join("mitm").join("rootCA.crt");

            println!("\x1b[36m>\x1b[0m Removing 9Router Root CA certificate from OS trust store...");
            uninstall_ca_from_system(&cert_path)?;
            println!("\x1b[32m✔\x1b[0m Root CA certificate uninstalled.");
            Ok(true)
        }
        "dns" => {
            let action = args.get(1).map(|s| s.as_str()).unwrap_or("status");
            let specific_tool = args.get(2).map(|s| s.as_str());

            let target_hosts: Vec<&str> = if let Some(tool) = specific_tool {
                let found = TOOL_HOSTS.iter().find(|(t, _)| *t == tool);
                match found {
                    Some((_, hosts)) => hosts.to_vec(),
                    None => {
                        eprintln!("Unknown tool: {}. Available: antigravity, copilot, kiro, cursor, openai, anthropic, cloudflare", tool);
                        return Ok(true);
                    }
                }
            } else {
                let mut all = Vec::new();
                for &(_, hosts) in TOOL_HOSTS {
                    all.extend_from_slice(hosts);
                }
                all
            };

            match action {
                "enable" => {
                    println!("\x1b[36m>\x1b[0m Adding loopback DNS entries for {} hosts...", target_hosts.len());
                    enable_dns_for_hosts(&target_hosts)?;
                    println!("\x1b[32m✔\x1b[0m Hosts added to 127.0.0.1 successfully.");
                }
                "disable" => {
                    println!("\x1b[36m>\x1b[0m Removing loopback DNS entries for {} hosts...", target_hosts.len());
                    disable_dns_for_hosts(&target_hosts)?;
                    println!("\x1b[32m✔\x1b[0m Hosts removed successfully.");
                }
                "status" => {
                    let hosts_path = get_hosts_file_path();
                    let content = fs::read_to_string(&hosts_path).unwrap_or_default();
                    let dns_map = check_tool_dns_status(&content);
                    println!("\x1b[1m=== 9Router Tool DNS Configuration ===\x1b[0m");
                    for &(tool, hosts) in TOOL_HOSTS {
                        let active = dns_map.get(tool).copied().unwrap_or(false);
                        let badge = if active { "\x1b[32mENABLED\x1b[0m" } else { "\x1b[90mDISABLED\x1b[0m" };
                        println!("  {:<12} {:<10} -> {}", tool, badge, hosts.join(", "));
                    }
                }
                other => {
                    eprintln!("Unknown dns subcommand: {}. Usage: 9r mitm dns [enable|disable|status] [tool]", other);
                }
            }
            Ok(true)
        }
        other => {
            eprintln!("Unknown mitm subcommand: {}. Run '9r mitm --help' for usage.", other);
            Ok(true)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mitm_cli_help() {
        assert!(run_mitm_command(&["--help".to_string()]).await.unwrap());
        assert!(run_mitm_command(&["-h".to_string()]).await.unwrap());
        assert!(run_mitm_command(&[]).await.unwrap());
    }

    #[tokio::test]
    async fn test_mitm_cli_dns_status() {
        assert!(run_mitm_command(&["dns".to_string(), "status".to_string()]).await.unwrap());
    }

    #[test]
    fn test_mitm_daemon_manager_pid_lifecycle() {
        let mgr = MitmDaemonManager::new();
        let record = MitmPidRecord {
            pid: 9999999,
            port: 8443,
            started_at: "2026-11-23T00:00:00Z".to_string(),
            exe: "/tmp/fake9r".to_string(),
        };
        mgr.write_pid_record(&record).unwrap();
        let loaded = mgr.read_pid_record().unwrap();
        assert_eq!(loaded.pid, 9999999);
        assert_eq!(loaded.port, 8443);
        mgr.remove_pid_file();
        assert!(mgr.read_pid_record().is_none());
    }
}
