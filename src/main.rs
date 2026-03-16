mod capture;
mod capture_backend;
mod capture_pw;
mod overlay;
mod session;
mod tray;
mod window_list;
mod window_picker;

use capture_backend::CaptureBackend;
use overlay::PipOverlay;
use session::SessionType;
use tray::TrayAction;

fn main() {
    let session_type = session::detect_session();

    println!("PiP Viewer");
    println!(
        "Session type: {}",
        match session_type {
            SessionType::X11 => "X11",
            SessionType::Wayland => "Wayland",
        }
    );
    println!("System tray icon active. Right-click to select a window.\n");

    // Start the system tray
    let rx = tray::run_tray(session_type);

    // Main event loop
    loop {
        match rx.recv() {
            Ok(TrayAction::SelectWindow(window_id)) => {
                start_pip_x11(window_id);
            }
            Ok(TrayAction::ClickToSelect) => {
                if session_type == SessionType::X11 {
                    println!("Click on any window to select it...");
                    match window_picker::pick_window() {
                        Ok(info) => {
                            println!(
                                "Selected: {} ({})",
                                info.name.as_deref().unwrap_or("<unnamed>"),
                                info.class.as_deref().unwrap_or("<unknown>")
                            );
                            start_pip_x11(info.window_id);
                        }
                        Err(e) => {
                            eprintln!("Selection cancelled: {}", e);
                        }
                    }
                }
            }
            Ok(TrayAction::PortalSelect) => {
                start_pip_wayland();
            }
            Ok(TrayAction::Quit) => {
                println!("Exiting...");
                break;
            }
            Err(_) => {
                // Channel closed, tray died
                eprintln!("Tray disconnected");
                break;
            }
        }
    }
}

fn start_pip_x11(window_id: u32) {
    let (_width, _height) = get_window_size(window_id).unwrap_or((800, 600));

    println!("Starting PiP for window 0x{:x}...", window_id);

    match capture::WindowCapture::new(window_id) {
        Ok(cap) => {
            let overlay = PipOverlay::new(Box::new(cap));
            if let Err(e) = overlay.run() {
                eprintln!("Overlay error: {}", e);
            }
        }
        Err(e) => {
            eprintln!("Failed to start capture: {}", e);
        }
    }

    println!("PiP closed. Select another window from the tray.\n");
}

fn start_pip_wayland() {
    println!("Starting portal window selection...");

    match capture_pw::PipeWireCapture::new() {
        Ok(cap) => {
            println!(
                "PipeWire capture started ({}x{})",
                cap.width(),
                cap.height()
            );
            let overlay = PipOverlay::new(Box::new(cap));
            if let Err(e) = overlay.run() {
                eprintln!("Overlay error: {}", e);
            }
        }
        Err(e) => {
            eprintln!("Failed to start PipeWire capture: {}", e);
        }
    }

    println!("PiP closed. Select another window from the tray.\n");
}

fn get_window_size(window_id: u32) -> Option<(u32, u32)> {
    use x11rb::protocol::xproto::ConnectionExt;
    use x11rb::rust_connection::RustConnection;

    let (conn, _) = RustConnection::connect(None).ok()?;
    let geom = conn.get_geometry(window_id).ok()?.reply().ok()?;
    Some((geom.width as u32, geom.height as u32))
}
