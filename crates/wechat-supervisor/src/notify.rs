use anyhow::Result;
use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::health::SharedState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotificationPayload {
    pub app: String,
    pub summary: String,
    pub body: Option<String>,
}

pub fn spawn_bridge(
    webhook_url: String,
    redact_body: bool,
    state: Arc<SharedState>,
) -> Result<JoinHandle<()>> {
    let handle = std::thread::Builder::new()
        .name("dbus-notify-bridge".to_string())
        .spawn(move || bridge_loop(&webhook_url, redact_body, state))?;
    Ok(handle)
}

fn bridge_loop(webhook_url: &str, redact_body: bool, state: Arc<SharedState>) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();

    while !state.shutdown.load(Ordering::Relaxed) {
        let mut child = match Command::new("dbus-monitor")
            .args([
                "--session",
                "type='method_call',interface='org.freedesktop.Notifications',member='Notify'",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                eprintln!("[wechat-notify] Failed to start dbus-monitor: {err}");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };

        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            let mut parser = NotifyParser::new(redact_body);
            for line in reader.lines() {
                if state.shutdown.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(line) = line else {
                    break;
                };
                if let Some(payload) = parser.feed_line(&line) {
                    if let Ok(json) = serde_json::to_string(&payload) {
                        if let Err(err) = agent
                            .post(webhook_url)
                            .set("Content-Type", "application/json")
                            .send_string(&json)
                        {
                            eprintln!("[wechat-notify] Webhook post failed: {err}");
                        }
                    }
                }
            }
        }

        let _ = child.kill();
        let _ = child.wait();
        if !state.shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

pub struct NotifyParser {
    redact_body: bool,
    in_notify: bool,
    strings: Vec<String>,
}

impl NotifyParser {
    pub fn new(redact_body: bool) -> Self {
        Self {
            redact_body,
            in_notify: false,
            strings: Vec::new(),
        }
    }

    pub fn feed_line(&mut self, line: &str) -> Option<NotificationPayload> {
        let trimmed = line.trim();
        if trimmed.starts_with("method call ") && trimmed.contains("member=Notify") {
            self.in_notify = true;
            self.strings.clear();
            return None;
        }
        if !self.in_notify {
            return None;
        }
        if let Some(val) = parse_dbus_string(trimmed) {
            self.strings.push(val);
            // org.freedesktop.Notifications.Notify string arguments arrive in order:
            // [0]=app_name, [1]=app_icon, [2]=summary, [3]=body.
            if self.strings.len() == 4 {
                self.in_notify = false;
                let app = self.strings[0].clone();
                let summary = self.strings[2].clone();
                let body = if self.redact_body {
                    None
                } else {
                    Some(self.strings[3].clone())
                };
                self.strings.clear();
                if app.to_ascii_lowercase().contains("wechat") || app.contains("微信") {
                    return Some(NotificationPayload { app, summary, body });
                }
            }
        }
        None
    }
}

fn parse_dbus_string(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("string \"")?;
    let val = rest.strip_suffix('"')?;
    Some(val.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wechat_notify_and_redacts_body_by_default() {
        let mut parser = NotifyParser::new(true);
        let lines = [
            "method call time=1.0 sender=:1.42 -> destination=:1.2 serial=7 path=/org/freedesktop/Notifications; interface=org.freedesktop.Notifications; member=Notify",
            "   string \"WeChat\"",
            "   uint32 0",
            "   string \"wechat\"",
            "   string \"New message\"",
            "   string \"Secret content\"",
        ];
        let mut out = None;
        for line in lines {
            if let Some(payload) = parser.feed_line(line) {
                out = Some(payload);
            }
        }
        assert_eq!(
            out,
            Some(NotificationPayload {
                app: "WeChat".to_string(),
                summary: "New message".to_string(),
                body: None,
            })
        );
    }

    #[test]
    fn includes_body_when_redaction_disabled() {
        let mut parser = NotifyParser::new(false);
        let lines = [
            "method call time=1.0 member=Notify",
            "   string \"微信\"",
            "   uint32 0",
            "   string \"wechat\"",
            "   string \"Alice\"",
            "   string \"Hello\"",
        ];
        let mut out = None;
        for line in lines {
            if let Some(payload) = parser.feed_line(line) {
                out = Some(payload);
            }
        }
        assert_eq!(
            out,
            Some(NotificationPayload {
                app: "微信".to_string(),
                summary: "Alice".to_string(),
                body: Some("Hello".to_string()),
            })
        );
    }
}
