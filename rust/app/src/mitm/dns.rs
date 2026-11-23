use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub const MITM_TAG: &str = "# 9router-mitm";

pub const TOOL_HOSTS: &[(&str, &[&str])] = &[
    (
        "antigravity",
        &[
            "daily-cloudcode-pa.googleapis.com",
            "cloudcode-pa.googleapis.com",
        ],
    ),
    ("copilot", &["api.individual.githubcopilot.com"]),
    (
        "kiro",
        &[
            "runtime.us-east-1.kiro.dev",
            "q.us-east-1.amazonaws.com",
            "codewhisperer.us-east-1.amazonaws.com",
        ],
    ),
    ("cursor", &["api2.cursor.sh"]),
    ("openai", &["api.openai.com"]),
    ("anthropic", &["api.anthropic.com"]),
    ("cloudflare", &["gateway.ai.cloudflare.com"]),
];

/// Returns the platform-specific hosts file path.
pub fn get_hosts_file_path() -> PathBuf {
    #[cfg(windows)]
    {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        PathBuf::from(system_root)
            .join("System32")
            .join("drivers")
            .join("etc")
            .join("hosts")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/etc/hosts")
    }
}

/// Checks if a host is currently routed to 127.0.0.1 in the hosts content.
pub fn is_host_mapped_to_loopback(hosts_content: &str, host: &str) -> bool {
    let host_lower = host.trim().to_lowercase();
    for line in hosts_content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let ip = parts[0];
            if ip == "127.0.0.1" || ip == "::1" {
                if parts[1..].iter().any(|h| h.to_lowercase() == host_lower) {
                    return true;
                }
            }
        }
    }
    false
}

/// Check DNS mapping status for each configured tool category.
pub fn check_tool_dns_status(hosts_content: &str) -> HashMap<String, bool> {
    let mut status = HashMap::new();
    for &(tool, hosts) in TOOL_HOSTS {
        let all_mapped = hosts
            .iter()
            .all(|h| is_host_mapped_to_loopback(hosts_content, h));
        status.insert(tool.to_string(), all_mapped);
    }
    status
}

/// Compute updated hosts file content with entries added for the specified hosts.
pub fn compute_added_hosts_content(
    original_content: &str,
    hosts_to_add: &[&str],
    ip: &str,
) -> String {
    let mut missing = Vec::new();
    for host in hosts_to_add {
        if !is_host_mapped_to_loopback(original_content, host) {
            missing.push(*host);
        }
    }

    if missing.is_empty() {
        return original_content.to_string();
    }

    let mut out = original_content.trim_end().to_string();
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&format!("\n# === 9Router MITM Interception ===\n"));
    for host in missing {
        out.push_str(&format!("{} {} {}\n", ip, host, MITM_TAG));
    }
    out
}

/// Compute updated hosts file content with entries removed for the specified hosts.
pub fn compute_removed_hosts_content(
    original_content: &str,
    hosts_to_remove: &[&str],
) -> String {
    let lower_targets: Vec<String> = hosts_to_remove
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();

    let mut lines = Vec::new();
    for line in original_content.lines() {
        let trimmed = line.trim();
        // Check if line contains one of our targeted hosts
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 2 && (parts[0] == "127.0.0.1" || parts[0] == "::1") {
            let matches = parts[1..]
                .iter()
                .any(|h| lower_targets.contains(&h.to_lowercase()));
            if matches {
                continue;
            }
        }
        lines.push(line);
    }

    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Compute hosts content with ALL 9router MITM entries removed.
pub fn compute_clean_9router_hosts_content(original_content: &str) -> String {
    let mut all_hosts = Vec::new();
    for &(_, hosts) in TOOL_HOSTS {
        all_hosts.extend_from_slice(hosts);
    }

    let mut lines = Vec::new();
    for line in original_content.lines() {
        if line.contains(MITM_TAG) || line.contains("=== 9Router MITM Interception ===") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && (parts[0] == "127.0.0.1" || parts[0] == "::1") {
            let is_tool_host = parts[1..]
                .iter()
                .any(|h| all_hosts.iter().any(|th| th.eq_ignore_ascii_case(h)));
            if is_tool_host {
                continue;
            }
        }
        lines.push(line);
    }

    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Write hosts content safely with elevation if needed.
pub fn write_hosts_file(content: &str) -> Result<()> {
    let hosts_path = get_hosts_file_path();

    // Try direct write first
    if fs::write(&hosts_path, content).is_ok() {
        return Ok(());
    }

    // Direct write failed (likely needs admin/root elevation)
    #[cfg(unix)]
    {
        let tmp_file = std::env::temp_dir().join(format!("9r_hosts_{}", std::process::id()));
        fs::write(&tmp_file, content)
            .with_context(|| format!("Failed to write tmp hosts to {}", tmp_file.display()))?;

        let status = Command::new("sudo")
            .args(["cp", tmp_file.to_str().unwrap(), hosts_path.to_str().unwrap()])
            .status()
            .context("Failed to copy hosts file with sudo")?;

        let _ = fs::remove_file(&tmp_file);

        if !status.success() {
            anyhow::bail!("Elevated write to {} failed", hosts_path.display());
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        let tmp_file = std::env::temp_dir().join(format!("9r_hosts_{}.tmp", std::process::id()));
        fs::write(&tmp_file, content)
            .with_context(|| format!("Failed to write tmp hosts to {}", tmp_file.display()))?;

        let script = format!(
            "Copy-Item -Path '{}' -Destination '{}' -Force",
            tmp_file.display(),
            hosts_path.display()
        );

        let status = Command::new("powershell")
            .args(["-Command", &format!("Start-Process powershell -Verb RunAs -ArgumentList \"-Command {}\" -Wait", script)])
            .status()
            .context("Failed to run elevated PowerShell to copy hosts file")?;

        let _ = fs::remove_file(&tmp_file);

        if !status.success() {
            anyhow::bail!("Elevated write to {} failed", hosts_path.display());
        }
        Ok(())
    }
}

/// Flushes OS DNS resolver cache.
pub fn flush_dns_cache() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("dscacheutil").arg("-flushcache").status();
        let _ = Command::new("killall").args(["-HUP", "mDNSResponder"]).status();
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("ipconfig").arg("/flushdns").status();
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("resolvectl").arg("flush-caches").status();
        let _ = Command::new("systemd-resolve").arg("--flush-caches").status();
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Ok(())
    }
}

/// Add specified tool or list of hosts to loopback DNS.
pub fn enable_dns_for_hosts(hosts: &[&str]) -> Result<()> {
    let hosts_path = get_hosts_file_path();
    let current = fs::read_to_string(&hosts_path).unwrap_or_default();
    let updated = compute_added_hosts_content(&current, hosts, "127.0.0.1");
    if current != updated {
        write_hosts_file(&updated)?;
        let _ = flush_dns_cache();
    }
    Ok(())
}

/// Remove specified tool or list of hosts from loopback DNS.
pub fn disable_dns_for_hosts(hosts: &[&str]) -> Result<()> {
    let hosts_path = get_hosts_file_path();
    let current = fs::read_to_string(&hosts_path).unwrap_or_default();
    let updated = compute_removed_hosts_content(&current, hosts);
    if current != updated {
        write_hosts_file(&updated)?;
        let _ = flush_dns_cache();
    }
    Ok(())
}

/// Remove all 9router entries from hosts file on MITM shutdown.
pub fn clean_all_mitm_dns() -> Result<()> {
    let hosts_path = get_hosts_file_path();
    let current = fs::read_to_string(&hosts_path).unwrap_or_default();
    let updated = compute_clean_9router_hosts_content(&current);
    if current != updated {
        write_hosts_file(&updated)?;
        let _ = flush_dns_cache();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_host_mapped_to_loopback() {
        let content = r#"
127.0.0.1 localhost
127.0.0.1 api.openai.com # 9router-mitm
::1 ip6-localhost
# 127.0.0.1 cursor.com
"#;
        assert!(is_host_mapped_to_loopback(content, "localhost"));
        assert!(is_host_mapped_to_loopback(content, "api.openai.com"));
        assert!(!is_host_mapped_to_loopback(content, "cursor.com")); // commented out
        assert!(!is_host_mapped_to_loopback(content, "api.anthropic.com"));
    }

    #[test]
    fn test_compute_added_hosts_content() {
        let original = "127.0.0.1 localhost\n";
        let targets = &["api.openai.com", "api.anthropic.com"];
        let updated = compute_added_hosts_content(original, targets, "127.0.0.1");

        assert!(is_host_mapped_to_loopback(&updated, "api.openai.com"));
        assert!(is_host_mapped_to_loopback(&updated, "api.anthropic.com"));
        assert!(updated.contains(MITM_TAG));

        // Adding again should be idempotent
        let second = compute_added_hosts_content(&updated, targets, "127.0.0.1");
        assert_eq!(updated, second);
    }

    #[test]
    fn test_compute_removed_hosts_content() {
        let content = r#"127.0.0.1 localhost
127.0.0.1 api.openai.com # 9router-mitm
127.0.0.1 api.anthropic.com # 9router-mitm
"#;
        let removed = compute_removed_hosts_content(content, &["api.openai.com"]);
        assert!(!is_host_mapped_to_loopback(&removed, "api.openai.com"));
        assert!(is_host_mapped_to_loopback(&removed, "api.anthropic.com"));
        assert!(is_host_mapped_to_loopback(&removed, "localhost"));
    }

    #[test]
    fn test_compute_clean_9router_hosts_content() {
        let content = r#"127.0.0.1 localhost
# === 9Router MITM Interception ===
127.0.0.1 api.openai.com # 9router-mitm
127.0.0.1 api2.cursor.sh # 9router-mitm
192.168.1.1 router.local
"#;
        let cleaned = compute_clean_9router_hosts_content(content);
        assert!(!cleaned.contains("api.openai.com"));
        assert!(!cleaned.contains("api2.cursor.sh"));
        assert!(!cleaned.contains(MITM_TAG));
        assert!(cleaned.contains("127.0.0.1 localhost"));
        assert!(cleaned.contains("192.168.1.1 router.local"));
    }

    #[test]
    fn test_check_tool_dns_status() {
        let content = r#"
127.0.0.1 daily-cloudcode-pa.googleapis.com
127.0.0.1 cloudcode-pa.googleapis.com
127.0.0.1 api2.cursor.sh
"#;
        let status = check_tool_dns_status(content);
        assert_eq!(status.get("antigravity"), Some(&true));
        assert_eq!(status.get("cursor"), Some(&true));
        assert_eq!(status.get("openai"), Some(&false));
    }
}
