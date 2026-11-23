use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub manifest_path: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
    pub url_override: Option<String>,
    pub sha256_override: Option<String>,
    pub allow_unverified_latest: bool,
}

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub wechat_bin: PathBuf,
    pub auto_start: bool,
    pub window_autofit: bool,
    pub show_tray: bool,
    pub health_port: u16,
    pub state_dir: PathBuf,
    pub notify_webhook_url: Option<String>,
    pub notify_redact_body: bool,
}
