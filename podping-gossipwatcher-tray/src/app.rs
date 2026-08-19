use crate::config::{Paths, Settings};
use crate::health::{spawn_poller, HealthSnapshot, HealthState, Status};
use crate::startup;
use crate::watcher::Watcher;
use crate::win_window::{spawn_tray_event_thread, TrayAction, WinWindow};

use anyhow::{Context, Result};
use eframe::egui;
use muda::{Menu, MenuItem, PredefinedMenuItem};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tray_icon::{TrayIcon, TrayIconBuilder};

const MAX_LOG_LINES: usize = 5000;
const STALL_SECS: u64 = 180;

/// Stats derived from the watcher child (start time + log lines). Used when
/// SSE is off or `/api/health` is not reachable yet.
struct LocalStats {
    started_at: Instant,
    ready: bool,
    notifications: u64,
    last_notification_at: Option<Instant>,
}

impl LocalStats {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            ready: false,
            notifications: 0,
            last_notification_at: None,
        }
    }

    fn observe(&mut self, line: &str) {
        if line.contains("Subscribed to gossip topic") {
            self.ready = true;
        }
        if line.contains("PODPING:") {
            self.notifications += 1;
            self.last_notification_at = Some(Instant::now());
            self.ready = true;
        }
    }
}

struct DisplaySnap {
    status: Status,
    starting: bool,
    uptime_seconds: u64,
    notifications: u64,
    last: Option<u64>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Log,
    Settings,
}

pub fn run() -> Result<()> {
    let paths = Paths::resolve().context("resolving data paths")?;
    paths.ensure_trust_files()?;

    let settings = Settings::load_or_default(&paths.settings_file);
    if !paths.settings_file.exists() {
        // Persist defaults so users can see/edit the file directly if they want.
        let _ = settings.save(&paths.settings_file);
    }
    if let Err(e) = startup::apply(settings.start_with_windows) {
        eprintln!("[TRAY] startup registration: {}", e);
    }

    let watcher_exe = Watcher::resolve_exe(&settings.watcher_exe)?;
    let watcher = Arc::new(Watcher::new(watcher_exe));

    let (log_tx, log_rx) = channel::<String>();
    watcher
        .start(settings.to_env(&paths), log_tx.clone())
        .context("starting watcher child")?;

    let (action_tx, action_rx) = channel::<TrayAction>();
    let win_window = WinWindow::new();
    let dashboard_url = Arc::new(Mutex::new(dashboard_url_for(&settings)));

    let health = HealthState::default();
    let port_arc = Arc::new(Mutex::new(settings.sse_port));
    let sse_enabled_arc = Arc::new(Mutex::new(settings.sse_enabled));
    {
        let port_arc = port_arc.clone();
        let sse_enabled_arc = sse_enabled_arc.clone();
        spawn_poller(
            health.clone(),
            move || {
                if !*sse_enabled_arc.lock().unwrap() {
                    return None;
                }
                Some(*port_arc.lock().unwrap())
            },
            log_tx.clone(),
        );
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 520.0])
            .with_min_inner_size([560.0, 380.0])
            .with_title("Podping Gossip Watcher"),
        ..Default::default()
    };

    let exiting = Arc::new(AtomicBool::new(false));
    let ui_ctx = Arc::new(Mutex::new(None::<egui::Context>));
    let start_minimized = startup::launched_minimized();

    let app_state = AppState {
        paths,
        settings_draft: settings.clone(),
        settings_saved: settings,
        watcher: watcher.clone(),
        log_tx,
        log_rx,
        log_lines: Vec::with_capacity(1024),
        health,
        port_arc,
        sse_enabled_arc,
        tab: Tab::Log,
        auto_scroll: true,
        validation_errors: Vec::new(),
        save_status: None,
        tray: None,
        exiting,
        ui_ctx,
        win_window,
        action_rx,
        dashboard_url,
        minimize_pending: start_minimized,
        last_tray_status: None,
        local_stats: LocalStats::new(),
    };

    eframe::run_native(
        "Podping Gossip Watcher",
        native_options,
        Box::new(move |cc| {
            // Build tray inside the eframe main-thread callback so the winit
            // message pump is available for menu/icon events on Windows.
            let mut app = app_state;
            match build_tray(
                &mut app,
                action_tx,
            ) {
                Ok(tray) => app.tray = Some(tray),
                Err(e) => app.log_lines.push(format!("[TRAY] failed to init: {}", e)),
            }
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {}", e))?;

    Ok(())
}

struct TrayHandles {
    icon: TrayIcon,
}

fn build_tray(app: &mut AppState, action_tx: Sender<TrayAction>) -> Result<TrayHandles> {
    let menu = Menu::new();
    let show = MenuItem::new("Show / Hide Window", true, None);
    let dashboard = MenuItem::new("Open Dashboard", true, None);
    let folder = MenuItem::new("Open Data Folder", true, None);
    let restart = MenuItem::new("Restart Watcher", true, None);
    let separator = PredefinedMenuItem::separator();
    let exit = MenuItem::new("Exit", true, None);

    menu.append(&show)?;
    menu.append(&dashboard)?;
    menu.append(&folder)?;
    menu.append(&restart)?;
    menu.append(&separator)?;
    menu.append(&exit)?;

    let icon = build_icon_rgba(Status::Unknown);

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Podping Gossip Watcher")
        .with_icon(icon)
        .build()?;

    spawn_tray_event_thread(
        show.id().clone(),
        dashboard.id().clone(),
        folder.id().clone(),
        restart.id().clone(),
        exit.id().clone(),
        app.win_window.clone(),
        action_tx,
        app.paths.root.clone(),
        app.dashboard_url.clone(),
        app.watcher.clone(),
        app.exiting.clone(),
        app.ui_ctx.clone(),
    );

    Ok(TrayHandles { icon: tray })
}

/// 32×32 ping mark: center node + two concentric rings, tinted by status.
/// A dark halo keeps it readable on both light and dark taskbars.
fn build_icon_rgba(status: Status) -> tray_icon::Icon {
    const SIZE: u32 = 32;
    let (r, g, b) = match status {
        Status::Ok => (63, 185, 80),
        Status::Degraded => (210, 153, 34),
        Status::Stalled => (248, 81, 73),
        Status::Unknown => (139, 148, 158),
    };
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let cx = SIZE as f32 / 2.0;
    let cy = SIZE as f32 / 2.0;
    paint_ping(&mut rgba, SIZE, cx, cy, 22, 27, 34, 1.4);
    paint_ping(&mut rgba, SIZE, cx, cy, r, g, b, 0.0);
    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon bytes")
}

fn paint_ping(buf: &mut [u8], size: u32, cx: f32, cy: f32, r: u8, g: u8, b: u8, inflate: f32) {
    let node_r = 4.2 + inflate;
    let ring_w = 1.35 + inflate * 0.45;
    let rings = [8.6 + inflate * 0.25, 13.3 + inflate * 0.25];
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dist = (px - cx).hypot(py - cy);
            let mut cov = circle_cov(dist, node_r);
            for ring_r in rings {
                cov = (cov + ring_cov(dist, ring_r, ring_w)).min(1.0);
            }
            if cov > 0.0 {
                blend_over(buf, ((y * size + x) * 4) as usize, r, g, b, cov);
            }
        }
    }
}

fn circle_cov(dist: f32, radius: f32) -> f32 {
    (radius - dist + 0.5).clamp(0.0, 1.0)
}

fn ring_cov(dist: f32, radius: f32, half_width: f32) -> f32 {
    (circle_cov(dist, radius + half_width) - circle_cov(dist, radius - half_width)).clamp(0.0, 1.0)
}

fn blend_over(buf: &mut [u8], i: usize, r: u8, g: u8, b: u8, a: f32) {
    let sa = a.clamp(0.0, 1.0);
    let da = buf[i + 3] as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a < 1e-6 {
        return;
    }
    let inv = 1.0 - sa;
    buf[i] = (((r as f32 / 255.0) * sa + (buf[i] as f32 / 255.0) * da * inv) / out_a * 255.0) as u8;
    buf[i + 1] =
        (((g as f32 / 255.0) * sa + (buf[i + 1] as f32 / 255.0) * da * inv) / out_a * 255.0) as u8;
    buf[i + 2] =
        (((b as f32 / 255.0) * sa + (buf[i + 2] as f32 / 255.0) * da * inv) / out_a * 255.0) as u8;
    buf[i + 3] = (out_a * 255.0) as u8;
}

struct AppState {
    paths: Paths,
    settings_draft: Settings,
    settings_saved: Settings,
    watcher: Arc<Watcher>,
    log_tx: std::sync::mpsc::Sender<String>,
    log_rx: Receiver<String>,
    log_lines: Vec<String>,
    health: HealthState,
    port_arc: Arc<Mutex<u16>>,
    sse_enabled_arc: Arc<Mutex<bool>>,
    tab: Tab,
    auto_scroll: bool,
    validation_errors: Vec<String>,
    save_status: Option<String>,
    tray: Option<TrayHandles>,
    exiting: Arc<AtomicBool>,
    ui_ctx: Arc<Mutex<Option<egui::Context>>>,
    win_window: WinWindow,
    action_rx: Receiver<TrayAction>,
    dashboard_url: Arc<Mutex<String>>,
    minimize_pending: bool,
    last_tray_status: Option<Status>,
    local_stats: LocalStats,
}

impl AppState {
    fn drain_logs(&mut self) {
        while let Ok(line) = self.log_rx.try_recv() {
            self.local_stats.observe(&line);
            self.log_lines.push(line);
        }
        if self.log_lines.len() > MAX_LOG_LINES {
            let drop = self.log_lines.len() - MAX_LOG_LINES;
            self.log_lines.drain(0..drop);
        }
    }

    fn process_actions(&mut self, ctx: &egui::Context) {
        while let Ok(action) = self.action_rx.try_recv() {
            match action {
                TrayAction::RestartWatcher => self.restart_watcher(),
                TrayAction::Show => self.show_window(ctx),
                TrayAction::Hide => self.hide_window(ctx),
                TrayAction::Toggle => {
                    if self.win_window.is_hidden() {
                        self.show_window(ctx);
                    } else {
                        self.hide_window(ctx);
                    }
                }
            }
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        // Prefer the WinAPI path — winit's redraw loop is parked while the
        // viewport is Minimized(true), so `ViewportCommand::Visible(true)`
        // alone doesn't actually pump. Calling ShowWindow/SetForegroundWindow
        // directly triggers WM_SHOWWINDOW / WM_ACTIVATE which winit sees and
        // resumes its normal event loop.
        let via_winapi = self.win_window.show_via_winapi();
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        if !via_winapi {
            // First frame: HWND not cached yet. Fall back to viewport commands.
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.win_window.set_hidden(false);
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        // Hide from taskbar/tray AND iconify the window. The `Minimized(true)`
        // matters because eframe's event loop checks `window.is_minimized()`
        // (IsIconic) and only skips `request_redraw()` when true. Without it,
        // eframe spins calling NtUserRedrawWindow under ControlFlow::Poll.
        // See eframe 0.28 run.rs:340-361.
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.win_window.set_hidden(true);
    }

    fn open_dashboard(&self) {
        if !self.settings_saved.sse_enabled {
            return;
        }
        let url = format!("http://127.0.0.1:{}/", self.settings_saved.sse_port);
        let _ = open::that(url);
    }

    fn restart_watcher(&mut self) {
        self.local_stats = LocalStats::new();
        self.log_lines.push("[TRAY] Restarting watcher...".into());
        if let Err(e) = self
            .watcher
            .start(self.settings_saved.to_env(&self.paths), self.log_tx.clone())
        {
            self.log_lines.push(format!("[TRAY] restart failed: {}", e));
        }
    }

    fn display_snap(&self, http: &HealthSnapshot) -> DisplaySnap {
        if http.reachable {
            return DisplaySnap {
                status: http.status,
                starting: false,
                uptime_seconds: http.uptime_seconds,
                notifications: http.notifications_received,
                last: if http.notifications_received > 0 {
                    Some(http.seconds_since_last_notification)
                } else {
                    None
                },
            };
        }
        let last = self
            .local_stats
            .last_notification_at
            .map(|t| t.elapsed().as_secs());
        let starting = !self.local_stats.ready;
        let status = if starting {
            Status::Unknown
        } else if last.map(|s| s > STALL_SECS).unwrap_or(false) {
            Status::Stalled
        } else {
            Status::Ok
        };
        DisplaySnap {
            status,
            starting,
            uptime_seconds: self.local_stats.started_at.elapsed().as_secs(),
            notifications: self.local_stats.notifications,
            last,
        }
    }

    fn save_and_restart(&mut self) {
        self.validation_errors = self.settings_draft.validate();
        if self.validation_errors.iter().any(|e| e.contains("not found")) {
            self.save_status = Some("Fix validation errors before saving.".into());
            return;
        }
        if let Err(e) = Watcher::resolve_exe(&self.settings_draft.watcher_exe) {
            self.save_status = Some(format!("Cannot save: {}", e));
            return;
        }
        if let Err(e) = self.settings_draft.save(&self.paths.settings_file) {
            self.save_status = Some(format!("Save failed: {}", e));
            return;
        }
        if let Err(e) = startup::apply(self.settings_draft.start_with_windows) {
            self.save_status = Some(format!("Saved settings, but startup registration failed: {}", e));
            return;
        }
        self.settings_saved = self.settings_draft.clone();
        if let Ok(exe) = Watcher::resolve_exe(&self.settings_saved.watcher_exe) {
            self.watcher.set_exe(exe);
        }
        *self.port_arc.lock().unwrap() = self.settings_saved.sse_port;
        *self.sse_enabled_arc.lock().unwrap() = self.settings_saved.sse_enabled;
        *self.dashboard_url.lock().unwrap() = dashboard_url_for(&self.settings_saved);
        self.restart_watcher();
        self.save_status = Some("Saved. Watcher restarted.".into());
    }

    fn revert(&mut self) {
        self.settings_draft = self.settings_saved.clone();
        self.validation_errors.clear();
        self.save_status = None;
    }
}

impl eframe::App for AppState {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.ui_ctx.lock().unwrap().is_none() {
            *self.ui_ctx.lock().unwrap() = Some(ctx.clone());
        }
        // Cache the HWND so the tray thread can use ShowWindow directly to
        // wake the window when eframe/winit is parked (Minimized+Invisible).
        self.win_window.ensure_hwnd("Podping Gossip Watcher");
        if self.minimize_pending {
            self.hide_window(ctx);
            self.minimize_pending = false;
        }
        self.drain_logs();
        self.process_actions(ctx);

        // Hide to tray when the user clicks the window close button.
        if !self.exiting.load(Ordering::SeqCst)
            && self.tray.is_some()
            && ctx.input(|i| i.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }

        let http = self.health.get();
        let snap = self.display_snap(&http);
        self.sync_tray(&snap);

        // When hidden to tray, skip UI work and use a long tick so background
        // state (log drain, tray tooltip) still updates. We mark the viewport
        // Minimized(true) on hide so eframe's event loop skips `request_redraw`
        // (see hide_window), which is what previously kept the CPU busy.
        if self.win_window.is_hidden() {
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
            return;
        }

        // Repaint regularly so log tail + health status stay fresh.
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        egui::TopBottomPanel::top("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                status_badge(ui, &snap);
                ui.separator();
                ui.label(format!("Uptime: {}", format_duration(snap.uptime_seconds)));
                ui.separator();
                ui.label(format!("Notifications: {}", snap.notifications));
                ui.separator();
                let last = match snap.last {
                    Some(secs) => format!("{}s ago", secs),
                    None => "—".into(),
                };
                ui.label(format!("Last: {}", last));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.settings_saved.sse_enabled {
                        if ui.button("Open Dashboard").clicked() {
                            self.open_dashboard();
                        }
                    }
                    if ui.button("Data Folder").clicked() {
                        let _ = open::that(&self.paths.root);
                    }
                });
            });
        });

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Log, "Log");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Log => self.log_tab(ui),
            Tab::Settings => self.settings_tab(ui),
        });
    }
}

impl AppState {
    fn log_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.auto_scroll, "Auto-scroll");
            if ui.button("Clear").clicked() {
                self.log_lines.clear();
            }
            ui.label(format!("({} lines)", self.log_lines.len()));
        });
        ui.separator();

        let text_style = egui::TextStyle::Monospace;
        let row_height = ui.text_style_height(&text_style);
        let total = self.log_lines.len();

        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false; 2]);
        if self.auto_scroll {
            scroll = scroll.stick_to_bottom(true);
        }
        scroll.show_rows(ui, row_height, total, |ui, range| {
            for i in range {
                if let Some(line) = self.log_lines.get(i) {
                    ui.add(
                        egui::Label::new(egui::RichText::new(line).monospace())
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                }
            }
        });
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Basic");
            egui::Grid::new("basic_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Node name");
                ui.text_edit_singleline(&mut self.settings_draft.node_friendly_name);
                ui.end_row();

                ui.label("Enable archive");
                ui.checkbox(&mut self.settings_draft.archive_enabled, "Store notifications in SQLite");
                ui.end_row();

                ui.label("Enable catch-up");
                ui.checkbox(&mut self.settings_draft.catchup_enabled, "Fetch missed notifications on startup");
                ui.end_row();

                ui.label("Enable SSE / dashboard");
                ui.checkbox(
                    &mut self.settings_draft.sse_enabled,
                    "Serve web UI and notification stream",
                );
                ui.end_row();

                ui.label("SSE / dashboard port");
                ui.add_enabled(
                    self.settings_draft.sse_enabled,
                    egui::DragValue::new(&mut self.settings_draft.sse_port).range(1..=65535),
                );
                ui.end_row();

                ui.label("Watcher executable");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings_draft.watcher_exe)
                            .hint_text("Leave empty for default")
                            .desired_width(280.0),
                    );
                    if ui.button("Browse…").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Select podping-gossipwatcher executable")
                            .add_filter("Executable", &["exe"])
                            .pick_file()
                        {
                            self.settings_draft.watcher_exe = path.display().to_string();
                        }
                    }
                    if ui.button("Use default").clicked() {
                        self.settings_draft.watcher_exe.clear();
                    }
                });
                ui.end_row();

                ui.label("Start with Windows");
                ui.checkbox(
                    &mut self.settings_draft.start_with_windows,
                    "Launch tray app at sign-in",
                );
                ui.end_row();
            });

            if self.settings_draft.watcher_exe.trim().is_empty() {
                ui.small("Default: podping-gossipwatcher.exe next to the tray app.");
            } else if !PathBuf::from(self.settings_draft.watcher_exe.trim()).is_file() {
                ui.colored_label(
                    egui::Color32::from_rgb(248, 81, 73),
                    "Path does not point to an existing file.",
                );
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Edit trusted publishers").clicked() {
                    let _ = open::that(&self.paths.trusted_publishers);
                }
                if ui.button("Edit trusted monitors").clicked() {
                    let _ = open::that(&self.paths.trusted_monitors);
                }
                if ui.button("Open data folder").clicked() {
                    let _ = open::that(&self.paths.root);
                }
            });

            ui.add_space(12.0);
            egui::CollapsingHeader::new("Advanced").default_open(false).show(ui, |ui| {
                egui::Grid::new("adv_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Bootstrap peer IDs");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.settings_draft.bootstrap_peer_ids)
                            .desired_rows(3)
                            .hint_text("Comma-separated iroh node IDs. Empty = defaults."),
                    );
                    ui.end_row();

                    ui.label("Peer announce interval (s)");
                    ui.add(egui::DragValue::new(&mut self.settings_draft.peer_announce_interval).range(0..=86400));
                    ui.end_row();

                    ui.label("Peer endorse interval (s)");
                    ui.add(egui::DragValue::new(&mut self.settings_draft.peer_endorse_interval).range(1..=86400));
                    ui.end_row();

                    ui.label("SSE buffer size");
                    ui.add_enabled(
                        self.settings_draft.sse_enabled,
                        egui::DragValue::new(&mut self.settings_draft.sse_buffer_size)
                            .range(16..=100_000),
                    );
                    ui.end_row();

                    ui.label("Write trace log");
                    ui.checkbox(&mut self.settings_draft.trace_to_file, "→ data folder / trace.log");
                    ui.end_row();
                });
                ui.small(format!(
                    "Data dir: {}",
                    self.paths.data.display()
                ));
            });

            ui.add_space(12.0);
            ui.separator();

            for e in &self.validation_errors {
                ui.colored_label(egui::Color32::from_rgb(210, 153, 34), format!("⚠ {}", e));
            }

            ui.horizontal(|ui| {
                let dirty = !settings_eq(&self.settings_draft, &self.settings_saved);
                if ui
                    .add_enabled(dirty, egui::Button::new("Save & Restart"))
                    .clicked()
                {
                    self.save_and_restart();
                }
                if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                    self.revert();
                }
                if let Some(msg) = &self.save_status {
                    ui.label(msg.clone());
                }
            });
        });
    }
}

fn dashboard_url_for(settings: &Settings) -> String {
    if settings.sse_enabled {
        format!("http://127.0.0.1:{}/", settings.sse_port)
    } else {
        String::new()
    }
}

fn settings_eq(a: &Settings, b: &Settings) -> bool {
    a == b
}

impl AppState {
    fn sync_tray(&mut self, snap: &DisplaySnap) {
        let icon_status = if snap.starting {
            Status::Unknown
        } else {
            snap.status
        };
        if self.last_tray_status != Some(icon_status) {
            if let Some(tray) = &self.tray {
                let _ = tray.icon.set_icon(Some(build_icon_rgba(icon_status)));
            }
            self.last_tray_status = Some(icon_status);
        }
        if let Some(tray) = &self.tray {
            let _ = tray.icon.set_tooltip(Some(tooltip_for(snap)));
        }
    }
}

fn tooltip_for(snap: &DisplaySnap) -> String {
    if snap.starting {
        return format!(
            "Podping Gossip Watcher — starting | Uptime {}",
            format_duration(snap.uptime_seconds)
        );
    }
    let status = match snap.status {
        Status::Ok => "OK",
        Status::Degraded => "DEGRADED",
        Status::Stalled => "STALLED",
        Status::Unknown => "UNKNOWN",
    };
    let last = match snap.last {
        Some(secs) => format!("{secs}s ago"),
        None => "—".into(),
    };
    format!(
        "Podping Gossip Watcher — {status} | Uptime {} | {} notifs | last {}",
        format_duration(snap.uptime_seconds),
        snap.notifications,
        last
    )
}

fn status_badge(ui: &mut egui::Ui, snap: &DisplaySnap) {
    let (label, color) = if snap.starting {
        ("STARTING", egui::Color32::from_rgb(139, 148, 158))
    } else {
        match snap.status {
            Status::Ok => ("OK", egui::Color32::from_rgb(63, 185, 80)),
            Status::Degraded => ("DEGRADED", egui::Color32::from_rgb(210, 153, 34)),
            Status::Stalled => ("STALLED", egui::Color32::from_rgb(248, 81, 73)),
            Status::Unknown => ("UNKNOWN", egui::Color32::from_rgb(139, 148, 158)),
        }
    };
    ui.colored_label(color, egui::RichText::new(label).strong());
}

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{:02}h", secs / 86400, (secs % 86400) / 3600)
    }
}
