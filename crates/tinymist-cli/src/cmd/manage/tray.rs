//! The system tray icon of the service manager.

use std::sync::mpsc::Sender;

use tray_icon::TrayIconBuilder;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};

use super::supervisor::ServiceCommand;

/// The native window handle shared between the application and the tray icon.
pub type SharedHwnd = std::sync::Arc<std::sync::Mutex<Option<isize>>>;

/// A message sent from the tray menu to the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayMessage {
    /// Shows the manager window.
    Open,
    /// Exits the manager.
    Quit,
}

/// The identifier of the tray menu items.
const MENU_OPEN: &str = "tinymist-manage-open";
const MENU_START_ALL: &str = "tinymist-manage-start-all";
const MENU_STOP_ALL: &str = "tinymist-manage-stop-all";
const MENU_QUIT: &str = "tinymist-manage-quit";

/// Runs the tray icon on a dedicated thread with a Win32 message pump.
pub fn spawn(
    event_tx: Sender<TrayMessage>,
    command_tx: Sender<ServiceCommand>,
    hwnd_slot: SharedHwnd,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("tinymist-manager-tray".to_string())
        .spawn(move || {
            if let Err(err) = run(event_tx, command_tx, hwnd_slot) {
                log::error!("the tray icon failed: {err}");
            }
        })
        .expect("failed to spawn the tray thread")
}

/// Builds the tray icon and pumps its messages until the thread is killed.
fn run(
    event_tx: Sender<TrayMessage>,
    command_tx: Sender<ServiceCommand>,
    hwnd_slot: SharedHwnd,
) -> Result<(), Box<dyn std::error::Error>> {
    let icon = tray_icon::Icon::from_rgba(icon_rgba(), 32, 32)?;

    let open = MenuItem::with_id(MENU_OPEN, "Open Manager", true, None);
    let start_all = MenuItem::with_id(MENU_START_ALL, "Start All", true, None);
    let stop_all = MenuItem::with_id(MENU_STOP_ALL, "Stop All", true, None);
    let quit = MenuItem::with_id(MENU_QUIT, "Quit", true, None);

    let menu = Menu::new();
    menu.append_items(&[
        &open,
        &PredefinedMenuItem::separator(),
        &start_all,
        &stop_all,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;

    let tray = TrayIconBuilder::new()
        .with_id("tinymist-service-manager")
        .with_tooltip("Tinymist Service Manager")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?;
    // Keeps the icon alive for the lifetime of the thread.
    let _ = &tray;

    let receiver = MenuEvent::receiver();
    loop {
        // The tray icon is driven by the Win32 message pump below.
        pump_messages();

        // The eframe repaint loop is suspended while the window is hidden, so
        // the tray thread restores the window itself instead of relying on
        // viewport commands processed by the suspended application.
        let hwnd = hwnd_slot.lock().ok().and_then(|slot| *slot);

        while let Ok(event) = receiver.try_recv() {
            match event.id().as_ref() {
                MENU_OPEN => {
                    if let Some(hwnd) = hwnd {
                        show_window(hwnd);
                    }
                    let _ = event_tx.send(TrayMessage::Open);
                }
                MENU_START_ALL => {
                    let _ = command_tx.send(ServiceCommand::StartAll);
                }
                MENU_STOP_ALL => {
                    let _ = command_tx.send(ServiceCommand::StopAll);
                }
                MENU_QUIT => {
                    // Resumes the repaint loop first so that the application
                    // can process the quit message and stop all the services.
                    if let Some(hwnd) = hwnd {
                        show_window(hwnd);
                    }
                    let _ = event_tx.send(TrayMessage::Quit);
                }
                _ => {}
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Shows and focuses the manager window from the tray thread.
fn show_window(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IsIconic, IsWindowVisible, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };

    // SAFETY: `hwnd` is the eframe window handle captured at creation time and
    // stays valid until the window is destroyed.
    unsafe {
        let hwnd = hwnd as _;
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        } else if IsWindowVisible(hwnd) == 0 {
            ShowWindow(hwnd, SW_SHOW);
        }
        SetForegroundWindow(hwnd);
    }
}

/// Pumps the Win32 messages of the tray window.
fn pump_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };

    // SAFETY: the message pointer points to a zeroed `MSG` record, which is the
    // expected usage of the message pump API.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        // Non-blocking peek keeps the polling loop responsive to shutdown.
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Generates the tray icon bitmap.
pub(crate) fn icon_rgba() -> Vec<u8> {
    // A dark rounded square with a light "t" glyph, rendered by hand so that
    // no additional asset is required.
    const SIZE: usize = 32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];

    let set = |rgba: &mut [u8], x: usize, y: usize, color: [u8; 4]| {
        let idx = (y * SIZE + x) * 4;
        rgba[idx..idx + 4].copy_from_slice(&color);
    };

    for y in 0..SIZE {
        for x in 0..SIZE {
            // Rounded corners.
            let corner = |cx: i32, cy: i32| {
                let dx = (x as i32 - cx).abs();
                let dy = (y as i32 - cy).abs();
                dx + dy > 5
            };
            let inside = !corner(2, 2)
                && !corner(29, 2)
                && !corner(2, 29)
                && !corner(29, 29)
                && (2..30).contains(&x)
                && (2..30).contains(&y);
            if inside {
                set(&mut rgba, x, y, [24, 36, 64, 255]);
            }
        }
    }

    // The vertical and horizontal bars of the glyph.
    for y in 8..25 {
        for x in 14..18 {
            set(&mut rgba, x, y, [240, 244, 255, 255]);
        }
    }
    for y in 8..12 {
        for x in 9..23 {
            set(&mut rgba, x, y, [240, 244, 255, 255]);
        }
    }

    rgba
}
