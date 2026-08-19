use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const DEFAULT_SSE_PORT: u16 = 8089;
const DEFAULT_PEER_ANNOUNCE_INTERVAL: u64 = 300;
const DEFAULT_PEER_ENDORSE_INTERVAL: u64 = 45;
const DEFAULT_SSE_BUFFER_SIZE: usize = 1000;

/// User-editable settings persisted in %APPDATA%\PodpingGossipWatcher\settings.toml.
///
/// File paths for keys, archive, and trusted lists are derived from the data dir
/// and not exposed here — the tray owns those locations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub node_friendly_name: String,
    #[serde(default)]
    pub archive_enabled: bool,
    #[serde(default)]
    pub catchup_enabled: bool,
    #[serde(default = "default_sse_enabled")]
    pub sse_enabled: bool,
    #[serde(default = "default_sse_port")]
    pub sse_port: u16,
    /// Empty = podping-gossipwatcher.exe next to the tray app.
    #[serde(default)]
    pub watcher_exe: String,
    #[serde(default)]
    pub bootstrap_peer_ids: String,
    #[serde(default = "default_announce")]
    pub peer_announce_interval: u64,
    #[serde(default = "default_endorse")]
    pub peer_endorse_interval: u64,
    #[serde(default = "default_buffer")]
    pub sse_buffer_size: usize,
    #[serde(default)]
    pub trace_to_file: bool,
    #[serde(default)]
    pub start_with_windows: bool,
}

fn default_sse_enabled() -> bool { true }
fn default_sse_port() -> u16 { DEFAULT_SSE_PORT }
fn default_announce() -> u64 { DEFAULT_PEER_ANNOUNCE_INTERVAL }
fn default_endorse() -> u64 { DEFAULT_PEER_ENDORSE_INTERVAL }
fn default_buffer() -> usize { DEFAULT_SSE_BUFFER_SIZE }

impl Default for Settings {
    fn default() -> Self {
        Self {
            node_friendly_name: String::new(),
            archive_enabled: false,
            catchup_enabled: false,
            sse_enabled: true,
            sse_port: DEFAULT_SSE_PORT,
            watcher_exe: String::new(),
            bootstrap_peer_ids: String::new(),
            peer_announce_interval: DEFAULT_PEER_ANNOUNCE_INTERVAL,
            peer_endorse_interval: DEFAULT_PEER_ENDORSE_INTERVAL,
            sse_buffer_size: DEFAULT_SSE_BUFFER_SIZE,
            trace_to_file: false,
            start_with_windows: false,
        }
    }
}

/// Resolved on-disk locations for the tray app.
#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub data: PathBuf,
    pub settings_file: PathBuf,
    pub node_key: PathBuf,
    pub known_peers: PathBuf,
    pub trusted_publishers: PathBuf,
    pub trusted_monitors: PathBuf,
    pub archive_db: PathBuf,
    pub trace_log: PathBuf,
}

impl Paths {
    /// Resolves %APPDATA%\PodpingGossipWatcher\ and creates the tree if missing.
    pub fn resolve() -> Result<Self> {
        let base = directories::BaseDirs::new()
            .context("could not locate user config directory")?;
        let root = base.config_dir().join("PodpingGossipWatcher");
        let data = root.join("data");
        std::fs::create_dir_all(&data)
            .with_context(|| format!("creating {}", data.display()))?;
        Ok(Self {
            settings_file: root.join("settings.toml"),
            node_key: data.join("node.key"),
            known_peers: data.join("known_peers.txt"),
            trusted_publishers: data.join("trusted_publishers.txt"),
            trusted_monitors: data.join("trusted_monitors.txt"),
            archive_db: data.join("listener_archive.db"),
            trace_log: data.join("trace.log"),
            root,
            data,
        })
    }

    /// Create empty trusted_* files on first run so users know where to edit them.
    pub fn ensure_trust_files(&self) -> Result<()> {
        for p in [&self.trusted_publishers, &self.trusted_monitors] {
            if !p.exists() {
                std::fs::write(p, b"# One ed25519 hex pubkey per line\n")
                    .with_context(|| format!("creating {}", p.display()))?;
            }
        }
        Ok(())
    }
}

impl Settings {
    pub fn load_or_default(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let s = toml::to_string_pretty(self).context("serializing settings.toml")?;
        std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Simple validation for the settings form. Returns human-readable errors.
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.sse_enabled && self.sse_port == 0 {
            errs.push("SSE port must be non-zero".into());
        }
        let watcher = self.watcher_exe.trim();
        if !watcher.is_empty() && !PathBuf::from(watcher).is_file() {
            errs.push(format!("Watcher executable not found: {}", watcher));
        }
        if self.node_friendly_name.len() > 64 {
            errs.push("Node name will be truncated to 64 characters".into());
        }
        errs
    }

    /// Build the environment variable map to pass to the watcher child.
    pub fn to_env(&self, paths: &Paths) -> HashMap<String, String> {
        let mut env = HashMap::new();

        env.insert(
            "SSE_ENABLED".into(),
            if self.sse_enabled { "true" } else { "false" }.into(),
        );
        if self.sse_enabled {
            env.insert(
                "SSE_BIND_ADDR".into(),
                format!("127.0.0.1:{}", self.sse_port),
            );
            env.insert("SSE_BUFFER_SIZE".into(), self.sse_buffer_size.to_string());
        }

        env.insert(
            "IROH_NODE_KEY_FILE".into(),
            paths.node_key.to_string_lossy().into_owned(),
        );
        env.insert(
            "KNOWN_PEERS_FILE".into(),
            paths.known_peers.to_string_lossy().into_owned(),
        );
        env.insert(
            "TRUSTED_PUBLISHERS_FILE".into(),
            paths.trusted_publishers.to_string_lossy().into_owned(),
        );
        env.insert(
            "TRUSTED_MONITORS_FILE".into(),
            paths.trusted_monitors.to_string_lossy().into_owned(),
        );
        env.insert(
            "ARCHIVE_PATH".into(),
            paths.archive_db.to_string_lossy().into_owned(),
        );

        env.insert(
            "ARCHIVE_ENABLED".into(),
            if self.archive_enabled { "true" } else { "false" }.into(),
        );
        env.insert(
            "CATCHUP_ENABLED".into(),
            if self.catchup_enabled { "true" } else { "false" }.into(),
        );

        env.insert(
            "PEER_ANNOUNCE_INTERVAL".into(),
            self.peer_announce_interval.to_string(),
        );
        env.insert(
            "PEER_ENDORSE_INTERVAL".into(),
            self.peer_endorse_interval.to_string(),
        );

        let name = self.node_friendly_name.trim();
        if !name.is_empty() {
            env.insert("NODE_FRIENDLY_NAME".into(), name.to_string());
        }

        let bootstrap = self.bootstrap_peer_ids.trim();
        if !bootstrap.is_empty() {
            env.insert("BOOTSTRAP_PEER_IDS".into(), bootstrap.to_string());
        }

        if self.trace_to_file {
            env.insert(
                "TRACE_FILE".into(),
                paths.trace_log.to_string_lossy().into_owned(),
            );
        }

        env
    }
}
