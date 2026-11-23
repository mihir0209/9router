use anyhow::{Context, Result};
use dashmap::DashMap;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose,
    IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

pub const ROOT_CA_CN: &str = "9Router MITM Root CA";
pub const ROOT_CA_ORG: &str = "9Router";
pub const ROOT_CA_COUNTRY: &str = "US";

pub const COMMON_TARGET_HOSTS: &[&str] = &[
    "api.openai.com",
    "api.anthropic.com",
    "cursor.com",
    "gateway.ai.cloudflare.com",
    "daily-cloudcode-pa.googleapis.com",
    "cloudcode-pa.googleapis.com",
    "api.individual.githubcopilot.com",
    "q.us-east-1.amazonaws.com",
    "codewhisperer.us-east-1.amazonaws.com",
    "runtime.us-east-1.kiro.dev",
    "api2.cursor.sh",
];

/// Computes SHA-1 fingerprint of a certificate file in hex format (e.g. `AA:BB:CC:...`)
pub fn get_cert_fingerprint_from_bytes(der_bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, der_bytes);
    digest
        .as_ref()
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(":")
}

/// Extracts DER bytes from a PEM certificate string
pub fn pem_to_der(pem_str: &str) -> Result<Vec<u8>> {
    let mut in_cert = false;
    let mut b64 = String::new();
    for line in pem_str.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("-----BEGIN") {
            in_cert = true;
            continue;
        }
        if trimmed.starts_with("-----END") {
            break;
        }
        if in_cert {
            b64.push_str(trimmed);
        }
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&b64)
        .context("Failed to base64 decode PEM certificate body")?;
    Ok(bytes)
}

/// Compute SHA-1 fingerprint for a PEM certificate file
pub fn get_cert_fingerprint_from_pem_file(path: &Path) -> Result<String> {
    let pem_str = fs::read_to_string(path)
        .with_context(|| format!("Failed to read certificate at {}", path.display()))?;
    let der = pem_to_der(&pem_str)?;
    Ok(get_cert_fingerprint_from_bytes(&der))
}

pub struct CertificateAuthority {
    pub ca_cert_pem: String,
    pub ca_key_pem: String,
    pub ca_cert_der: Vec<u8>,
    pub ca_key: KeyPair,
    pub cert_cache: Arc<DashMap<String, Arc<CertifiedKey>>>,
}

impl CertificateAuthority {
    /// Load existing Root CA from `mitm_dir` or generate a new one if not present.
    pub fn load_or_generate(mitm_dir: &Path) -> Result<Arc<Self>> {
        fs::create_dir_all(mitm_dir)
            .with_context(|| format!("Failed to create MITM dir: {}", mitm_dir.display()))?;

        let key_path = mitm_dir.join("rootCA.key");
        let cert_path = mitm_dir.join("rootCA.crt");

        if key_path.exists() && cert_path.exists() {
            match Self::load_from_files(&cert_path, &key_path) {
                Ok(ca) => {
                    tracing::info!("Loaded existing Root CA from {}", cert_path.display());
                    return Ok(Arc::new(ca));
                }
                Err(e) => {
                    tracing::warn!("Failed to load existing Root CA ({}), regenerating...", e);
                    let _ = fs::remove_file(&key_path);
                    let _ = fs::remove_file(&cert_path);
                }
            }
        }

        let ca = Self::generate(mitm_dir)?;
        Ok(Arc::new(ca))
    }

    /// Load existing CA from given cert and key files.
    pub fn load_from_files(cert_path: &Path, key_path: &Path) -> Result<Self> {
        let ca_cert_pem = fs::read_to_string(cert_path)
            .with_context(|| format!("Failed to read {}", cert_path.display()))?;
        let ca_key_pem = fs::read_to_string(key_path)
            .with_context(|| format!("Failed to read {}", key_path.display()))?;

        let ca_cert_der = pem_to_der(&ca_cert_pem)?;
        let ca_key = KeyPair::from_pem(&ca_key_pem)
            .context("Failed to parse Root CA private key PEM")?;

        Ok(Self {
            ca_cert_pem,
            ca_key_pem,
            ca_cert_der,
            ca_key,
            cert_cache: Arc::new(DashMap::new()),
        })
    }

    /// Generate new Root CA, saving `rootCA.crt` and `rootCA.key` into `mitm_dir`.
    pub fn generate(mitm_dir: &Path) -> Result<Self> {
        fs::create_dir_all(mitm_dir)?;

        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, ROOT_CA_CN);
        dn.push(DnType::OrganizationName, ROOT_CA_ORG);
        dn.push(DnType::CountryName, ROOT_CA_COUNTRY);
        ca_params.distinguished_name = dn;
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];

        let ca_key = KeyPair::generate()
            .context("Failed to generate Root CA key pair")?;
        let ca_cert = ca_params
            .self_signed(&ca_key)
            .context("Failed to self-sign Root CA certificate")?;

        let ca_cert_pem = ca_cert.pem();
        let ca_key_pem = ca_key.serialize_pem();
        let ca_cert_der = ca_cert.der().to_vec();

        let cert_path = mitm_dir.join("rootCA.crt");
        let key_path = mitm_dir.join("rootCA.key");

        fs::write(&cert_path, &ca_cert_pem)
            .with_context(|| format!("Failed to write {}", cert_path.display()))?;
        fs::write(&key_path, &ca_key_pem)
            .with_context(|| format!("Failed to write {}", key_path.display()))?;

        tracing::info!(
            "Generated fresh Root CA at {} (Key: {})",
            cert_path.display(),
            key_path.display()
        );

        Ok(Self {
            ca_cert_pem,
            ca_key_pem,
            ca_cert_der,
            ca_key,
            cert_cache: Arc::new(DashMap::new()),
        })
    }

    /// Dynamically issues a leaf certificate for the given domain name, signed by this Root CA.
    pub fn get_or_generate_leaf_cert(&self, domain: &str) -> Result<Arc<CertifiedKey>> {
        let domain_clean = domain.split(':').next().unwrap_or(domain).trim();
        if let Some(existing) = self.cert_cache.get(domain_clean) {
            return Ok(Arc::clone(existing.value()));
        }

        let san_list = if domain_clean == "localhost" || domain_clean.parse::<std::net::IpAddr>().is_ok() {
            vec![domain_clean.to_string()]
        } else {
            vec![domain_clean.to_string(), format!("*.{}", domain_clean)]
        };

        let mut leaf_params = CertificateParams::new(san_list)
            .context("Failed to create leaf CertificateParams")?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, domain_clean);
        leaf_params.distinguished_name = dn;
        leaf_params.is_ca = IsCa::NoCa;
        leaf_params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        leaf_params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];

        let leaf_key = KeyPair::generate()
            .context("Failed to generate leaf key pair")?;

        let issuer = Issuer::from_ca_cert_pem(&self.ca_cert_pem, &self.ca_key)
            .context("Failed to construct Issuer from Root CA")?;

        let leaf_cert = leaf_params
            .signed_by(&leaf_key, &issuer)
            .context("Failed to sign leaf certificate with Root CA")?;

        let cert_chain = vec![
            CertificateDer::from(leaf_cert.der().to_vec()),
            CertificateDer::from(self.ca_cert_der.clone()),
        ];
        let key_der = PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into());

        let signing_key = rustls::crypto::ring::sign::any_supported_type(&key_der)
            .map_err(|e| anyhow::anyhow!("Failed to create signing key from private key: {:?}", e))?;

        let certified_key = Arc::new(CertifiedKey::new(cert_chain, signing_key));
        self.cert_cache
            .insert(domain_clean.to_string(), Arc::clone(&certified_key));

        Ok(certified_key)
    }

    /// Pre-generate certificates for known target hosts to reduce handshake latency.
    pub fn preload_target_hosts(&self, hosts: &[&str]) {
        for host in hosts {
            if let Err(e) = self.get_or_generate_leaf_cert(host) {
                tracing::warn!("Failed to preload leaf certificate for {}: {}", host, e);
            }
        }
    }
}

/// Dynamic TLS SNI Certificate Resolver implementing `ResolvesServerCert`.
pub struct DynamicCertResolver {
    ca: Arc<CertificateAuthority>,
    default_key: Arc<CertifiedKey>,
}

impl std::fmt::Debug for DynamicCertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicCertResolver")
            .field("cached_domains", &self.ca.cert_cache.len())
            .finish()
    }
}

impl DynamicCertResolver {
    pub fn new(ca: Arc<CertificateAuthority>) -> Result<Self> {
        let default_key = ca.get_or_generate_leaf_cert("localhost")?;
        Ok(Self { ca, default_key })
    }
}

impl ResolvesServerCert for DynamicCertResolver {
    fn resolve(&self, client_hello: ClientHello) -> Option<Arc<CertifiedKey>> {
        let domain = client_hello.server_name().unwrap_or("localhost");
        match self.ca.get_or_generate_leaf_cert(domain) {
            Ok(key) => Some(key),
            Err(e) => {
                tracing::error!("Error generating leaf certificate for SNI {}: {}", domain, e);
                Some(Arc::clone(&self.default_key))
            }
        }
    }
}

/// Builds `rustls::ServerConfig` using dynamic SNI leaf certificate resolver.
pub fn create_mitm_server_config(ca: Arc<CertificateAuthority>) -> Result<rustls::ServerConfig> {
    let resolver = Arc::new(DynamicCertResolver::new(ca)?);

    let mut config = rustls::ServerConfig::builder_with_provider(
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_cert_resolver(resolver);

    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

// ---------------------------------------------------------------------------
// Trust Store Installation Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustStoreStatus {
    Installed,
    NotInstalled,
    Unsupported,
}

/// Check if Root CA is installed in the system or browser trust store.
pub fn check_ca_installed(cert_path: &Path) -> bool {
    if !cert_path.exists() {
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        let fingerprint = match get_cert_fingerprint_from_pem_file(cert_path) {
            Ok(fp) => fp.replace(':', ""),
            Err(_) => return false,
        };
        let out = Command::new("security")
            .args(["find-certificate", "-a", "-c", ROOT_CA_CN, "-Z", "/Library/Keychains/System.keychain"])
            .output();
        if let Ok(output) = out {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.to_lowercase().contains(&fingerprint.to_lowercase()) {
                return true;
            }
        }
        false
    }

    #[cfg(target_os = "windows")]
    {
        let fingerprint = match get_cert_fingerprint_from_pem_file(cert_path) {
            Ok(fp) => fp.replace(':', ""),
            Err(_) => return false,
        };
        let out = Command::new("certutil")
            .args(["-store", "Root", &fingerprint])
            .output();
        if let Ok(output) = out {
            return output.status.success();
        }
        false
    }

    #[cfg(target_os = "linux")]
    {
        let known_dirs = [
            "/usr/local/share/ca-certificates/9router-root-ca.crt",
            "/etc/ca-certificates/trust-source/anchors/9router-root-ca.crt",
            "/etc/pki/ca-trust/source/anchors/9router-root-ca.crt",
            "/etc/pki/trust/anchors/9router-root-ca.crt",
        ];
        for path in &known_dirs {
            if Path::new(path).exists() {
                return true;
            }
        }
        false
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

/// Install Root CA to system trust store
pub fn install_ca_to_system(cert_path: &Path) -> Result<()> {
    if !cert_path.exists() {
        anyhow::bail!("Certificate file not found at {}", cert_path.display());
    }

    #[cfg(target_os = "macos")]
    {
        let cert_str = cert_path.to_str().context("Invalid cert path")?;
        let _ = Command::new("security")
            .args(["delete-certificate", "-c", ROOT_CA_CN, "/Library/Keychains/System.keychain"])
            .status();

        let status = Command::new("sudo")
            .args(["security", "add-trusted-cert", "-d", "-r", "trustRoot", "-k", "/Library/Keychains/System.keychain", cert_str])
            .status()
            .context("Failed to execute security command")?;

        if !status.success() {
            anyhow::bail!("security add-trusted-cert failed with status {:?}", status);
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        let cert_str = cert_path.to_str().context("Invalid cert path")?;
        let _ = Command::new("certutil")
            .args(["-delstore", "Root", ROOT_CA_CN])
            .status();

        let status = Command::new("certutil")
            .args(["-addstore", "Root", cert_str])
            .status()
            .context("Failed to execute certutil command")?;

        if !status.success() {
            anyhow::bail!("certutil -addstore failed with status {:?}", status);
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        let linux_dirs = [
            ("/usr/local/share/ca-certificates", "update-ca-certificates"),
            ("/etc/ca-certificates/trust-source/anchors", "update-ca-trust"),
            ("/etc/pki/ca-trust/source/anchors", "update-ca-trust"),
            ("/etc/pki/trust/anchors", "update-ca-certificates"),
        ];

        let mut target_dir = None;
        let mut update_cmd = "update-ca-certificates";
        for (dir, cmd) in &linux_dirs {
            if Path::new(dir).exists() {
                target_dir = Some(Path::new(dir));
                update_cmd = cmd;
                break;
            }
        }

        let dir = target_dir.unwrap_or_else(|| Path::new("/usr/local/share/ca-certificates"));
        let dest = dir.join("9router-root-ca.crt");

        // Try direct copy, fallback to sudo cp if permission denied
        if let Err(_) = fs::copy(cert_path, &dest) {
            let status = Command::new("sudo")
                .args(["cp", cert_path.to_str().unwrap(), dest.to_str().unwrap()])
                .status()
                .context("Failed to copy cert to system ca directory via sudo")?;
            if !status.success() {
                anyhow::bail!("Failed to copy certificate to {}", dest.display());
            }
            let _ = Command::new("sudo").arg(update_cmd).status();
        } else {
            let _ = Command::new(update_cmd).status();
        }

        tracing::info!("Root CA certificate copied to {}", dest.display());
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        anyhow::bail!("Installing Root CA is not supported on this operating system");
    }
}

/// Uninstall Root CA from system trust store
pub fn uninstall_ca_from_system(_cert_path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let fingerprint = match get_cert_fingerprint_from_pem_file(cert_path) {
            Ok(fp) => fp.replace(':', ""),
            Err(_) => return Ok(()),
        };
        let _ = Command::new("sudo")
            .args(["security", "delete-certificate", "-Z", &fingerprint, "/Library/Keychains/System.keychain"])
            .status();
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("certutil")
            .args(["-delstore", "Root", ROOT_CA_CN])
            .status();
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        let linux_dirs = [
            ("/usr/local/share/ca-certificates/9router-root-ca.crt", "update-ca-certificates"),
            ("/etc/ca-certificates/trust-source/anchors/9router-root-ca.crt", "update-ca-trust"),
            ("/etc/pki/ca-trust/source/anchors/9router-root-ca.crt", "update-ca-trust"),
            ("/etc/pki/trust/anchors/9router-root-ca.crt", "update-ca-certificates"),
        ];

        for (file, cmd) in &linux_dirs {
            let p = Path::new(file);
            if p.exists() {
                if let Err(_) = fs::remove_file(p) {
                    let _ = Command::new("sudo").args(["rm", "-f", file]).status();
                }
                let _ = Command::new("sudo").arg(cmd).status();
                let _ = Command::new(cmd).status();
            }
        }
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn create_test_mitm_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("mitm-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&path);
        let _ = fs::create_dir_all(&path);
        path
    }

    #[test]
    fn test_ca_generation_and_loading() {
        let mitm_dir = create_test_mitm_dir("gen-load");

        let ca = CertificateAuthority::generate(&mitm_dir).unwrap();
        assert!(mitm_dir.join("rootCA.crt").exists());
        assert!(mitm_dir.join("rootCA.key").exists());

        // Verify fingerprint
        let fp = get_cert_fingerprint_from_pem_file(&mitm_dir.join("rootCA.crt")).unwrap();
        assert_eq!(fp.split(':').count(), 20); // SHA-1 is 20 bytes

        // Load existing
        let ca_loaded = CertificateAuthority::load_or_generate(&mitm_dir).unwrap();
        assert_eq!(ca.ca_cert_pem, ca_loaded.ca_cert_pem);

        let _ = fs::remove_dir_all(&mitm_dir);
    }

    #[test]
    fn test_leaf_certificate_generation_and_caching() {
        let mitm_dir = create_test_mitm_dir("leaf-cache");
        let ca = CertificateAuthority::generate(&mitm_dir).unwrap();

        for domain in COMMON_TARGET_HOSTS {
            let key = ca.get_or_generate_leaf_cert(domain).unwrap();
            assert_eq!(key.cert.len(), 2); // Leaf + CA
        }

        // Verify cached
        assert_eq!(ca.cert_cache.len(), COMMON_TARGET_HOSTS.len());
        let cached_key = ca.get_or_generate_leaf_cert("api.openai.com").unwrap();
        assert_eq!(cached_key.cert.len(), 2);

        let _ = fs::remove_dir_all(&mitm_dir);
    }

    #[test]
    fn test_create_mitm_server_config() {
        let mitm_dir = create_test_mitm_dir("server-config");
        let ca = CertificateAuthority::generate(&mitm_dir).unwrap();
        let server_config = create_mitm_server_config(Arc::new(ca)).unwrap();
        assert!(server_config.alpn_protocols.contains(&b"h2".to_vec()));
        assert!(server_config.alpn_protocols.contains(&b"http/1.1".to_vec()));

        let _ = fs::remove_dir_all(&mitm_dir);
    }
}
