//! The Windows service manager supervising multiple language servers.

pub mod app;
pub mod config;
pub mod instance;
pub mod status;
pub mod supervisor;
pub mod tray;

use std::sync::{Arc, Mutex, mpsc};

use eframe::egui;

use crate::cmd::manage::app::ManagerApp;
use crate::cmd::manage::config::ManagerConfig;
use crate::cmd::manage::instance::InstanceGuard;
use crate::cmd::manage::supervisor::{ServiceCommand, SharedServices, Supervisor};

/// The arguments of the `tinymist manage` command.
#[derive(Debug, Clone, clap::Parser)]
pub struct ManageArgs {
    /// Starts with the window hidden, only showing the tray icon.
    #[clap(long)]
    pub minimized: bool,
}

/// The entry point of the service manager.
pub fn manage_main(args: ManageArgs) -> anyhow::Result<()> {
    // Prevents multiple managers from fighting for the same services.
    if InstanceGuard::acquire().is_none() {
        log::warn!("another service manager instance is already running");
        return Ok(());
    }

    let config = ManagerConfig::load();
    let config_path = config::config_path();
    let shared: SharedServices = Arc::new(Mutex::new(Vec::new()));

    let (command_tx, command_rx) = mpsc::channel::<ServiceCommand>();
    let services = config.services.clone();
    Supervisor::spawn(shared.clone(), command_rx, services);
    status::spawn_sampler(shared.clone());

    // Starts the services marked as auto-started.
    for entry in config.services.iter().filter(|it| it.auto_start) {
        let _ = command_tx.send(ServiceCommand::Start(entry.name.clone()));
    }

    let (tray_tx, tray_rx) = mpsc::channel();
    let hwnd_slot: tray::SharedHwnd = Arc::new(Mutex::new(None));
    tray::spawn(tray_tx, command_tx.clone(), hwnd_slot.clone());

    let icon = egui::IconData {
        width: 32,
        height: 32,
        rgba: tray::icon_rgba(),
    };
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([960.0, 540.0])
        .with_icon(icon);

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let app_shared = shared.clone();
    let app_config = config.clone();
    let app_tx = command_tx.clone();
    let minimized = args.minimized;
    let creator = move |cc: &eframe::CreationContext| {
        Ok(Box::new(ManagerApp::new(
            cc.egui_ctx.clone(),
            app_tx,
            tray_rx,
            app_shared,
            app_config,
            config_path,
            minimized,
            extract_hwnd(cc),
            hwnd_slot,
        )) as Box<dyn eframe::App>)
    };

    eframe::run_native("Tinymist Service Manager", options, Box::new(creator))
        .map_err(|err| anyhow::anyhow!("failed to start the manager window: {err}"))?;

    // Makes sure no service is left behind when the window loop exits.
    let _ = command_tx.send(ServiceCommand::StopAll);
    std::thread::sleep(std::time::Duration::from_millis(300));
    Ok(())
}

/// Extracts the Win32 window handle from the creation context.
fn extract_hwnd(cc: &eframe::CreationContext) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}
