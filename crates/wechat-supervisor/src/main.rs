mod bootstrap;
mod config;
mod health;
mod manifest;
mod notify;
mod supervisor;
mod x11_window;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "wechat-supervisor", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Bootstrap {
        #[arg(
            long,
            env = "WECHAT_MANIFEST_PATH",
            default_value = "/etc/wechat-selkies/wechat.json"
        )]
        manifest: PathBuf,
        #[arg(
            long,
            env = "WECHAT_CACHE_DIR",
            default_value = "/config/.wechat-cache"
        )]
        cache_dir: PathBuf,
        #[arg(
            long,
            env = "WECHAT_STATE_DIR",
            default_value = "/var/lib/wechat-selkies"
        )]
        state_dir: PathBuf,
        #[arg(long, env = "WECHAT_DEB_URL")]
        url_override: Option<String>,
        #[arg(long, env = "WECHAT_DEB_SHA256")]
        sha256_override: Option<String>,
        #[arg(long, env = "WECHAT_ALLOW_UNVERIFIED_LATEST", default_value_t = false)]
        allow_unverified_latest: bool,
    },
    Run {
        #[arg(long, env = "WECHAT_BIN", default_value = "/usr/bin/wechat")]
        wechat_bin: PathBuf,
        #[arg(long, env = "AUTO_START_WECHAT", default_value_t = true)]
        auto_start: bool,
        #[arg(long, env = "WECHAT_WINDOW_AUTOFIT", default_value_t = true)]
        window_autofit: bool,
        #[arg(long, env = "WECHAT_SHOW_TRAY", default_value_t = true)]
        show_tray: bool,
        #[arg(long, env = "WECHAT_HEALTH_PORT", default_value_t = 8090)]
        health_port: u16,
        #[arg(
            long,
            env = "WECHAT_STATE_DIR",
            default_value = "/var/lib/wechat-selkies"
        )]
        state_dir: PathBuf,
        #[arg(long, env = "WECHAT_NOTIFY_WEBHOOK_URL")]
        notify_webhook_url: Option<String>,
        #[arg(long, env = "WECHAT_NOTIFY_REDACT_BODY", default_value_t = true)]
        notify_redact_body: bool,
    },
    Healthcheck {
        #[arg(long, env = "WECHAT_HEALTH_PORT", default_value_t = 8090)]
        health_port: u16,
    },
    Status {
        #[arg(long, env = "WECHAT_HEALTH_PORT", default_value_t = 8090)]
        health_port: u16,
    },
    Restart {
        #[arg(long, env = "WECHAT_HEALTH_PORT", default_value_t = 8090)]
        health_port: u16,
    },
    FitWindow {
        #[arg(long, env = "WECHAT_HEALTH_PORT", default_value_t = 8090)]
        health_port: u16,
        #[arg(long, env = "WECHAT_SHOW_TRAY", default_value_t = true)]
        show_tray: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Bootstrap {
            manifest,
            cache_dir,
            state_dir,
            url_override,
            sha256_override,
            allow_unverified_latest,
        } => {
            let cfg = config::BootstrapConfig {
                manifest_path: manifest,
                cache_dir,
                state_dir,
                url_override: normalize_opt(url_override),
                sha256_override: normalize_opt(sha256_override),
                allow_unverified_latest,
            };
            bootstrap::run_bootstrap(&cfg)
        }
        Command::Run {
            wechat_bin,
            auto_start,
            window_autofit,
            show_tray,
            health_port,
            state_dir,
            notify_webhook_url,
            notify_redact_body,
        } => {
            let cfg = config::RunConfig {
                wechat_bin,
                auto_start,
                window_autofit,
                show_tray,
                health_port,
                state_dir,
                notify_webhook_url: normalize_opt(notify_webhook_url),
                notify_redact_body,
            };
            supervisor::run_supervisor(cfg)
        }
        Command::Healthcheck { health_port } => health::probe_health(health_port),
        Command::Status { health_port } => health::print_status(health_port),
        Command::Restart { health_port } => health::send_control(health_port, "restart"),
        Command::FitWindow {
            health_port,
            show_tray,
        } => {
            if health::send_control(health_port, "fit-window").is_ok() {
                Ok(())
            } else {
                x11_window::fit_once(show_tray)
            }
        }
    }
}

fn normalize_opt(val: Option<String>) -> Option<String> {
    val.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
