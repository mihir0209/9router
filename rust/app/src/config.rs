use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Application configuration for 9Router.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory for data storage (defaults to ~/.9router or $DATA_DIR).
    pub data_dir: PathBuf,
    /// Path to the SQLite database file (~/.9router/db/data.sqlite).
    pub db_path: PathBuf,
    /// Port for the HTTP server (defaults to 20127 or $PORT).
    pub port: u16,
    /// JWT secret for dashboard sessions and auth tokens.
    pub jwt_secret: String,
}

impl Config {
    /// Load configuration by resolving environment variables and local files.
    pub fn load() -> Self {
        let data_dir = resolve_data_dir();
        let db_path = data_dir.join("db").join("data.sqlite");
        let port = resolve_port();
        let jwt_secret = resolve_jwt_secret(&data_dir);

        Self {
            data_dir,
            db_path,
            port,
            jwt_secret,
        }
    }

    /// Ensure that data directory and database directory exist on disk.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        fs::create_dir_all(&self.data_dir)?;
        fs::create_dir_all(self.data_dir.join("db"))?;
        Ok(())
    }
}

/// Resolves the base data directory:
/// 1. $DATA_DIR env variable if present.
/// 2. ~/.9router (or %APPDATA%/9router on Windows).
pub fn resolve_data_dir() -> PathBuf {
    if let Ok(dir) = env::var("DATA_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return expand_tilde(trimmed);
        }
    }

    if cfg!(windows) {
        if let Ok(app_data) = env::var("APPDATA") {
            let trimmed = app_data.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed).join("9router");
            }
        }
    }

    if let Ok(home) = env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join(".9router");
        }
    }

    if let Ok(user_profile) = env::var("USERPROFILE") {
        let trimmed = user_profile.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join(".9router");
        }
    }

    PathBuf::from(".9router")
}

/// Expands a leading tilde (~/) to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        let home = env::var("HOME").or_else(|_| env::var("USERPROFILE")).unwrap_or_default();
        if !home.is_empty() {
            if path == "~" {
                return PathBuf::from(home);
            }
            return PathBuf::from(home).join(&path[2..]);
        }
    }
    PathBuf::from(path)
}

/// Resolves the HTTP server port:
/// Reads $PORT (e.g. 20127), defaults to 20127.
pub fn resolve_port() -> u16 {
    if let Ok(port_str) = env::var("PORT") {
        if let Ok(port) = port_str.trim().parse::<u16>() {
            if port > 0 {
                return port;
            }
        }
    }
    20127
}

/// Resolves the JWT secret:
/// 1. $JWT_SECRET env variable if set and non-empty.
/// 2. File `<data_dir>/jwt-secret` if present and non-empty.
/// 3. Generates 32 random bytes (64 hex characters), writes to `<data_dir>/jwt-secret`, and returns it.
pub fn resolve_jwt_secret(data_dir: &Path) -> String {
    if let Ok(secret) = env::var("JWT_SECRET") {
        let trimmed = secret.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    let secret_file = data_dir.join("jwt-secret");
    if let Ok(content) = fs::read_to_string(&secret_file) {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Generate fresh secret
    let generated = generate_random_hex(32);
    let _ = fs::create_dir_all(data_dir);
    let _ = fs::write(&secret_file, &generated);
    generated
}

/// Generates random hex string of given byte length.
fn generate_random_hex(bytes_count: usize) -> String {
    // Try /dev/urandom on Unix
    if let Ok(mut file) = fs::File::open("/dev/urandom") {
        use std::io::Read;
        let mut buf = vec![0u8; bytes_count];
        if file.read_exact(&mut buf).is_ok() {
            return buf.iter().map(|b| format!("{:02x}", b)).collect();
        }
    }

    // High entropy fallback using system time, pid, and PCG-style bit mixing
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut state = seed ^ (std::process::id() as u128) << 32;
    let mut out = String::with_capacity(bytes_count * 2);
    for _ in 0..bytes_count {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let byte = (state >> 56) as u8;
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_resolve_port_default() {
        unsafe {
            env::remove_var("PORT");
        }
        assert_eq!(resolve_port(), 20127);
    }

    #[test]
    fn test_resolve_port_custom() {
        unsafe {
            env::set_var("PORT", "3000");
        }
        assert_eq!(resolve_port(), 3000);
        unsafe {
            env::remove_var("PORT");
        }
    }

    #[test]
    fn test_resolve_port_invalid_fallback() {
        unsafe {
            env::set_var("PORT", "invalid-port");
        }
        assert_eq!(resolve_port(), 20127);
        unsafe {
            env::remove_var("PORT");
        }
    }

    #[test]
    fn test_resolve_data_dir_env() {
        unsafe {
            env::set_var("DATA_DIR", "/tmp/9router-test-dir");
        }
        let dir = resolve_data_dir();
        assert_eq!(dir, PathBuf::from("/tmp/9router-test-dir"));
        unsafe {
            env::remove_var("DATA_DIR");
        }
    }

    #[test]
    fn test_jwt_secret_generation_and_persistence() {
        let temp_dir = env::temp_dir().join(format!("9router-test-jwt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);

        unsafe {
            env::remove_var("JWT_SECRET");
        }
        let secret1 = resolve_jwt_secret(&temp_dir);
        assert_eq!(secret1.len(), 64);

        // Read back from file
        let secret2 = resolve_jwt_secret(&temp_dir);
        assert_eq!(secret1, secret2);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_jwt_secret_env_override() {
        let temp_dir = env::temp_dir().join(format!("9router-test-jwt-env-{}", std::process::id()));
        unsafe {
            env::set_var("JWT_SECRET", "custom-super-secret-token");
        }
        let secret = resolve_jwt_secret(&temp_dir);
        assert_eq!(secret, "custom-super-secret-token");
        unsafe {
            env::remove_var("JWT_SECRET");
        }
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
