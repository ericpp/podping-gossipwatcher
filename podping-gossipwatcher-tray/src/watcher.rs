use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

/// CREATE_NO_WINDOW — hide the console when spawning a child from the tray app.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub type LogSender = Sender<String>;

/// A running (or previously running) watcher child process.
pub struct Watcher {
    child: Arc<Mutex<Option<Child>>>,
    exe: Arc<Mutex<PathBuf>>,
}

impl Watcher {
    /// Locate the watcher exe next to the tray exe.
    pub fn locate_exe() -> Result<PathBuf> {
        let tray = std::env::current_exe().context("resolving current exe")?;
        let dir = tray
            .parent()
            .ok_or_else(|| anyhow!("tray exe has no parent"))?;
        let candidate = dir.join("podping-gossipwatcher.exe");
        if !candidate.exists() {
            return Err(anyhow!(
                "podping-gossipwatcher.exe not found next to tray exe at {}",
                candidate.display()
            ));
        }
        Ok(candidate)
    }

    /// Use `custom` when non-empty; otherwise locate the exe beside the tray app.
    pub fn resolve_exe(custom: &str) -> Result<PathBuf> {
        let trimmed = custom.trim();
        if trimmed.is_empty() {
            return Self::locate_exe();
        }
        let p = PathBuf::from(trimmed);
        if !p.is_file() {
            return Err(anyhow!(
                "podping-gossipwatcher executable not found at {}",
                p.display()
            ));
        }
        Ok(p)
    }

    pub fn new(exe: PathBuf) -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            exe: Arc::new(Mutex::new(exe)),
        }
    }

    pub fn set_exe(&self, exe: PathBuf) {
        *self.exe.lock().unwrap() = exe;
    }

    /// Spawn the watcher with the given env vars. stdout+stderr lines are
    /// forwarded through `log_tx` as they arrive.
    pub fn start(&self, env: HashMap<String, String>, log_tx: LogSender) -> Result<()> {
        self.stop();

        let exe = self.exe.lock().unwrap().clone();
        let mut cmd = Command::new(&exe);
        // Inherit the tray's environment (APPDATA, SystemRoot, etc. — iroh
        // and Windows sockets need them) and overlay watcher settings on top.
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(not(windows))]
        {
            let _ = CREATE_NO_WINDOW;
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning {}", exe.display()))?;

        if let Some(stdout) = child.stdout.take() {
            let tx = log_tx.clone();
            thread::spawn(move || pipe_lines(stdout, tx));
        }
        if let Some(stderr) = child.stderr.take() {
            let tx = log_tx.clone();
            thread::spawn(move || pipe_lines(stderr, tx));
        }

        *self.child.lock().unwrap() = Some(child);
        Ok(())
    }

    /// Kill the child process (best-effort) and reap it.
    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// True if a child is currently running.
    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        let mut guard = self.child.lock().unwrap();
        if let Some(child) = guard.as_mut() {
            match child.try_wait() {
                Ok(None) => true,
                Ok(Some(_)) | Err(_) => {
                    *guard = None;
                    false
                }
            }
        } else {
            false
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}

fn pipe_lines<R: std::io::Read + Send + 'static>(reader: R, tx: LogSender) {
    let buf = BufReader::new(reader);
    for line in buf.lines() {
        match line {
            Ok(l) => {
                if tx.send(strip_ansi(&l)).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// Strip ANSI SGR escapes so the tray log view stays readable.
/// The watcher uses colored output; egui doesn't interpret ANSI.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for term in chars.by_ref() {
                let code = term as u32;
                if (0x40..=0x7e).contains(&code) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
