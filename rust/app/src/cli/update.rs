use std::fs;
use std::path::Path;
use serde::Deserialize;

const GITHUB_REPO: &str = "mihir0209/9router";

#[derive(Debug, Deserialize)]
pub struct GithubRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub html_url: Option<String>,
    pub body: Option<String>,
    #[serde(default)]
    pub assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
pub struct GithubAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: Option<u64>,
}

pub fn print_update_help() {
    println!(
        r#"9r update - Check for and install 9Router binary updates from GitHub releases

Usage:
  9r update [options]
  9r upgrade [options]

Options:
  -c, --check            Check if an update is available without downloading or installing
  -f, --force            Force re-download and install even if already on the latest version
      --tag <tag>        Install a specific release tag (e.g. v0.7.0)
      --url <url>        Query custom release JSON endpoint URL (for testing or mirrors)
  -h, --help             Show this help message

Description:
  Checks GitHub releases for {} (comparing against v{}), downloads the
  appropriate native prebuilt binary for current OS and architecture, validates
  SHA-256 checksums if checksums.txt is published, and safely replaces the current
  executable. If write permissions are lacking, clear instructions are provided.
"#,
        GITHUB_REPO,
        env!("CARGO_PKG_VERSION")
    );
}

/// Parses a semver string like "v0.6.1" or "0.6.0" into (major, minor, patch).
pub fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
    let trimmed = tag.trim().trim_start_matches(|c| c == 'v' || c == 'V');
    let clean_ver = trimmed.split('-').next().unwrap_or(trimmed);
    let parts: Vec<&str> = clean_ver.split('.').collect();
    if parts.len() >= 3 {
        let major = parts[0].parse::<u32>().ok()?;
        let minor = parts[1].parse::<u32>().ok()?;
        let patch = parts[2].parse::<u32>().ok()?;
        Some((major, minor, patch))
    } else if parts.len() == 2 {
        let major = parts[0].parse::<u32>().ok()?;
        let minor = parts[1].parse::<u32>().ok()?;
        Some((major, minor, 0))
    } else {
        None
    }
}

/// Checks whether `latest_tag` is strictly newer than `current_version`.
pub fn is_newer_version(latest_tag: &str, current_version: &str) -> bool {
    match (parse_version(latest_tag), parse_version(current_version)) {
        (Some((l_maj, l_min, l_pat)), Some((c_maj, c_min, c_pat))) => {
            (l_maj, l_min, l_pat) > (c_maj, c_min, c_pat)
        }
        _ => {
            let l = latest_tag.trim().trim_start_matches(|c| c == 'v' || c == 'V');
            let c = current_version.trim().trim_start_matches(|c| c == 'v' || c == 'V');
            l != c
        }
    }
}

/// Resolves candidate asset names for the current target OS and architecture.
pub fn target_asset_names() -> Result<Vec<&'static str>, String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    match (os, arch) {
        ("linux", "x86_64") => Ok(vec!["9r-linux-x64", "9router-linux-x64"]),
        ("linux", "aarch64") => Ok(vec!["9r-linux-arm64", "9router-linux-arm64"]),
        ("macos", "x86_64") => Ok(vec!["9r-darwin-x64", "9router-darwin-x64", "9r-macos-x64"]),
        ("macos", "aarch64") => Ok(vec!["9r-darwin-arm64", "9router-darwin-arm64", "9r-macos-arm64"]),
        ("windows", "x86_64") => Ok(vec!["9r-windows-x64.exe", "9router-windows-x64.exe"]),
        ("windows", "aarch64") => Ok(vec!["9r-windows-arm64.exe", "9router-windows-arm64.exe"]),
        _ => Err(format!(
            "Unsupported OS ({}) and Architecture ({}) for automated binary updates",
            os, arch
        )),
    }
}

/// Extracts the SHA-256 hex checksum for a given asset from a checksums.txt file content.
pub fn parse_checksum_for_asset(checksums_content: &str, asset_name: &str) -> Option<String> {
    for line in checksums_content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        if let (Some(hash), Some(filename)) = (parts.next(), parts.next()) {
            let clean_filename = filename.trim_start_matches('*');
            if clean_filename.eq_ignore_ascii_case(asset_name) {
                return Some(hash.to_lowercase());
            }
        }
    }
    None
}

/// Verifies a buffer's SHA-256 against an expected hex string.
pub fn verify_sha256(data: &[u8], expected_hex: &str) -> bool {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    let computed_hex: String = digest
        .as_ref()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();
    computed_hex.eq_ignore_ascii_case(expected_hex.trim())
}

/// Safely replaces target executable binary with write permission diagnostics.
pub fn replace_binary(new_bytes: &[u8], target_exe: &Path, download_url: Option<&str>) -> anyhow::Result<()> {
    let exe_dir = target_exe.parent().unwrap_or_else(|| Path::new("."));
    let temp_file = exe_dir.join(format!(".9r.tmp-{}", std::process::id()));

    // 1. Write new binary into temporary file
    match fs::write(&temp_file, new_bytes) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            bail_permission_denied(target_exe, download_url);
        }
        Err(e) => return Err(e.into()),
    }

    // 2. Set execute permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(err) = fs::set_permissions(&temp_file, fs::Permissions::from_mode(0o755)) {
            let _ = fs::remove_file(&temp_file);
            if err.kind() == std::io::ErrorKind::PermissionDenied {
                bail_permission_denied(target_exe, download_url);
            }
            return Err(err.into());
        }
    }

    // 3. On Windows, rename active exe before overwriting
    #[cfg(windows)]
    {
        let old_backup = exe_dir.join(format!(".9r.old-{}", std::process::id()));
        let _ = fs::rename(target_exe, &old_backup);
    }

    // 4. Atomic rename into target path
    match fs::rename(&temp_file, target_exe) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            let _ = fs::remove_file(&temp_file);
            bail_permission_denied(target_exe, download_url);
        }
        Err(e) => {
            let _ = fs::remove_file(&temp_file);
            Err(e.into())
        }
    }
}

fn bail_permission_denied(target_exe: &Path, download_url: Option<&str>) -> ! {
    eprintln!("\n\x1b[31m✗ Permission Denied:\x1b[0m Cannot overwrite executable at '{}'", target_exe.display());
    eprintln!("  Updating 9Router requires write permissions to the binary directory.\n");
    eprintln!("To update with administrator privileges, run:");
    eprintln!("  sudo 9r update\n");
    if let Some(url) = download_url {
        eprintln!("Alternatively, manually download and place the binary:");
        eprintln!("  sudo curl -L -o {} {}", target_exe.display(), url);
        #[cfg(unix)]
        eprintln!("  sudo chmod +x {}", target_exe.display());
    }
    std::process::exit(1);
}

pub async fn run_update(args: &[String]) -> anyhow::Result<()> {
    let mut check_only = false;
    let mut force = false;
    let mut custom_url = None;
    let mut tag_override = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_update_help();
                return Ok(());
            }
            "-c" | "--check" => check_only = true,
            "-f" | "--force" => force = true,
            "--url" => {
                i += 1;
                if i < args.len() {
                    custom_url = Some(args[i].clone());
                }
            }
            "--tag" => {
                i += 1;
                if i < args.len() {
                    tag_override = Some(args[i].clone());
                }
            }
            other => {
                eprintln!("Unknown option '{}'. Run '9r update --help' for usage.", other);
                return Ok(());
            }
        }
        i += 1;
    }

    let current_version = env!("CARGO_PKG_VERSION");
    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("\x1b[31m✗ Error:\x1b[0m Failed to determine current binary path: {}", e);
            return Ok(());
        }
    };

    println!("\x1b[36m>\x1b[0m Current 9Router binary: v{} ({})", current_version, current_exe.display());

    let release_url = if let Some(u) = custom_url {
        u
    } else if let Some(ref tag) = tag_override {
        format!("https://api.github.com/repos/{}/releases/tags/{}", GITHUB_REPO, tag)
    } else {
        format!("https://api.github.com/repos/{}/releases/latest", GITHUB_REPO)
    };

    let client = reqwest::Client::builder()
        .user_agent(format!("9router/{}", current_version))
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    println!("\x1b[36m>\x1b[0m Checking for updates from GitHub ({})...", GITHUB_REPO);

    let resp = match client.get(&release_url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\x1b[31m✗ Network Error:\x1b[0m Failed to query GitHub releases: {}", e);
            eprintln!("  Please check your internet connection or try again later.");
            return Ok(());
        }
    };

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("\x1b[33m•\x1b[0m No release found at {} (HTTP 404).", release_url);
        return Ok(());
    }

    if resp.status() == reqwest::StatusCode::FORBIDDEN {
        eprintln!("\x1b[33m•\x1b[0m GitHub API rate limit reached or access forbidden.");
        eprintln!("  Visit https://github.com/{}/releases to check for updates manually.", GITHUB_REPO);
        return Ok(());
    }

    if !resp.status().is_success() {
        eprintln!("\x1b[31m✗ Error:\x1b[0m GitHub API returned HTTP status {}", resp.status());
        return Ok(());
    }

    let release: GithubRelease = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\x1b[31m✗ Error:\x1b[0m Failed to parse GitHub release payload: {}", e);
            return Ok(());
        }
    };

    let latest_tag = release.tag_name.trim();
    let release_name = release.name.as_deref().unwrap_or(latest_tag);
    let newer = is_newer_version(latest_tag, current_version);

    println!("\x1b[36m>\x1b[0m Latest release: {} ({})", latest_tag, release_name);

    if !newer && !force {
        println!("\x1b[32m✓\x1b[0m 9Router is already on the latest version (v{}).", current_version);
        return Ok(());
    }

    if check_only {
        if newer {
            println!(
                "\x1b[33m!\x1b[0m Update available: v{} -> {}! Run '9r update' to upgrade.",
                current_version, latest_tag
            );
        } else {
            println!("\x1b[32m✓\x1b[0m 9Router is up to date (v{}).", current_version);
        }
        return Ok(());
    }

    // Identify candidate assets for current platform
    let candidate_names = match target_asset_names() {
        Ok(names) => names,
        Err(msg) => {
            eprintln!("\x1b[31m✗ Platform Error:\x1b[0m {}", msg);
            return Ok(());
        }
    };

    let mut matched_asset: Option<&GithubAsset> = None;
    for cand in &candidate_names {
        if let Some(asset) = release.assets.iter().find(|a| a.name.eq_ignore_ascii_case(cand)) {
            matched_asset = Some(asset);
            break;
        }
    }

    let asset = match matched_asset {
        Some(a) => a,
        None => {
            let available: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
            eprintln!(
                "\x1b[31m✗ Asset Not Found:\x1b[0m Could not find prebuilt binary for current platform ({:?}).",
                candidate_names
            );
            if !available.is_empty() {
                eprintln!("  Available release assets: {}", available.join(", "));
            }
            eprintln!("  Visit {} to view release files.", release.html_url.as_deref().unwrap_or("GitHub"));
            return Ok(());
        }
    };

    // Check for checksums file (checksums.txt or SHA256SUMS)
    let checksum_asset = release.assets.iter().find(|a| {
        a.name.eq_ignore_ascii_case("checksums.txt")
            || a.name.eq_ignore_ascii_case("sha256sums")
            || a.name.eq_ignore_ascii_case("sha256sums.txt")
    });

    let expected_checksum = if let Some(cs_asset) = checksum_asset {
        println!("\x1b[36m>\x1b[0m Fetching SHA-256 checksums from {}...", cs_asset.name);
        if let Ok(cs_resp) = client.get(&cs_asset.browser_download_url).send().await {
            if let Ok(text) = cs_resp.text().await {
                parse_checksum_for_asset(&text, &asset.name)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Download native binary
    println!("\x1b[36m>\x1b[0m Downloading {} from GitHub...", asset.name);
    let download_resp = match client.get(&asset.browser_download_url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\x1b[31m✗ Download Failed:\x1b[0m {}", e);
            return Ok(());
        }
    };

    if !download_resp.status().is_success() {
        eprintln!("\x1b[31m✗ Download Failed:\x1b[0m HTTP status {}", download_resp.status());
        return Ok(());
    }

    let binary_bytes = match download_resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("\x1b[31m✗ Failed to read downloaded bytes:\x1b[0m {}", e);
            return Ok(());
        }
    };

    println!("  Downloaded {} bytes.", binary_bytes.len());

    // Validate checksum if available
    if let Some(ref exp_hash) = expected_checksum {
        println!("\x1b[36m>\x1b[0m Validating SHA-256 checksum...");
        if !verify_sha256(&binary_bytes, exp_hash) {
            eprintln!("\x1b[31m✗ Checksum Mismatch!\x1b[0m");
            eprintln!("  Expected SHA-256: {}", exp_hash);
            eprintln!("  Download was aborted to protect your system.");
            return Ok(());
        }
        println!("\x1b[32m✓\x1b[0m SHA-256 Checksum verified: {}", exp_hash);
    } else {
        println!("\x1b[33m•\x1b[0m Note: checksums.txt not published in release; skipping checksum validation.");
    }

    // Replace current binary
    println!("\x1b[36m>\x1b[0m Replacing current binary at {}...", current_exe.display());
    replace_binary(&binary_bytes, &current_exe, Some(&asset.browser_download_url))?;

    println!(
        "\x1b[32m\x1b[1m✓ Successfully updated 9Router to {}!\x1b[0m",
        latest_tag
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("0.6.0"), Some((0, 6, 0)));
        assert_eq!(parse_version("v0.6.1"), Some((0, 6, 1)));
        assert_eq!(parse_version("V1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.0"), Some((1, 0, 0)));
        assert_eq!(parse_version("invalid"), None);
    }

    #[test]
    fn test_is_newer_version() {
        assert!(is_newer_version("v0.6.1", "0.6.0"));
        assert!(is_newer_version("0.7.0", "0.6.0"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
        assert!(!is_newer_version("0.6.0", "0.6.0"));
        assert!(!is_newer_version("v0.6.0", "0.6.0"));
        assert!(!is_newer_version("0.5.9", "0.6.0"));
    }

    #[test]
    fn test_target_asset_names() {
        let names = target_asset_names().expect("Should support test runner OS");
        assert!(!names.is_empty());
        assert!(names[0].starts_with("9r-"));
    }

    #[test]
    fn test_parse_checksum_for_asset() {
        let mock_checksums = r#"
# Release SHA-256 Checksums
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  9r-linux-x64
a1b2c3d4e5f60718293a4b5c6d7e8f90123456789abcdef0123456789abcdef0  9r-darwin-arm64
9876543210abcdef0123456789abcdef0123456789abcdef0123456789abcdef *9r-windows-x64.exe
"#;

        assert_eq!(
            parse_checksum_for_asset(mock_checksums, "9r-linux-x64"),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string())
        );
        assert_eq!(
            parse_checksum_for_asset(mock_checksums, "9r-darwin-arm64"),
            Some("a1b2c3d4e5f60718293a4b5c6d7e8f90123456789abcdef0123456789abcdef0".to_string())
        );
        assert_eq!(
            parse_checksum_for_asset(mock_checksums, "9r-windows-x64.exe"),
            Some("9876543210abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string())
        );
        assert_eq!(parse_checksum_for_asset(mock_checksums, "unknown-file"), None);
    }

    #[test]
    fn test_verify_sha256() {
        // SHA-256 of empty bytes: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let empty = b"";
        assert!(verify_sha256(
            empty,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        ));
        assert!(!verify_sha256(empty, "0000000000000000000000000000000000000000000000000000000000000000"));
    }
}
