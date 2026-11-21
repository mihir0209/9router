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
/// standard environment variables, or looking up active keys in the local SQLite database.
pub fn resolve_api_key(key_opt: Option<String>) -> Option<String> {
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

    // Check local SQLite database for an active API key
    let base_dir = std::env::var("DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".9router")
        });
    let db_path = base_dir.join("database.sqlite");
    if db_path.exists() {
        if let Ok(conn) = rusqlite::Connection::open_with_flags(
            &db_path,
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

    None
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
}
