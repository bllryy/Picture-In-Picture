use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::CURRENT_TIME;

#[derive(Debug)]
#[allow(dead_code)]
pub struct WindowInfo {
    pub window_id: u32,
    pub name: Option<String>,
    pub class: Option<String>,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug)]
pub enum PickerError {
    ConnectionFailed(String),
    GrabFailed,
    UserCancelled,
    WindowQueryFailed(String),
}

impl std::fmt::Display for PickerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionFailed(e) => write!(f, "Failed to connect to X11: {}", e),
            Self::GrabFailed => write!(f, "Failed to grab pointer"),
            Self::UserCancelled => write!(f, "Selection cancelled"),
            Self::WindowQueryFailed(e) => write!(f, "Failed to query window: {}", e),
        }
    }
}

impl std::error::Error for PickerError {}

pub fn pick_window() -> Result<WindowInfo, PickerError> {
    let (conn, screen_num) = RustConnection::connect(None)
        .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;

    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    // Create a crosshair cursor for visual feedback
    let cursor = create_crosshair_cursor(&conn, screen)?;

    // Grab the pointer - all clicks will come to us
    let grab_reply = conn
        .grab_pointer(
            false,                                                        // owner_events
            root,                                                         // grab_window
            (EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE).into(), // event_mask
            GrabMode::ASYNC,                                              // pointer_mode
            GrabMode::ASYNC,                                              // keyboard_mode
            x11rb::NONE,                                                  // confine_to
            cursor,                                                       // cursor
            CURRENT_TIME,                                                 // time
        )
        .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?
        .reply()
        .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;

    if grab_reply.status != GrabStatus::SUCCESS {
        return Err(PickerError::GrabFailed);
    }

    // Wait for a button press
    let selected_window = loop {
        let event = conn
            .wait_for_event()
            .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;

        match event {
            x11rb::protocol::Event::ButtonPress(ev) => {
                // Right-click or middle-click cancels
                if ev.detail != 1 {
                    conn.ungrab_pointer(CURRENT_TIME).ok();
                    conn.flush().ok();
                    return Err(PickerError::UserCancelled);
                }

                // Find the actual top-level window under the cursor
                let window = find_client_window(&conn, ev.child, root)?;
                break window;
            }
            x11rb::protocol::Event::ButtonRelease(_) => {
                // Ignore button releases
            }
            _ => {}
        }
    };

    // Release the pointer grab
    conn.ungrab_pointer(CURRENT_TIME)
        .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;
    conn.flush()
        .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;

    // If user clicked on root window (desktop), that's not useful
    if selected_window == root || selected_window == 0 {
        return Err(PickerError::UserCancelled);
    }

    // Get window information
    get_window_info(&conn, selected_window)
}

/// Find the top-level client window at the given coordinates
fn find_client_window(
    conn: &RustConnection,
    _clicked_window: u32,
    root: u32,
) -> Result<u32, PickerError> {
    // Get click coordinates
    let pointer = conn
        .query_pointer(root)
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?
        .reply()
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?;

    let click_x = pointer.root_x as i32;
    let click_y = pointer.root_y as i32;

    // Get the list of all managed windows from the WM
    let net_client_list = get_atom(conn, "_NET_CLIENT_LIST_STACKING")?;

    let reply = conn
        .get_property(false, root, net_client_list, AtomEnum::WINDOW, 0, 1024)
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?
        .reply()
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?;

    if reply.type_ == 0 {
        // Try _NET_CLIENT_LIST as fallback
        let net_client_list = get_atom(conn, "_NET_CLIENT_LIST")?;
        let reply = conn
            .get_property(false, root, net_client_list, AtomEnum::WINDOW, 0, 1024)
            .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?
            .reply()
            .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?;

        if reply.type_ == 0 {
            return Ok(root);
        }
    }

    // Parse window list (4 bytes per window ID)
    let windows: Vec<u32> = reply
        .value
        .chunks_exact(4)
        .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();

    // Find the topmost window that contains the click point
    // _NET_CLIENT_LIST_STACKING is ordered bottom-to-top, so iterate in reverse
    for &win in windows.iter().rev() {
        if let Ok(true) = window_contains_point(conn, win, root, click_x, click_y) {
            return Ok(win);
        }
    }

    Ok(root)
}

/// Check if a window contains the given point
fn window_contains_point(
    conn: &RustConnection,
    window: u32,
    root: u32,
    x: i32,
    y: i32,
) -> Result<bool, PickerError> {
    // Get window geometry
    let geom = match conn.get_geometry(window) {
        Ok(cookie) => match cookie.reply() {
            Ok(g) => g,
            Err(_) => return Ok(false),
        },
        Err(_) => return Ok(false),
    };

    // Translate to root coordinates
    let coords = match conn.translate_coordinates(window, root, 0, 0) {
        Ok(cookie) => match cookie.reply() {
            Ok(c) => c,
            Err(_) => return Ok(false),
        },
        Err(_) => return Ok(false),
    };

    let win_x = coords.dst_x as i32;
    let win_y = coords.dst_y as i32;
    let win_w = geom.width as i32;
    let win_h = geom.height as i32;

    Ok(x >= win_x && x < win_x + win_w && y >= win_y && y < win_y + win_h)
}

fn get_atom(conn: &RustConnection, name: &str) -> Result<u32, PickerError> {
    let reply = conn
        .intern_atom(false, name.as_bytes())
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?
        .reply()
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?;

    Ok(reply.atom)
}

fn get_window_info(conn: &RustConnection, window: u32) -> Result<WindowInfo, PickerError> {
    // Get geometry
    let geom = conn
        .get_geometry(window)
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?
        .reply()
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?;

    // Translate coordinates to root window
    let coords = conn
        .translate_coordinates(window, geom.root, 0, 0)
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?
        .reply()
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?;

    // Get window name (_NET_WM_NAME or WM_NAME)
    let name = get_window_name(conn, window)?;

    // Get window class (WM_CLASS)
    let class = get_window_class(conn, window)?;

    Ok(WindowInfo {
        window_id: window,
        name,
        class,
        x: coords.dst_x,
        y: coords.dst_y,
        width: geom.width,
        height: geom.height,
    })
}

fn get_window_name(conn: &RustConnection, window: u32) -> Result<Option<String>, PickerError> {
    // Try _NET_WM_NAME first (UTF-8)
    let net_wm_name = get_atom(conn, "_NET_WM_NAME")?;
    let utf8_string = get_atom(conn, "UTF8_STRING")?;

    let reply = conn
        .get_property(false, window, net_wm_name, utf8_string, 0, 1024)
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?
        .reply()
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?;

    if reply.type_ != 0 && !reply.value.is_empty() {
        return Ok(Some(String::from_utf8_lossy(&reply.value).into_owned()));
    }

    // Fall back to WM_NAME
    let reply = conn
        .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 1024)
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?
        .reply()
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?;

    if reply.type_ != 0 && !reply.value.is_empty() {
        return Ok(Some(String::from_utf8_lossy(&reply.value).into_owned()));
    }

    Ok(None)
}

fn get_window_class(conn: &RustConnection, window: u32) -> Result<Option<String>, PickerError> {
    let reply = conn
        .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?
        .reply()
        .map_err(|e| PickerError::WindowQueryFailed(e.to_string()))?;

    if reply.type_ != 0 && !reply.value.is_empty() {
        // WM_CLASS contains two null-terminated strings: instance and class
        // We want the class (second one)
        let parts: Vec<&[u8]> = reply.value.split(|&b| b == 0).collect();
        if parts.len() >= 2 && !parts[1].is_empty() {
            return Ok(Some(String::from_utf8_lossy(parts[1]).into_owned()));
        } else if !parts.is_empty() && !parts[0].is_empty() {
            return Ok(Some(String::from_utf8_lossy(parts[0]).into_owned()));
        }
    }

    Ok(None)
}

fn create_crosshair_cursor(
    conn: &RustConnection,
    _screen: &Screen,
) -> Result<u32, PickerError> {
    // Use the standard crosshair cursor from the cursor font
    let font = conn
        .generate_id()
        .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;

    conn.open_font(font, b"cursor")
        .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;

    let cursor = conn
        .generate_id()
        .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;

    // 34 is the crosshair cursor in the standard cursor font
    conn.create_glyph_cursor(
        cursor,
        font,
        font,
        34,      // source_char (crosshair)
        34 + 1,  // mask_char
        0, 0, 0, // foreground RGB (black)
        65535, 65535, 65535, // background RGB (white)
    )
    .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;

    conn.close_font(font)
        .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;

    conn.flush()
        .map_err(|e| PickerError::ConnectionFailed(e.to_string()))?;

    Ok(cursor)
}
