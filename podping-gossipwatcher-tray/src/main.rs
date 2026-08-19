#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod config;
#[cfg(windows)]
mod health;
#[cfg(windows)]
mod startup;
#[cfg(windows)]
mod watcher;
#[cfg(windows)]
mod win_window;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    app::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "podping-gossipwatcher-tray is a Windows-only wrapper. \
         On Linux/macOS run the podping-gossipwatcher binary directly."
    );
    std::process::exit(1);
}
