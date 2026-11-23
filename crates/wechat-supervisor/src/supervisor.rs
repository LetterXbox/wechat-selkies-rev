use anyhow::{Context, Result};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bootstrap::read_installed_version;
use crate::config::RunConfig;
use crate::health::{spawn_http_server, SharedState};
use crate::notify;
use crate::x11_window;

static SIGNAL_STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: libc::c_int) {
    SIGNAL_STOP.store(true, Ordering::SeqCst);
}

pub fn run_supervisor(cfg: RunConfig) -> Result<()> {
    install_signal_handlers();
    enable_child_subreaper();

    let version = read_installed_version(&cfg.state_dir);
    let state = Arc::new(SharedState::new(cfg.auto_start, version));

    spawn_http_server(cfg.health_port, Arc::clone(&state))?;

    let x11_handle = if cfg.window_autofit {
        Some(x11_window::spawn_watcher(
            Arc::clone(&state),
            cfg.show_tray,
        )?)
    } else {
        None
    };

    let notify_handle = if let Some(webhook_url) = cfg.notify_webhook_url.clone() {
        Some(notify::spawn_bridge(
            webhook_url,
            cfg.notify_redact_body,
            Arc::clone(&state),
        )?)
    } else {
        None
    };

    let mut active_child: Option<Child> = None;
    let mut active_pgid: Option<libc::pid_t> = None;
    let mut last_launch = Instant::now() - Duration::from_secs(60);
    let mut consecutive_failures: u32 = 0;

    while !SIGNAL_STOP.load(Ordering::SeqCst) {
        reap_stray_children();

        if state.restart_requested.swap(false, Ordering::SeqCst) {
            eprintln!("[wechat-supervisor] Restart requested");
            stop_managed_process(&mut active_child, &mut active_pgid, &state);
            consecutive_failures = 0;
        }

        if let Some(child) = active_child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    eprintln!("[wechat-supervisor] WeChat exited with {status}");
                    if let Some(pgid) = active_pgid.take() {
                        terminate_process_group(pgid, Duration::from_secs(3));
                    }
                    active_child = None;
                    state.set_wechat_pid(None);
                    if last_launch.elapsed() > Duration::from_secs(30) {
                        consecutive_failures = 0;
                    } else {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    eprintln!("[wechat-supervisor] wait error: {err}");
                }
            }
        }

        if active_child.is_none() && cfg.auto_start {
            let delay = backoff_delay(consecutive_failures);
            if delay > Duration::ZERO && last_launch.elapsed() < delay {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            if cfg.wechat_bin.exists() {
                match spawn_wechat(&cfg.wechat_bin) {
                    Ok((child, pgid)) => {
                        let pid = child.id();
                        eprintln!("[wechat-supervisor] Started WeChat (pid={pid}, pgid={pgid})");
                        state.set_wechat_pid(Some(pid));
                        state.fit_requested.store(true, Ordering::SeqCst);
                        active_child = Some(child);
                        active_pgid = Some(pgid);
                        last_launch = Instant::now();
                    }
                    Err(err) => {
                        eprintln!("[wechat-supervisor] Failed to spawn WeChat: {err}");
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        last_launch = Instant::now();
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(200));
    }

    eprintln!("[wechat-supervisor] Shutdown signal received");
    state.shutdown.store(true, Ordering::SeqCst);
    stop_managed_process(&mut active_child, &mut active_pgid, &state);
    reap_stray_children();

    if let Some(handle) = x11_handle {
        let _ = handle.join();
    }
    if let Some(handle) = notify_handle {
        let _ = handle.join();
    }
    Ok(())
}

fn spawn_wechat(bin: &Path) -> Result<(Child, libc::pid_t)> {
    let mut cmd = Command::new(bin);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    // Place WeChat and its WeChatAppEx descendants in a dedicated session/process group.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {}", bin.display()))?;
    let pgid = child.id() as libc::pid_t;
    Ok((child, pgid))
}

fn stop_managed_process(
    active_child: &mut Option<Child>,
    active_pgid: &mut Option<libc::pid_t>,
    state: &SharedState,
) {
    if let Some(pgid) = active_pgid.take() {
        terminate_process_group(pgid, Duration::from_secs(5));
    }
    if let Some(mut child) = active_child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    reap_stray_children();
    state.set_wechat_pid(None);
}

fn terminate_process_group(pgid: libc::pid_t, grace: Duration) {
    if pgid <= 0 {
        return;
    }
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        reap_stray_children();
        let alive = unsafe { libc::kill(-pgid, 0) == 0 };
        if !alive {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    reap_stray_children();
}

fn reap_stray_children() {
    loop {
        let mut status: libc::c_int = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
    }
}

fn enable_child_subreaper() {
    // PR_SET_CHILD_SUBREAPER ensures orphaned WeChatAppEx helpers reparent to this supervisor.
    unsafe {
        let _ = libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
    }
}

pub fn backoff_delay(consecutive_failures: u32) -> Duration {
    if consecutive_failures == 0 {
        return Duration::ZERO;
    }
    let shift = (consecutive_failures - 1).min(5);
    Duration::from_secs(1u64 << shift).min(Duration::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_backoff_caps_at_30s() {
        assert_eq!(backoff_delay(0), Duration::ZERO);
        assert_eq!(backoff_delay(1), Duration::from_secs(1));
        assert_eq!(backoff_delay(2), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(4));
        assert_eq!(backoff_delay(6), Duration::from_secs(30));
        assert_eq!(backoff_delay(20), Duration::from_secs(30));
    }
}
