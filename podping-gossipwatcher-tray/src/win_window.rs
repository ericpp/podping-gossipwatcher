//! Tray-event background thread. Show/hide of the main window is dispatched
//! through `TrayAction` so the UI thread can use `ViewportCommand::Visible`
//! and winit can suspend paints while the window is hidden.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsWindow, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOWNORMAL,
};

#[derive(Clone, Default)]
pub struct WinWindow {
    hidden: Arc<AtomicBool>,
    hwnd: Arc<AtomicIsize>,
}

impl WinWindow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden.load(Ordering::SeqCst)
    }

    pub fn set_hidden(&self, v: bool) {
        self.hidden.store(v, Ordering::SeqCst);
    }

    /// Cache the HWND so background threads can call `ShowWindow` directly.
    /// Called from the UI thread once per frame; cheap after the first find.
    pub fn ensure_hwnd(&self, title: &str) -> HWND {
        let cur = self.hwnd.load(Ordering::SeqCst);
        if cur != 0 {
            let hwnd = cur as HWND;
            unsafe {
                if IsWindow(hwnd) != 0 {
                    return hwnd;
                }
            }
        }
        let hwnd = find_own_window(title);
        self.hwnd.store(hwnd as isize, Ordering::SeqCst);
        hwnd
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd.load(Ordering::SeqCst) as HWND
    }

    /// Directly restore + foreground the window via WinAPI. Safe to call from
    /// any thread. We can't rely on eframe/winit to do this while the viewport
    /// is Minimized(true) + Visible(false) — its event loop suspends
    /// `request_redraw` in that state, so `ViewportCommand::Visible(true)`
    /// queued from the tray thread would never be pumped.
    pub fn show_via_winapi(&self) -> bool {
        let hwnd = self.hwnd();
        if hwnd.is_null() {
            return false;
        }
        unsafe {
            ShowWindow(hwnd, SW_RESTORE);
            ShowWindow(hwnd, SW_SHOWNORMAL);
            SetForegroundWindow(hwnd);
        }
        self.set_hidden(false);
        true
    }

}

/// Find our top-level window by title. Our title is unique enough
/// ("Podping Gossip Watcher") that a naive FindWindowW is sufficient.
fn find_own_window(title: &str) -> HWND {
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { FindWindowW(std::ptr::null(), wide.as_ptr()) }
}

/// Background thread entry: forward tray menu/icon events to the UI thread via
/// `action_tx`. This thread never touches the window handle directly; the UI
/// thread does that with `ViewportCommand::Visible`.
pub fn spawn_tray_event_thread(
    show_id: muda::MenuId,
    dashboard_id: muda::MenuId,
    folder_id: muda::MenuId,
    restart_id: muda::MenuId,
    exit_id: muda::MenuId,
    win_window: WinWindow,
    action_tx: std::sync::mpsc::Sender<TrayAction>,
    data_folder: std::path::PathBuf,
    dashboard_url: Arc<Mutex<String>>,
    watcher: Arc<crate::watcher::Watcher>,
    exiting: Arc<AtomicBool>,
    ui_ctx: Arc<Mutex<Option<egui::Context>>>,
) {
    std::thread::spawn(move || {
        loop {
            while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
                if ev.id == show_id {
                    toggle_window(&win_window, &action_tx, &ui_ctx);
                } else if ev.id == dashboard_id {
                    let url = dashboard_url.lock().unwrap().clone();
                    if !url.is_empty() {
                        let _ = open::that(url);
                    }
                } else if ev.id == folder_id {
                    let _ = open::that(&data_folder);
                } else if ev.id == restart_id {
                    send(&action_tx, &ui_ctx, TrayAction::RestartWatcher);
                } else if ev.id == exit_id {
                    exiting.store(true, Ordering::SeqCst);
                    watcher.stop();
                    // The egui loop may not run while the window is hidden,
                    // so don't rely on ViewportCommand::Close — exit directly.
                    std::process::exit(0);
                }
            }

            while let Ok(ev) = tray_icon::TrayIconEvent::receiver().try_recv() {
                if matches!(
                    ev,
                    tray_icon::TrayIconEvent::Click {
                        button: tray_icon::MouseButton::Left,
                        button_state: tray_icon::MouseButtonState::Up,
                        ..
                    }
                ) {
                    show_window(&win_window, &action_tx, &ui_ctx);
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    });
}

fn toggle_window(
    win_window: &WinWindow,
    action_tx: &std::sync::mpsc::Sender<TrayAction>,
    ui_ctx: &Arc<Mutex<Option<egui::Context>>>,
) {
    if win_window.is_hidden() {
        show_window(win_window, action_tx, ui_ctx);
    } else {
        hide_window(win_window, action_tx, ui_ctx);
    }
}

fn show_window(
    win_window: &WinWindow,
    action_tx: &std::sync::mpsc::Sender<TrayAction>,
    ui_ctx: &Arc<Mutex<Option<egui::Context>>>,
) {
    // Bring the window back at the OS level first — winit's redraw loop is
    // parked while the viewport is Minimized(true), so a queued Show action
    // alone would never be processed.
    let awakened = win_window.show_via_winapi();
    send(action_tx, ui_ctx, TrayAction::Show);
    if awakened {
        if let Some(ctx) = ui_ctx.lock().unwrap().clone() {
            ctx.request_repaint();
        }
    }
}

fn hide_window(
    _win_window: &WinWindow,
    action_tx: &std::sync::mpsc::Sender<TrayAction>,
    ui_ctx: &Arc<Mutex<Option<egui::Context>>>,
) {
    send(action_tx, ui_ctx, TrayAction::Hide);
}

fn send(
    tx: &std::sync::mpsc::Sender<TrayAction>,
    ui_ctx: &Arc<Mutex<Option<egui::Context>>>,
    action: TrayAction,
) {
    let _ = tx.send(action);
    if let Some(ctx) = ui_ctx.lock().unwrap().clone() {
        // Wake winit so it processes the action even if the viewport is
        // currently invisible (winit stops paints for invisible viewports).
        ctx.request_repaint();
    }
}

#[derive(Debug)]
pub enum TrayAction {
    RestartWatcher,
    Show,
    Hide,
    #[allow(dead_code)]
    Toggle,
}
