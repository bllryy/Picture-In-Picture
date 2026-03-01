mod capture;
mod overlay;
mod tray;
mod window_list;
mod window_picker;

use overlay::PipOverlay;
use tray::TrayAction;

fn main() {
    println!("PiP Viewer");
    println!("System tray icon active. Right-click to select a window.\n");

    // Start the system tray
    let rx = tray::run_tray();

    // Main event loop
    loop {
        match rx.recv() {
            Ok(TrayAction::SelectWindow(window_id)) => {
                start_pip(window_id);
            }
            Ok(TrayAction::ClickToSelect) => {
                println!("Click on any window to select it...");
                match window_picker::pick_window() {
                    Ok(info) => {
                        println!(
                            "Selected: {} ({})",
                            info.name.as_deref().unwrap_or("<unnamed>"),
                            info.class.as_deref().unwrap_or("<unknown>")
                        );
                        start_pip(info.window_id);
                    }
                    Err(e) => {
                        eprintln!("Selection cancelled: {}", e);
                    }
                }
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

fn start_pip(window_id: u32) {
    let (width, height) = get_window_size(window_id).unwrap_or((800, 600));

    println!("Starting PiP for window 0x{:x}...", window_id);

    let overlay = PipOverlay::new(window_id, width, height);

    if let Err(e) = overlay.run() {
        eprintln!("Overlay error: {}", e);
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
