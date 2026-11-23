use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::config::BootstrapConfig;
use crate::manifest::{
    debian_arch_from_rust, load_manifest, resolve_package, sha256_file, verify_digest,
    ResolvedPackage,
};

const INSTALLED_MARKER: &str = "installed.sha256";
const INSTALLED_VERSION_MARKER: &str = "installed.version";
const MACHINE_ID_FILE: &str = "machine-id";

pub fn run_bootstrap(cfg: &BootstrapConfig) -> Result<()> {
    let manifest = load_manifest(&cfg.manifest_path)?;
    let deb_arch = debian_arch_from_rust(std::env::consts::ARCH)?;
    let pkg = resolve_package(
        &manifest,
        deb_arch,
        cfg.url_override.as_deref(),
        cfg.sha256_override.as_deref(),
        cfg.allow_unverified_latest,
    )?;

    fs::create_dir_all(&cfg.cache_dir)
        .with_context(|| format!("failed to create cache dir {}", cfg.cache_dir.display()))?;
    fs::create_dir_all(&cfg.state_dir)
        .with_context(|| format!("failed to create state dir {}", cfg.state_dir.display()))?;

    if let Err(err) = ensure_persistent_machine_id(
        &cfg.cache_dir.join(MACHINE_ID_FILE),
        &[
            Path::new("/etc/machine-id"),
            Path::new("/var/lib/dbus/machine-id"),
        ],
    ) {
        eprintln!("[wechat-bootstrap] Warning: could not sync persistent machine-id: {err}");
    }

    let marker_path = cfg.state_dir.join(INSTALLED_MARKER);
    if let Some(expected) = pkg.expected_sha256.as_deref() {
        if is_already_installed(&marker_path, expected) {
            eprintln!(
                "[wechat-bootstrap] WeChat {} ({}) already installed (sha256 {}...)",
                pkg.version,
                pkg.arch,
                &expected[..12]
            );
            sync_runtime_icons();
            return Ok(());
        }
    }

    let (deb_path, actual_sha256) = ensure_cached_deb(&cfg.cache_dir, &pkg)?;
    install_deb(&deb_path)?;

    let installed_version = query_dpkg_version(&deb_path).unwrap_or_else(|| pkg.version.clone());
    fs::write(&marker_path, format!("{actual_sha256}\n"))?;
    fs::write(
        cfg.state_dir.join(INSTALLED_VERSION_MARKER),
        format!("{installed_version}\n"),
    )?;
    sync_runtime_icons();

    eprintln!(
        "[wechat-bootstrap] Installed WeChat {} ({}, sha256 {}...)",
        installed_version,
        pkg.arch,
        &actual_sha256[..12]
    );
    Ok(())
}

pub fn ensure_persistent_machine_id(persisted_path: &Path, targets: &[&Path]) -> Result<String> {
    let machine_id = match read_valid_machine_id(persisted_path) {
        Some(existing) => existing,
        None => {
            let candidate = targets
                .iter()
                .find_map(|p| read_valid_machine_id(p))
                .unwrap_or(generate_machine_id()?);
            if let Some(parent) = persisted_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(persisted_path, format!("{candidate}\n")).with_context(|| {
                format!(
                    "failed to write persistent machine-id {}",
                    persisted_path.display()
                )
            })?;
            candidate
        }
    };

    let content = format!("{machine_id}\n");
    for target in targets {
        if read_valid_machine_id(target).as_deref() == Some(machine_id.as_str()) {
            continue;
        }
        if let Some(parent) = target.parent() {
            if !parent.exists() {
                let _ = fs::create_dir_all(parent);
            }
        }
        let _ = fs::write(target, &content);
    }

    Ok(machine_id)
}

fn read_valid_machine_id(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.len() == 32 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(trimmed)
    } else {
        None
    }
}

fn generate_machine_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    let mut urandom = File::open("/dev/urandom").context("failed to open /dev/urandom")?;
    urandom
        .read_exact(&mut bytes)
        .context("failed to read random bytes for machine-id")?;
    Ok(hex::encode(bytes))
}

pub fn read_installed_version(state_dir: &Path) -> Option<String> {
    fs::read_to_string(state_dir.join(INSTALLED_VERSION_MARKER))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn is_already_installed(marker_path: &Path, expected_sha256: &str) -> bool {
    if !Path::new("/usr/bin/wechat").exists() {
        return false;
    }
    fs::read_to_string(marker_path)
        .map(|s| s.trim().eq_ignore_ascii_case(expected_sha256))
        .unwrap_or(false)
}

fn ensure_cached_deb(cache_dir: &Path, pkg: &ResolvedPackage) -> Result<(PathBuf, String)> {
    if let Some(expected) = pkg.expected_sha256.as_deref() {
        let cached = cache_dir.join(format!("wechat-{}-{}.deb", pkg.arch, &expected[..16]));
        if cached.exists() {
            match sha256_file(&cached) {
                Ok(actual) if actual == expected => {
                    eprintln!(
                        "[wechat-bootstrap] Using verified cached package {}",
                        cached.display()
                    );
                    return Ok((cached, actual));
                }
                Ok(actual) => {
                    eprintln!(
                        "[wechat-bootstrap] Cached package checksum mismatch ({actual}), re-downloading"
                    );
                    let _ = fs::remove_file(&cached);
                }
                Err(err) => {
                    eprintln!("[wechat-bootstrap] Failed reading cached package: {err}");
                    let _ = fs::remove_file(&cached);
                }
            }
        }
    }

    let tmp_path = cache_dir.join(format!(".wechat-{}-{}.part", pkg.arch, std::process::id()));
    let actual_sha256 = download_and_hash(&pkg.url, &tmp_path)?;

    if let Some(expected) = pkg.expected_sha256.as_deref() {
        if let Err(err) = verify_digest(&actual_sha256, expected) {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
    }

    let final_path = cache_dir.join(format!("wechat-{}-{}.deb", pkg.arch, &actual_sha256[..16]));
    fs::rename(&tmp_path, &final_path).with_context(|| {
        format!(
            "failed to move verified package {} to {}",
            tmp_path.display(),
            final_path.display()
        )
    })?;
    prune_old_cached_debs(cache_dir, &pkg.arch, &final_path);

    Ok((final_path, actual_sha256))
}

fn download_and_hash(url: &str, dest: &Path) -> Result<String> {
    eprintln!("[wechat-bootstrap] Downloading official WeChat package from {url}");
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(300))
        .build();

    let mut last_err = None;
    for attempt in 1..=3 {
        match agent
            .get(url)
            .set("User-Agent", "wechat-selkies-rev/0.1")
            .call()
        {
            Ok(resp) => {
                let mut reader = resp.into_reader();
                let mut out = File::create(dest)
                    .with_context(|| format!("failed to create {}", dest.display()))?;
                let mut hasher = Sha256::new();
                let mut buf = [0u8; 128 * 1024];
                loop {
                    let n = reader.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    out.write_all(&buf[..n])?;
                }
                out.flush()?;
                return Ok(hex::encode(hasher.finalize()));
            }
            Err(err) => {
                eprintln!("[wechat-bootstrap] Download attempt {attempt}/3 failed: {err}");
                let _ = fs::remove_file(dest);
                last_err = Some(err.to_string());
                std::thread::sleep(Duration::from_secs(3 * attempt as u64));
            }
        }
    }
    bail!(
        "failed to download {url} after 3 attempts: {}",
        last_err.unwrap_or_default()
    )
}

fn prune_old_cached_debs(cache_dir: &Path, arch: &str, keep: &Path) {
    let prefix = format!("wechat-{arch}-");
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with(&prefix) && name.ends_with(".deb") {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn install_deb(deb_path: &Path) -> Result<()> {
    let status = Command::new("dpkg")
        .arg("-i")
        .arg(deb_path)
        .status()
        .context("failed to execute dpkg -i")?;
    if status.success() {
        return Ok(());
    }

    eprintln!("[wechat-bootstrap] dpkg reported missing dependencies, running apt-get -f install");
    let fix = Command::new("apt-get")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .args(["install", "-f", "-y", "--no-install-recommends"])
        .status()
        .context("failed to execute apt-get install -f")?;
    if !fix.success() {
        bail!("apt-get install -f failed with status {fix}");
    }
    Ok(())
}

fn query_dpkg_version(deb_path: &Path) -> Option<String> {
    let out = Command::new("dpkg-deb")
        .args(["-f"])
        .arg(deb_path)
        .arg("Version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

fn sync_runtime_icons() {
    let src = Path::new("/usr/share/icons/hicolor/128x128/apps/wechat.png");
    if !src.exists() {
        return;
    }
    for dst in [
        "/usr/share/selkies/www/icon.png",
        "/usr/share/selkies/web/icon.png",
        "/usr/share/selkies/web/favicon.ico",
    ] {
        let dst_path = Path::new(dst);
        if dst_path.parent().map(Path::exists).unwrap_or(false) {
            let _ = fs::copy(src, dst_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "wechat-supervisor-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn generates_and_syncs_persistent_machine_id() {
        let dir = unique_temp_dir("gen");
        let persisted = dir.join("cache/machine-id");
        let etc_id = dir.join("etc/machine-id");
        let dbus_id = dir.join("dbus/machine-id");

        let first = ensure_persistent_machine_id(&persisted, &[&etc_id, &dbus_id]).unwrap();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(fs::read_to_string(&etc_id).unwrap().trim(), first);
        assert_eq!(fs::read_to_string(&dbus_id).unwrap().trim(), first);

        fs::write(&etc_id, "00000000000000000000000000000000\n").unwrap();
        let second = ensure_persistent_machine_id(&persisted, &[&etc_id, &dbus_id]).unwrap();
        assert_eq!(second, first);
        assert_eq!(fs::read_to_string(&etc_id).unwrap().trim(), first);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replaces_corrupt_persisted_machine_id() {
        let dir = unique_temp_dir("corrupt");
        let persisted = dir.join("machine-id");
        let target = dir.join("target-machine-id");
        fs::write(&persisted, "not-a-valid-id\n").unwrap();

        let id = ensure_persistent_machine_id(&persisted, &[&target]).unwrap();
        assert_eq!(id.len(), 32);
        assert_eq!(fs::read_to_string(&persisted).unwrap().trim(), id);
        assert_eq!(fs::read_to_string(&target).unwrap().trim(), id);

        let _ = fs::remove_dir_all(&dir);
    }
}
