use std::path::PathBuf;
use crate::cli::daemon::DaemonManager;

/// Resolves the gateway base URL from command line flags, active daemon PID file,
/// environment variables, or falls back to the default port 20127.
pub fn resolve_gateway_url(port_opt: Option<u16>, url_opt: Option<String>) -> String {
    if let Some(mut url) = url_opt {
        if url.ends_with('/') {
            url.pop();
        }
        return url;
    }

    if let Some(port) = port_opt {
        return format!("http://127.0.0.1:{}", port);
    }

    // Check running daemon PID record
    let mgr = DaemonManager::new();
    if let Some(rec) = mgr.read_pid_record() {
        if DaemonManager::is_pid_alive(rec.pid) {
            return format!("http://127.0.0.1:{}", rec.port);
        }
    }

    // Check environment variables
    for env_var in &["GATEWAY_PORT", "PORT", "ROUTER_PORT"] {
        if let Ok(val) = std::env::var(env_var) {
            if let Ok(p) = val.parse::<u16>() {
                return format!("http://127.0.0.1:{}", p);
            }
        }
    }

    "http://127.0.0.1:20127".to_string()
}

/// Resolves an API key for gateway queries by checking CLI arguments,
/// standard environment variables, or looking up active keys in the local SQLite database
/// within the provided base directory.
pub fn resolve_api_key_from_dir(key_opt: Option<String>, base_dir: &std::path::Path) -> Option<String> {
    if let Some(k) = key_opt {
        let trimmed = k.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }

    for env_var in &["NINEROUTER_API_KEY", "NINE_ROUTER_API_KEY", "ROUTER_API_KEY", "API_KEY"] {
        if let Ok(val) = std::env::var(env_var) {
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    // Check local SQLite database for an active API key across known database locations
    let candidate_paths = [
        base_dir.join("db").join("data.sqlite"),
        base_dir.join("database.sqlite"),
        base_dir.join("data.sqlite"),
    ];

    for db_path in &candidate_paths {
        if db_path.exists() {
            if let Ok(conn) = rusqlite::Connection::open_with_flags(
                db_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ) {
                let stmt = conn
                    .prepare("SELECT key FROM apiKeys WHERE isActive = 1 ORDER BY createdAt ASC LIMIT 1")
                    .ok();
                if let Some(mut s) = stmt {
                    if let Ok(k) = s.query_row([], |row| row.get::<_, String>(0)) {
                        let trimmed = k.trim().to_string();
                        if !trimmed.is_empty() {
                            return Some(trimmed);
                        }
                    }
                }
            }
        }
    }

    None
}

/// Resolves an API key for gateway queries by checking CLI arguments,
/// standard environment variables, or looking up active keys in the resolved 9Router data directory.
pub fn resolve_api_key(key_opt: Option<String>) -> Option<String> {
    let base_dir = std::env::var("DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".9router")
        });
    resolve_api_key_from_dir(key_opt, &base_dir)
}

/// Standard graceful shutdown signal listener (Ctrl+C and SIGTERM)
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut stream) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            stream.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_gateway_url_defaults_and_overrides() {
        assert_eq!(
            resolve_gateway_url(Some(8080), None),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            resolve_gateway_url(None, Some("http://localhost:3000/".into())),
            "http://localhost:3000"
        );
        assert_eq!(
            resolve_gateway_url(Some(8080), Some("http://myhost:9999".into())),
            "http://myhost:9999"
        );
    }

    #[test]
    fn test_resolve_api_key_flag() {
        assert_eq!(
            resolve_api_key(Some("sk-test-key".into())),
            Some("sk-test-key".into())
        );
        assert_eq!(
            resolve_api_key(Some("  sk-trimmed  ".into())),
            Some("sk-trimmed".into())
        );
    }

    #[test]
    fn test_resolve_api_key_sqlite_autodiscovery() {
        let temp_dir = std::env::temp_dir().join(format!("9r-test-{}", std::process::id()));
        let db_dir = temp_dir.join("db");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db_file = db_dir.join("data.sqlite");

        let conn = rusqlite::Connection::open(&db_file).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE apiKeys (
                id TEXT PRIMARY KEY,
                key TEXT UNIQUE NOT NULL,
                isActive INTEGER DEFAULT 1,
                createdAt TEXT NOT NULL
            );
            INSERT INTO apiKeys (id, key, isActive, createdAt)
            VALUES ('1', 'sk-autodiscovered-default', 1, '2026-01-01T00:00:00Z');
            "#,
        ).unwrap();

        let discovered = resolve_api_key_from_dir(None, &temp_dir);
        let _ = std::fs::remove_dir_all(&temp_dir);

        assert_eq!(discovered, Some("sk-autodiscovered-default".to_string()));
    }
}
