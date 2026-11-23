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
    listener.set_nonblocking(true)?;

    std::thread::Builder::new()
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

    Ok(())
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

pub fn probe_health(port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/healthz");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build();
    match agent.get(&url).call() {
        Ok(resp) if resp.status() == 200 => Ok(()),
        Ok(resp) => bail!("healthcheck returned HTTP {}", resp.status()),
        Err(err) => bail!("healthcheck request failed: {err}"),
    }
}

pub fn print_status(port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/healthz");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build();
    let body = match agent.get(&url).call() {
        Ok(resp) => resp.into_string()?,
        Err(ureq::Error::Status(_, resp)) => resp.into_string()?,
        Err(err) => bail!("status request failed: {err}"),
    };
    println!("{body}");
    Ok(())
}

pub fn send_control(port: u16, action: &str) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/control/{action}");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build();
    let resp = agent
        .post(&url)
        .call()
        .with_context(|| format!("control action '{action}' failed"))?;
    if resp.status() >= 300 {
        bail!("control action '{action}' returned HTTP {}", resp.status());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
