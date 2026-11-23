use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct WeChatManifest {
    pub schema_version: u32,
    pub checked_at: String,
    pub architectures: BTreeMap<String, ArchEntry>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ArchEntry {
    pub url: String,
    pub version: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackage {
    pub arch: String,
    pub url: String,
    pub version: String,
    pub expected_sha256: Option<String>,
}

pub fn debian_arch_from_rust(rust_arch: &str) -> Result<&'static str> {
    match rust_arch {
        "x86_64" => Ok("amd64"),
        "aarch64" => Ok("arm64"),
        other => bail!("unsupported architecture: {other}"),
    }
}

pub fn load_manifest(path: &Path) -> Result<WeChatManifest> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read WeChat manifest {}", path.display()))?;
    let manifest: WeChatManifest = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse WeChat manifest {}", path.display()))?;
    if manifest.schema_version != 1 {
        bail!(
            "unsupported manifest schema_version {}",
            manifest.schema_version
        );
    }
    Ok(manifest)
}

pub fn resolve_package(
    manifest: &WeChatManifest,
    deb_arch: &str,
    url_override: Option<&str>,
    sha256_override: Option<&str>,
    allow_unverified_latest: bool,
) -> Result<ResolvedPackage> {
    let entry = manifest
        .architectures
        .get(deb_arch)
        .with_context(|| format!("architecture '{deb_arch}' missing in manifest"))?;

    let url = url_override.unwrap_or(&entry.url).to_string();
    if !url.starts_with("https://") {
        bail!("WeChat download URL must use https://, got '{url}'");
    }

    let expected_sha256 = if let Some(sha) = sha256_override {
        let normalized = normalize_sha256(sha)?;
        Some(normalized)
    } else if allow_unverified_latest {
        None
    } else {
        Some(normalize_sha256(&entry.sha256)?)
    };

    let version = if url_override.is_some() || sha256_override.is_some() {
        "custom".to_string()
    } else {
        entry.version.clone()
    };

    Ok(ResolvedPackage {
        arch: deb_arch.to_string(),
        url,
        version,
        expected_sha256,
    })
}

pub fn normalize_sha256(input: &str) -> Result<String> {
    let trimmed = input.trim().to_ascii_lowercase();
    if trimmed.len() != 64 || !trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid SHA-256 digest '{input}': expected 64 hex characters");
    }
    Ok(trimmed)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("failed to open {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn verify_digest(actual: &str, expected: &str) -> Result<()> {
    let actual_norm = normalize_sha256(actual)?;
    let expected_norm = normalize_sha256(expected)?;
    if actual_norm != expected_norm {
        bail!(
            "SHA-256 mismatch: expected {expected_norm}, got {actual_norm}. \
             Update versions/wechat.json or pass WECHAT_DEB_SHA256."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> WeChatManifest {
        serde_json::from_str(
            r#"{
                "schema_version": 1,
                "checked_at": "2026-11-23T05:37:00Z",
                "architectures": {
                    "amd64": {
                        "url": "https://dldir1v6.qq.com/weixin/Universal/Linux/WeChatLinux_x86_64.deb",
                        "version": "4.1.13.29",
                        "sha256": "d5b21c6e21186a58b2c2fd41712a4ef90f9452735931a6b8500e85c5b0913b0b"
                    },
                    "arm64": {
                        "url": "https://dldir1v6.qq.com/weixin/Universal/Linux/WeChatLinux_arm64.deb",
                        "version": "4.1.13.29",
                        "sha256": "b9ee12ba417ff68840d57020ef7014abb23c7b4f4c00ce807672b100c90bc853"
                    }
                }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn maps_rust_arches() {
        assert_eq!(debian_arch_from_rust("x86_64").unwrap(), "amd64");
        assert_eq!(debian_arch_from_rust("aarch64").unwrap(), "arm64");
        assert!(debian_arch_from_rust("riscv64").is_err());
    }

    #[test]
    fn resolves_pinned_amd64_entry() {
        let pkg = resolve_package(&sample_manifest(), "amd64", None, None, false).unwrap();
        assert_eq!(pkg.version, "4.1.13.29");
        assert_eq!(
            pkg.expected_sha256.as_deref(),
            Some("d5b21c6e21186a58b2c2fd41712a4ef90f9452735931a6b8500e85c5b0913b0b")
        );
    }

    #[test]
    fn rejects_non_https_override() {
        let err = resolve_package(
            &sample_manifest(),
            "amd64",
            Some("http://example.com/wechat.deb"),
            None,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("https://"));
    }

    #[test]
    fn rejects_digest_mismatch() {
        let err = verify_digest(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .unwrap_err();
        assert!(err.to_string().contains("SHA-256 mismatch"));
    }
}
