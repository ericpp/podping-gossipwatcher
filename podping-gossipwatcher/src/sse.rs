use axum::{
    extract::{Query, State},
    response::{sse::{Event, KeepAlive, Sse}, Html, Json},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use crate::archive;

#[derive(Clone)]
pub struct AppState {
    pub sse_tx: broadcast::Sender<String>,
    pub notifications_received: Arc<AtomicU64>,
    pub broadcast_failures: Arc<AtomicU64>,
    pub last_notification_time: Arc<AtomicU64>,
    pub peer_names: Arc<RwLock<HashMap<String, String>>>,
    pub active_peers: Arc<RwLock<HashSet<String>>>,
    pub trusted_publishers: Arc<RwLock<HashSet<String>>>,
    pub node_id: String,
    pub pubkey: String,
    pub version: String,
    pub friendly_name: Option<String>,
    pub archive_db: Option<Arc<Mutex<archive::Archive>>>,
    pub rebootstrap_timeout: u64,
    pub start_time: u64,
}

#[derive(Debug, Deserialize, Default)]
pub struct EventFilter {
    pub medium: Option<String>,
    pub reason: Option<String>,
    pub sender: Option<String>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    uptime_seconds: u64,
    notifications_received: u64,
    broadcast_failures: u64,
    seconds_since_last_notification: u64,
    archive_messages: Option<u64>,
}

#[derive(Serialize)]
struct PeerEntry {
    id: String,
    name: Option<String>,
}

#[derive(Serialize)]
struct PeersResponse {
    peers: Vec<PeerEntry>,
    trusted_publishers_count: usize,
}

#[derive(Serialize)]
struct InfoResponse {
    node_id: String,
    pubkey: String,
    version: String,
    friendly_name: Option<String>,
}

pub fn start_web_server(addr: SocketAddr, state: AppState) {
    tokio::spawn(async move {
        let app = Router::new()
            .route("/", get(index_handler))
            .route("/events", get(sse_handler))
            .route("/api/health", get(health_handler))
            .route("/api/peers", get(peers_handler))
            .route("/api/info", get(info_handler))
            .with_state(state);

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "\x1b[1;31m[FATAL] Cannot bind web server on {}: {}\x1b[0m",
                    addr, e
                );
                return;
            }
        };
        println!("  Web UI + SSE server listening on {}", addr);
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("\x1b[1;31m[FATAL] Web server error: {}\x1b[0m", e);
        }
    });
}

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("web_ui.html"))
}

async fn sse_handler(
    Query(filter): Query<EventFilter>,
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(move |result| {
        match result {
            Ok(json_str) => {
                if matches_filter(&json_str, &filter) {
                    Some(Ok(Event::default().event("podping").data(json_str)))
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn health_handler(State(state): State<AppState>) -> Json<HealthResponse> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let last_notif = state.last_notification_time.load(Ordering::Relaxed);
    let since_last = now.saturating_sub(last_notif);
    let failures = state.broadcast_failures.load(Ordering::Relaxed);

    let status = if since_last > state.rebootstrap_timeout {
        "STALLED"
    } else if failures > 0 {
        "DEGRADED"
    } else {
        "OK"
    };

    let archive_messages = state.archive_db.as_ref().and_then(|db| {
        db.lock().ok().and_then(|d| d.message_count().ok())
    });

    Json(HealthResponse {
        status: status.to_string(),
        uptime_seconds: now.saturating_sub(state.start_time),
        notifications_received: state.notifications_received.load(Ordering::Relaxed),
        broadcast_failures: failures,
        seconds_since_last_notification: since_last,
        archive_messages,
    })
}

async fn peers_handler(State(state): State<AppState>) -> Json<PeersResponse> {
    let peers = {
        let names = state.peer_names.read().unwrap();
        let active = state.active_peers.read().unwrap();
        let mut seen = HashSet::new();
        let mut list: Vec<PeerEntry> = Vec::new();
        for id in active.iter() {
            seen.insert(id.clone());
            let name = names.get(id).filter(|n| !n.is_empty()).cloned();
            list.push(PeerEntry { id: id.clone(), name });
        }
        for (id, name) in names.iter() {
            if seen.contains(id) { continue; }
            list.push(PeerEntry {
                id: id.clone(),
                name: if name.is_empty() { None } else { Some(name.clone()) },
            });
        }
        list
    };
    let trusted_count = state.trusted_publishers.read().unwrap().len();

    Json(PeersResponse {
        peers,
        trusted_publishers_count: trusted_count,
    })
}

async fn info_handler(State(state): State<AppState>) -> Json<InfoResponse> {
    Json(InfoResponse {
        node_id: state.node_id.clone(),
        pubkey: state.pubkey.clone(),
        version: state.version.clone(),
        friendly_name: state.friendly_name.clone(),
    })
}

fn matches_filter(json_str: &str, filter: &EventFilter) -> bool {
    if filter.medium.is_none() && filter.reason.is_none() && filter.sender.is_none() {
        return true;
    }
    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return true,
    };
    if let Some(ref medium) = filter.medium {
        if parsed.get("medium").and_then(|v| v.as_str()) != Some(medium.as_str()) {
            return false;
        }
    }
    if let Some(ref reason) = filter.reason {
        if parsed.get("reason").and_then(|v| v.as_str()) != Some(reason.as_str()) {
            return false;
        }
    }
    if let Some(ref sender_prefix) = filter.sender {
        match parsed.get("sender").and_then(|v| v.as_str()) {
            Some(sender) if sender.starts_with(sender_prefix.as_str()) => {}
            _ => return false,
        }
    }
    true
}
