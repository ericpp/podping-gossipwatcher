use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Unknown,
    Ok,
    Degraded,
    Stalled,
}

#[derive(Debug, Clone)]
pub struct HealthSnapshot {
    pub status: Status,
    pub uptime_seconds: u64,
    pub notifications_received: u64,
    pub seconds_since_last_notification: u64,
    pub reachable: bool,
}

impl Default for HealthSnapshot {
    fn default() -> Self {
        Self {
            status: Status::Unknown,
            uptime_seconds: 0,
            notifications_received: 0,
            seconds_since_last_notification: 0,
            reachable: false,
        }
    }
}

#[derive(Deserialize)]
struct RawHealth {
    #[serde(default)]
    status: String,
    #[serde(default)]
    uptime_seconds: u64,
    #[serde(default)]
    notifications_received: u64,
    #[serde(default)]
    seconds_since_last_notification: u64,
}

/// Shared latest snapshot updated by the background polling thread.
#[derive(Clone, Default)]
pub struct HealthState {
    inner: Arc<Mutex<HealthSnapshot>>,
}

impl HealthState {
    pub fn get(&self) -> HealthSnapshot {
        self.inner.lock().unwrap().clone()
    }

    fn set(&self, snap: HealthSnapshot) {
        *self.inner.lock().unwrap() = snap;
    }
}

/// Spawn a background thread that polls `/api/health` every ~3s and updates
/// `state`. The port can change between polls (settings restart) — we read it
/// fresh from `port_fn` each iteration. `None` means SSE is disabled.
///
/// Uses a raw localhost TCP GET so Windows system/HTTP proxies cannot intercept
/// the loopback health check.
pub fn spawn_poller<F>(state: HealthState, port_fn: F, log_tx: Sender<String>)
where
    F: Fn() -> Option<u16> + Send + 'static,
{
    thread::spawn(move || {
        let mut logged_failure = false;
        let mut consecutive_failures: u32 = 0;

        loop {
            let snap = match port_fn() {
                None => HealthSnapshot::default(),
                Some(port) => match fetch_health(port) {
                    Ok(snap) => {
                        if logged_failure {
                            let _ = log_tx.send("[TRAY] watcher health endpoint is up".into());
                            logged_failure = false;
                        }
                        consecutive_failures = 0;
                        snap
                    }
                    Err(e) => {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        // Skip the first couple of failures — the web server starts
                        // during iroh bind and may not be listening yet.
                        if consecutive_failures == 3 && !logged_failure {
                            let _ = log_tx.send(format!(
                                "[TRAY] health poll failed (http://127.0.0.1:{}/api/health): {}",
                                port, e
                            ));
                            logged_failure = true;
                        }
                        HealthSnapshot::default()
                    }
                },
            };
            state.set(snap);
            thread::sleep(Duration::from_secs(3));
        }
    });
}

fn fetch_health(port: u16) -> Result<HealthSnapshot, String> {
    let body = http_get_json(port, "/api/health")?;
    let r: RawHealth = serde_json::from_str(&body).map_err(|e| {
        format!("decode health JSON: {e} (body starts {:?})", body.chars().take(80).collect::<String>())
    })?;
    Ok(HealthSnapshot {
        status: parse_status(&r.status),
        uptime_seconds: r.uptime_seconds,
        notifications_received: r.notifications_received,
        seconds_since_last_notification: r.seconds_since_last_notification,
        reachable: true,
    })
}

fn http_get_json(port: u16, path: &str) -> Result<String, String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|e| format!("connect {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let _ = stream.set_nodelay(true);

    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| format!("write: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| format!("read status: {e}"))?;
    let code = status_line.split_whitespace().nth(1).unwrap_or("");
    if code != "200" {
        return Err(format!("HTTP {code}"));
    }

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| format!("read header: {e}"))?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().ok();
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }

    if chunked {
        read_chunked(&mut reader)
    } else if let Some(len) = content_length {
        let mut buf = vec![0u8; len];
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("read body: {e}"))?;
        String::from_utf8(buf).map_err(|e| format!("utf-8: {e}"))
    } else {
        let mut buf = String::new();
        reader
            .read_to_string(&mut buf)
            .map_err(|e| format!("read body: {e}"))?;
        Ok(buf)
    }
}

fn read_chunked<R: BufRead>(reader: &mut R) -> Result<String, String> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        reader
            .read_line(&mut size_line)
            .map_err(|e| format!("chunk size: {e}"))?;
        let hex = size_line
            .trim()
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size = usize::from_str_radix(hex, 16).map_err(|e| format!("chunk size '{hex}': {e}"))?;
        if size == 0 {
            let mut trailer = String::new();
            let _ = reader.read_line(&mut trailer);
            break;
        }
        let mut chunk = vec![0u8; size];
        reader
            .read_exact(&mut chunk)
            .map_err(|e| format!("chunk data: {e}"))?;
        body.extend_from_slice(&chunk);
        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .map_err(|e| format!("chunk crlf: {e}"))?;
    }
    String::from_utf8(body).map_err(|e| format!("utf-8: {e}"))
}

fn parse_status(s: &str) -> Status {
    match s {
        "OK" => Status::Ok,
        "DEGRADED" => Status::Degraded,
        "STALLED" => Status::Stalled,
        _ => Status::Unknown,
    }
}
