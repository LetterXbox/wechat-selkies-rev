use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub status: String,
    pub wechat_running: bool,
    pub wechat_pid: Option<u32>,
    pub wechat_version: Option<String>,
    pub display_connected: bool,
    pub viewport: Option<(u16, u16)>,
    pub auto_start: bool,
    pub uptime_secs: u64,
}

#[derive(Debug)]
pub struct SharedState {
    started_at: Instant,
    inner: Mutex<InnerState>,
    pub restart_requested: AtomicBool,
    pub fit_requested: AtomicBool,
    pub shutdown: AtomicBool,
}

#[derive(Debug)]
struct InnerState {
    wechat_pid: Option<u32>,
    wechat_version: Option<String>,
    display_connected: bool,
    viewport: Option<(u16, u16)>,
    auto_start: bool,
}

impl SharedState {
    pub fn new(auto_start: bool, wechat_version: Option<String>) -> Self {
        Self {
            started_at: Instant::now(),
            inner: Mutex::new(InnerState {
                wechat_pid: None,
                wechat_version,
                display_connected: false,
                viewport: None,
                auto_start,
            }),
            restart_requested: AtomicBool::new(false),
            fit_requested: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        }
    }

    pub fn set_wechat_pid(&self, pid: Option<u32>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.wechat_pid = pid;
        }
    }

    pub fn set_display_state(&self, connected: bool, viewport: Option<(u16, u16)>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.display_connected = connected;
            if viewport.is_some() {
                guard.viewport = viewport;
            }
        }
    }

    pub fn snapshot(&self) -> HealthSnapshot {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let wechat_running = guard.wechat_pid.is_some();
        let healthy = !guard.auto_start || wechat_running;
        HealthSnapshot {
            status: if healthy {
                "ok".to_string()
            } else {
                "degraded".to_string()
            },
            wechat_running,
            wechat_pid: guard.wechat_pid,
            wechat_version: guard.wechat_version.clone(),
            display_connected: guard.display_connected,
            viewport: guard.viewport,
            auto_start: guard.auto_start,
            uptime_secs: self.started_at.elapsed().as_secs(),
        }
    }
}

pub fn spawn_http_server(port: u16, state: Arc<SharedState>) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("failed to bind health/control server on 127.0.0.1:{port}"))?;
    let _ = spawn_http_server_with_listener(listener, state)?;
    Ok(())
}

fn spawn_http_server_with_listener(
    listener: TcpListener,
    state: Arc<SharedState>,
) -> Result<std::thread::JoinHandle<()>> {
    listener.set_nonblocking(true)?;

    let handle = std::thread::Builder::new()
        .name("health-http".to_string())
        .spawn(move || {
            while !state.shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = handle_connection(stream, &state);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        })?;

    Ok(handle)
}

fn handle_connection(mut stream: TcpStream, state: &SharedState) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");

    let (status_line, body) = route_request(method, path, state);
    write_http_response(&mut stream, status_line, &body)
}

fn route_request(method: &str, path: &str, state: &SharedState) -> (&'static str, String) {
    match (method, path) {
        ("GET", "/healthz") | ("GET", "/readyz") => {
            let snap = state.snapshot();
            let code = if snap.status == "ok" {
                "200 OK"
            } else {
                "503 Service Unavailable"
            };
            let body = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_string());
            (code, body)
        }
        ("POST", "/control/restart") => {
            state.restart_requested.store(true, Ordering::SeqCst);
            ("202 Accepted", r#"{"accepted":"restart"}"#.to_string())
        }
        ("POST", "/control/fit-window") => {
            state.fit_requested.store(true, Ordering::SeqCst);
            ("202 Accepted", r#"{"accepted":"fit-window"}"#.to_string())
        }
        _ => ("404 Not Found", r#"{"error":"not_found"}"#.to_string()),
    }
}

fn write_http_response(stream: &mut TcpStream, status_line: &str, body: &str) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn loopback_agent(http_status_as_error: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .proxy(None)
        .http_status_as_error(http_status_as_error)
        .build()
        .into()
}

pub fn probe_health(port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/healthz");
    let agent = loopback_agent(true);
    match agent.get(&url).call() {
        Ok(resp) if resp.status() == 200 => Ok(()),
        Ok(resp) => bail!("healthcheck returned HTTP {}", resp.status().as_u16()),
        Err(err) => bail!("healthcheck request failed: {err}"),
    }
}

fn fetch_status(port: u16) -> Result<String> {
    let url = format!("http://127.0.0.1:{port}/healthz");
    let agent = loopback_agent(false);
    let mut resp = agent
        .get(&url)
        .call()
        .with_context(|| "status request failed")?;
    Ok(resp.body_mut().read_to_string()?)
}

pub fn print_status(port: u16) -> Result<()> {
    let body = fetch_status(port)?;
    println!("{body}");
    Ok(())
}

pub fn send_control(port: u16, action: &str) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/control/{action}");
    let agent = loopback_agent(true);
    let resp = agent
        .post(&url)
        .send_empty()
        .with_context(|| format!("control action '{action}' failed"))?;
    if resp.status().as_u16() >= 300 {
        bail!(
            "control action '{action}' returned HTTP {}",
            resp.status().as_u16()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const CHILD_PORT_ENV: &str = "WECHAT_SUPERVISOR_TEST_HEALTH_PORT";
    const CHILD_PHASE_ENV: &str = "WECHAT_SUPERVISOR_TEST_HEALTH_PHASE";

    struct ServerGuard {
        state: Arc<SharedState>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for ServerGuard {
        fn drop(&mut self) {
            self.state.shutdown.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn run_proxy_probe_child(port: u16, phase: &str) {
        let exe = std::env::current_exe().unwrap();
        let output = Command::new(exe)
            .args([
                "--exact",
                "health::tests::loopback_requests_ignore_http_proxy_env",
                "--nocapture",
            ])
            .env_clear()
            .env(CHILD_PORT_ENV, port.to_string())
            .env(CHILD_PHASE_ENV, phase)
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("http_proxy", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("https_proxy", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .env("all_proxy", "http://127.0.0.1:1")
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child phase '{phase}' failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn health_is_degraded_until_wechat_running_when_autostart_enabled() {
        let state = SharedState::new(true, Some("4.1.13.29".to_string()));
        let (code, body) = route_request("GET", "/healthz", &state);
        assert_eq!(code, "503 Service Unavailable");
        let snap: HealthSnapshot = serde_json::from_str(&body).unwrap();
        assert_eq!(snap.status, "degraded");
        assert!(!snap.wechat_running);

        state.set_wechat_pid(Some(4242));
        let (code, body) = route_request("GET", "/healthz", &state);
        assert_eq!(code, "200 OK");
        let snap: HealthSnapshot = serde_json::from_str(&body).unwrap();
        assert_eq!(snap.status, "ok");
        assert_eq!(snap.wechat_pid, Some(4242));
    }

    #[test]
    fn health_is_ok_without_wechat_when_autostart_disabled() {
        let state = SharedState::new(false, None);
        let (code, _) = route_request("GET", "/healthz", &state);
        assert_eq!(code, "200 OK");
    }

    #[test]
    fn control_routes_set_atomic_flags() {
        let state = SharedState::new(true, None);
        let (code, _) = route_request("POST", "/control/restart", &state);
        assert_eq!(code, "202 Accepted");
        assert!(state.restart_requested.load(Ordering::SeqCst));

        let (code, _) = route_request("POST", "/control/fit-window", &state);
        assert_eq!(code, "202 Accepted");
        assert!(state.fit_requested.load(Ordering::SeqCst));
    }

    #[test]
    fn loopback_requests_ignore_http_proxy_env() {
        if let (Ok(port_str), Ok(phase)) = (
            std::env::var(CHILD_PORT_ENV),
            std::env::var(CHILD_PHASE_ENV),
        ) {
            let port: u16 = port_str.parse().unwrap();
            match phase.as_str() {
                "degraded" => {
                    assert!(probe_health(port).is_err());
                    let degraded: HealthSnapshot =
                        serde_json::from_str(&fetch_status(port).unwrap()).unwrap();
                    assert_eq!(degraded.status, "degraded");
                }
                "healthy" => {
                    probe_health(port).unwrap();
                    let healthy: HealthSnapshot =
                        serde_json::from_str(&fetch_status(port).unwrap()).unwrap();
                    assert_eq!(healthy.status, "ok");
                    send_control(port, "restart").unwrap();
                }
                other => panic!("unexpected test phase: {other}"),
            }
            return;
        }

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(SharedState::new(true, Some("4.1.13.29".to_string())));
        let handle = spawn_http_server_with_listener(listener, Arc::clone(&state)).unwrap();
        let _server = ServerGuard {
            state: Arc::clone(&state),
            handle: Some(handle),
        };

        run_proxy_probe_child(port, "degraded");

        state.set_wechat_pid(Some(4242));
        run_proxy_probe_child(port, "healthy");
        assert!(state.restart_requested.load(Ordering::SeqCst));
    }
}
