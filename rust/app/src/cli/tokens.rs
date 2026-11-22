use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use serde_json::json;

use crate::db::Database;
use crate::token_refresh::{
    get_pacing_delay_ms, identify_expiring_tokens, refresh_oauth_token, TokenRefreshConfig,
};

pub fn print_tokens_help() {
    println!(
        r#"9r tokens - Inspect OAuth provider connections and token refresh status

Usage:
  9r tokens [options]

Commands & Flags:
  --check              Inspect provider connections and report token expiry (default)
  --refresh            Identify and refresh tokens expiring within 5 minutes
  --all                Display all OAuth connections regardless of expiry status
  --json               Output scan results in machine-readable JSON format
  -h, --help           Display this help text

Examples:
  9r tokens            # View expiring OAuth connection tokens
  9r tokens --all      # List all configured OAuth connections
  9r tokens --refresh  # Proactively refresh expiring OAuth tokens
"#
    );
}

pub async fn run_tokens(args: &[String]) -> anyhow::Result<()> {
    let mut refresh_now = false;
    let mut show_all = false;
    let mut json_output = false;

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                print_tokens_help();
                return Ok(());
            }
            "--refresh" => {
                refresh_now = true;
            }
            "--check" => {
                // Default inspect mode
            }
            "--all" => {
                show_all = true;
            }
            "--json" => {
                json_output = true;
            }
            other => {
                eprintln!("Unknown option '{}'. Run '9r tokens --help' for usage.", other);
                return Ok(());
            }
        }
    }

    let config = TokenRefreshConfig::load();

    // Resolve SQLite database
    let base_dir = std::env::var("DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".9router")
        });

    let db_path = [
        base_dir.join("db").join("data.sqlite"),
        base_dir.join("data.sqlite"),
        base_dir.join("database.sqlite"),
    ]
    .into_iter()
    .find(|p| p.exists())
    .unwrap_or_else(|| base_dir.join("db").join("data.sqlite"));

    if !db_path.exists() {
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "status": "error",
                    "error": format!("Database not found at {}", db_path.display()),
                    "connections": []
                }))?
            );
        } else {
            println!("\x1b[33m•\x1b[0m Database not found at {}. Start 9Router or specify DATA_DIR.", db_path.display());
        }
        return Ok(());
    }

    let db = Database::open_path(&db_path)?;
    let connections = db.load_provider_connections()?;
    let now = Utc::now();

    let expiring = identify_expiring_tokens(&connections, now, config.expiry_buffer_secs);

    if refresh_now {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        let mut refresh_results = Vec::new();

        if !json_output {
            println!("Refreshed {} expiring token(s)...", expiring.len());
        }

        for (i, cand) in expiring.iter().enumerate() {
            if let Some(conn) = connections.iter().find(|c| c.id == cand.connection_id) {
                let res = refresh_oauth_token(&client, conn, &db).await;
                refresh_results.push(res);

                if i < expiring.len() - 1 {
                    let delay = get_pacing_delay_ms(&conn.provider, &config);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }

        if json_output {
            println!("{}", serde_json::to_string_pretty(&refresh_results)?);
        } else {
            for res in &refresh_results {
                if res.success {
                    println!(
                        "\x1b[32m✔\x1b[0m Refreshed [{}] {} (new expiry: {})",
                        res.provider,
                        res.connection_id,
                        res.new_expires_at.as_deref().unwrap_or("unknown")
                    );
                } else {
                    println!(
                        "\x1b[31m✖\x1b[0m Failed [{}] {}: {}",
                        res.provider,
                        res.connection_id,
                        res.error.as_deref().unwrap_or("unknown error")
                    );
                }
            }
        }
        return Ok(());
    }

    if json_output {
        let out = json!({
            "timestamp": now.to_rfc3339(),
            "scan_window_seconds": config.expiry_buffer_secs,
            "total_connections": connections.len(),
            "expiring_count": expiring.len(),
            "expiring_tokens": expiring,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("\n9Router OAuth Token Lifecycle Status:");
    println!("Scan buffer window: {} seconds (5 minutes)", config.expiry_buffer_secs);
    println!("Database: {}\n", db_path.display());

    if expiring.is_empty() {
        println!("\x1b[32m✔\x1b[0m All OAuth connection tokens are healthy (none expiring within 5 minutes).");
        if !show_all {
            println!("  (Run '9r tokens --all' to inspect all OAuth connections)\n");
            return Ok(());
        }
    }

    println!(
        "{:<12} {:<15} {:<24} {:<15} {:<10}",
        "PROVIDER", "ID", "ACCOUNT / EMAIL", "STATUS", "EXPIRATION"
    );
    println!("{}", "-".repeat(78));

    for cand in &expiring {
        let status = if cand.is_expired {
            "\x1b[31mEXPIRED\x1b[0m"
        } else {
            "\x1b[33mEXPIRING SOON\x1b[0m"
        };
        let email = cand.email.as_deref().or(cand.name.as_deref()).unwrap_or("-");
        let exp_str = match cand.seconds_until_expiry {
            Some(s) if s < 0 => format!("expired {}s ago", -s),
            Some(s) => format!("in {}s", s),
            None => "unknown".to_string(),
        };

        println!(
            "{:<12} {:<15} {:<24} {:<15} {:<10}",
            cand.provider, cand.connection_id, email, status, exp_str
        );
    }

    if show_all {
        for conn in &connections {
            if !expiring.iter().any(|c| c.connection_id == conn.id) {
                let email = conn.email.as_deref().or(conn.name.as_deref()).unwrap_or("-");
                let status = if conn.is_active {
                    "\x1b[32mACTIVE\x1b[0m"
                } else {
                    "\x1b[90mINACTIVE\x1b[0m"
                };
                let exp_str = crate::token_refresh::extract_token_expiry(conn)
                    .map(|dt| {
                        let diff = (dt - now).num_seconds();
                        if diff > 0 {
                            format!("in {}s", diff)
                        } else {
                            format!("expired {}s ago", -diff)
                        }
                    })
                    .unwrap_or_else(|| "none".to_string());

                println!(
                    "{:<12} {:<15} {:<24} {:<15} {:<10}",
                    conn.provider, conn.id, email, status, exp_str
                );
            }
        }
    }

    println!("\n{} expiring token(s) identified.", expiring.len());
    if !expiring.is_empty() {
        println!("Run '9r tokens --refresh' to refresh expiring tokens proactively.\n");
    }

    Ok(())
}
