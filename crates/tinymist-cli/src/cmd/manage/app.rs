//! The graphical interface of the service manager.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use eframe::egui;

use super::config::{ManagerConfig, ServiceEntry, ServiceTransport};
use super::supervisor::{ServiceCommand, ServiceStatus, ServiceView, SharedServices};
use super::tray::TrayMessage;

/// The application state of the service manager.
pub struct ManagerApp {
    ctx: egui::Context,
    command_tx: Sender<ServiceCommand>,
    tray_rx: std::sync::mpsc::Receiver<TrayMessage>,
    services: SharedServices,
    /// The persisted configuration edited by the interface.
    config: ManagerConfig,
    config_path: Option<PathBuf>,
    /// The service being edited, if any.
    editing: Option<FormState>,
    /// Whether to show the tokens in plain text.
    show_tokens: bool,
    /// Whether to stop all the services when the manager exits.
    stop_on_exit: bool,
    /// Whether the manager should exit after handling the current frame.
    exiting: bool,
    /// Frames to wait before hiding the window at startup, if any.
    ///
    /// eframe forces the window visible after the first frame, so the initial
    /// hiding must happen on a later frame to stick.
    hide_countdown: Option<u32>,
    /// The name of the service whose log is displayed.
    log_view: Option<(String, String)>,
}

/// The state of the add/edit service form.
struct FormState {
    original_name: Option<String>,
    name: String,
    transport_ws: bool,
    port: String,
    token: String,
    font_paths: String,
    auto_start: bool,
}

impl FormState {
    fn new() -> Self {
        Self {
            original_name: None,
            name: String::new(),
            transport_ws: true,
            port: "23626".to_string(),
            token: String::new(),
            font_paths: String::new(),
            auto_start: true,
        }
    }

    fn from_entry(entry: &ServiceEntry) -> Self {
        Self {
            original_name: Some(entry.name.clone()),
            name: entry.name.clone(),
            transport_ws: entry.transport == ServiceTransport::Ws,
            port: entry.port.to_string(),
            token: entry.token.clone(),
            font_paths: entry.font_paths.join("\n"),
            auto_start: entry.auto_start,
        }
    }

    fn to_entry(&self) -> Result<ServiceEntry, String> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err("The service name must not be empty.".to_string());
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(
                "The service name may only contain letters, digits, '-' and '_'.".to_string(),
            );
        }
        let port: u16 = self
            .port
            .trim()
            .parse()
            .map_err(|_| format!("Invalid port: {}", self.port))?;
        if port == 0 {
            return Err("The port must not be zero.".to_string());
        }
        Ok(ServiceEntry {
            name,
            transport: if self.transport_ws {
                ServiceTransport::Ws
            } else {
                ServiceTransport::Tcp
            },
            port,
            token: self.token.clone(),
            font_paths: self
                .font_paths
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
            auto_start: self.auto_start,
        })
    }
}

impl ManagerApp {
    /// Creates the application state.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: egui::Context,
        command_tx: Sender<ServiceCommand>,
        tray_rx: std::sync::mpsc::Receiver<TrayMessage>,
        services: SharedServices,
        config: ManagerConfig,
        config_path: Option<PathBuf>,
        start_hidden: bool,
        hwnd: Option<isize>,
        hwnd_slot: super::tray::SharedHwnd,
    ) -> Self {
        // Publishes the native window handle so that the tray thread can
        // restore the window while the repaint loop is suspended (a hidden
        // window receives no repaint events on Windows).
        if let Some(hwnd) = hwnd
            && let Ok(mut slot) = hwnd_slot.lock()
        {
            *slot = Some(hwnd);
        }
        Self {
            ctx,
            command_tx,
            tray_rx,
            services,
            config,
            config_path,
            editing: None,
            show_tokens: false,
            stop_on_exit: true,
            exiting: false,
            hide_countdown: start_hidden.then_some(1),
            log_view: None,
        }
    }

    /// Handles the events coming from the tray icon.
    fn poll_tray(&mut self) {
        while let Ok(message) = self.tray_rx.try_recv() {
            match message {
                TrayMessage::Open => {
                    // The tray thread already made the window visible with
                    // Win32 calls; this only raises and focuses it.
                    self.ctx
                        .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayMessage::Quit => {
                    if self.stop_on_exit {
                        let _ = self.command_tx.send(ServiceCommand::StopAll);
                    }
                    self.exiting = true;
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    /// Persists the configuration and applies it to the supervisor.
    fn commit_config(&mut self, start_added: bool, added_name: Option<String>) {
        self.config.save();
        let _ = self
            .command_tx
            .send(ServiceCommand::ApplyConfig(self.config.services.clone()));
        if start_added && let Some(name) = added_name {
            let _ = self.command_tx.send(ServiceCommand::Start(name));
        }
    }

    /// Renders the service table.
    fn render_services(&mut self, ui: &mut egui::Ui) {
        let deleted = {
            let Ok(services) = self.services.lock() else {
                return;
            };
            let mut deleted: Option<String> = None;

            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("services")
                    .num_columns(9)
                    .striped(true)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.strong("Name");
                        ui.strong("Endpoint");
                        ui.strong("Status");
                        ui.strong("PID");
                        ui.strong("CPU");
                        ui.strong("Memory");
                        ui.strong("Connections");
                        ui.strong("Restarts");
                        ui.strong("Actions");
                        ui.end_row();

                        for service in services.iter() {
                            let name = service.entry.name.clone();
                            ui.label(&service.entry.name);
                            ui.monospace(service.endpoint());
                            ui.label(status_label(service));
                            ui.label(
                                service
                                    .pid
                                    .map(|pid| pid.to_string())
                                    .unwrap_or_else(|| "-".to_string()),
                            );
                            ui.label(format!("{:.1}%", service.cpu_percent));
                            ui.label(format!("{:.1} MiB", service.memory_mb));
                            ui.label(service.connections.to_string());
                            ui.label(service.restarts.to_string());

                            ui.horizontal(|ui| {
                                if service.status == ServiceStatus::Running {
                                    if ui.button("Stop").clicked() {
                                        let _ = self
                                            .command_tx
                                            .send(ServiceCommand::Stop(name.clone()));
                                    }
                                } else if ui.button("Start").clicked() {
                                    let _ =
                                        self.command_tx.send(ServiceCommand::Start(name.clone()));
                                }
                                if ui.button("Restart").clicked() {
                                    let _ =
                                        self.command_tx.send(ServiceCommand::Restart(name.clone()));
                                }
                                if ui.button("Edit").clicked() {
                                    self.editing = Some(FormState::from_entry(&service.entry));
                                }
                                if ui.button("Log").clicked() {
                                    self.log_view = Some((name.clone(), String::new()));
                                }
                                if ui.button("Delete").clicked() {
                                    deleted = Some(name.clone());
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
            deleted
        };

        if let Some(name) = deleted {
            self.config.services.retain(|it| it.name != name);
            self.commit_config(false, None);
        }
    }

    /// Renders the add/edit service form.
    fn render_form(&mut self, ctx: &egui::Context) {
        let Some(form) = &mut self.editing else {
            return;
        };

        let mut close = false;
        let mut window_open = true;
        let mut apply = false;
        let mut start_after = false;
        let mut error: Option<String> = None;

        egui::Window::new(if form.original_name.is_some() {
            "Edit Service"
        } else {
            "Add Service"
        })
        .open(&mut window_open)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("form")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut form.name);
                    ui.end_row();

                    ui.label("Transport");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut form.transport_ws, true, "WebSocket");
                        ui.radio_value(&mut form.transport_ws, false, "TCP");
                    });
                    ui.end_row();

                    ui.label("Port");
                    ui.text_edit_singleline(&mut form.port);
                    ui.end_row();

                    ui.label("Token");
                    ui.horizontal(|ui| {
                        if self.show_tokens {
                            ui.add(
                                egui::TextEdit::singleline(&mut form.token).desired_width(220.0),
                            );
                        } else {
                            let mut masked = "*".repeat(form.token.chars().count());
                            ui.add(
                                egui::TextEdit::singleline(&mut masked)
                                    .desired_width(220.0)
                                    .password(true),
                            );
                        }
                        ui.checkbox(&mut self.show_tokens, "show");
                    });
                    ui.end_row();

                    ui.label("Font paths");
                    ui.add(
                        egui::TextEdit::multiline(&mut form.font_paths)
                            .desired_width(260.0)
                            .hint_text("One directory per line"),
                    );
                    ui.end_row();

                    ui.label("Autostart");
                    ui.checkbox(&mut form.auto_start, "start with the manager");
                    ui.end_row();
                });

            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    match form.to_entry() {
                        Ok(entry) => {
                            let duplicated = self.config.services.iter().any(|it| {
                                it.name == entry.name
                                    && form.original_name.as_deref() != Some(it.name.as_str())
                            });
                            if duplicated {
                                error =
                                    Some("A service with this name already exists.".to_string());
                            } else {
                                match &form.original_name {
                                    Some(original) => {
                                        if let Some(slot) = self
                                            .config
                                            .services
                                            .iter_mut()
                                            .find(|it| it.name == *original)
                                        {
                                            *slot = entry.clone();
                                        }
                                    }
                                    None => self.config.services.push(entry.clone()),
                                }
                                let added = form.original_name.is_none();
                                apply = true;
                                start_after = added && entry.auto_start;
                                close = true;
                            }
                        }
                        Err(err) => error = Some(err),
                    }
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });

            if let Some(err) = &error {
                ui.colored_label(egui::Color32::RED, err);
            }
        });

        if apply {
            let added_name = self
                .editing
                .as_ref()
                .and_then(|form| form.to_entry().ok())
                .map(|entry| entry.name);
            self.commit_config(start_after, added_name);
        }
        if close || !window_open {
            self.editing = None;
        }
    }

    /// Renders the log viewer window.
    fn render_log(&mut self, ctx: &egui::Context) {
        let Some((name, content)) = &mut self.log_view else {
            return;
        };

        let mut close = false;
        egui::Window::new(format!("Log - {name}"))
            .default_width(720.0)
            .default_height(420.0)
            .open(&mut close)
            .show(ctx, |ui| {
                if ui.button("Refresh").clicked() {
                    *content = read_log_tail(name);
                }
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.monospace(content.as_str());
                    });
            });
        if close {
            self.log_view = None;
        }
    }
}

impl eframe::App for ManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe forces the window visible right after the first frame, so the
        // startup hiding waits one extra frame to take effect.
        if self.hide_countdown == Some(0) {
            self.hide_countdown = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        } else if let Some(count) = self.hide_countdown.as_mut() {
            *count -= 1;
        }

        self.poll_tray();

        if self.exiting {
            return;
        }

        // Keeps repainting so that the sampled statuses stay fresh.
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        if ctx.input(|input| input.viewport().close_requested()) && !self.exiting {
            // Minimizes to the tray instead of exiting.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Tinymist Service Manager");
                if ui.button("Start All").clicked() {
                    let _ = self.command_tx.send(ServiceCommand::StartAll);
                }
                if ui.button("Stop All").clicked() {
                    let _ = self.command_tx.send(ServiceCommand::StopAll);
                }
                if ui.button("Add Service").clicked() {
                    self.editing = Some(FormState::new());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut autostart = super::instance::autostart_command().is_some();
                    if ui.checkbox(&mut autostart, "Start with Windows").changed() {
                        let command = format!(
                            "\"{}\" manage --minimized",
                            std::env::current_exe()
                                .map(|exe| exe.display().to_string())
                                .unwrap_or_default()
                        );
                        super::instance::set_autostart(autostart, &command);
                    }
                    if ui.button("Quit").clicked() {
                        if self.stop_on_exit {
                            let _ = self.command_tx.send(ServiceCommand::StopAll);
                        }
                        self.exiting = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_services(ui);
        });
        self.render_form(ctx);
        self.render_log(ctx);

        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.stop_on_exit, "Stop services on exit");
                if let Some(path) = &self.config_path {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(path.display().to_string());
                    });
                }
            });
        });
    }
}

/// The label of a service status.
fn status_label(service: &ServiceView) -> String {
    match service.status {
        ServiceStatus::Stopped => "Stopped".to_string(),
        ServiceStatus::Running => "Running".to_string(),
        ServiceStatus::Restarting => "Restarting".to_string(),
    }
}

/// Reads the tail of a service log.
fn read_log_tail(name: &str) -> String {
    use std::io::Read;

    let Some(dir) = super::config::log_dir() else {
        return String::new();
    };
    let name = name.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_");
    let path = dir.join(format!("{name}.log"));
    let Ok(mut file) = std::fs::File::open(&path) else {
        return String::new();
    };
    const TAIL: u64 = 64 * 1024;
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if len > TAIL {
        use std::io::Seek;
        let _ = file.seek(std::io::SeekFrom::Start(len - TAIL));
    }
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}
