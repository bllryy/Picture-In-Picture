use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

#[derive(Debug, Clone)]
pub struct WindowEntry {
    pub id: u32,
    pub name: String,
    pub class: String,
}

pub fn list_windows() -> Result<Vec<WindowEntry>, Box<dyn std::error::Error>> {
    let (conn, screen_num) = RustConnection::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    // Get _NET_CLIENT_LIST
    let net_client_list = conn
        .intern_atom(false, b"_NET_CLIENT_LIST")?
        .reply()?
        .atom;

    let reply = conn
        .get_property(false, root, net_client_list, AtomEnum::WINDOW, 0, 1024)?
        .reply()?;

    if reply.type_ == 0 {
        return Ok(Vec::new());
    }

    // Parse window IDs
    let window_ids: Vec<u32> = reply
        .value
        .chunks_exact(4)
        .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();

    let mut entries = Vec::new();

    for id in window_ids {
        let name = get_window_name(&conn, id).unwrap_or_else(|| "<unnamed>".to_string());
        let class = get_window_class(&conn, id).unwrap_or_else(|| "<unknown>".to_string());

        // Skip empty or very short names (likely not user windows)
        if name.len() > 1 || class.len() > 1 {
            entries.push(WindowEntry { id, name, class });
        }
    }

    Ok(entries)
}

fn get_window_name(conn: &RustConnection, window: u32) -> Option<String> {
    // Try _NET_WM_NAME first (UTF-8)
    let net_wm_name = conn.intern_atom(false, b"_NET_WM_NAME").ok()?.reply().ok()?.atom;
    let utf8_string = conn.intern_atom(false, b"UTF8_STRING").ok()?.reply().ok()?.atom;

    let reply = conn
        .get_property(false, window, net_wm_name, utf8_string, 0, 1024)
        .ok()?
        .reply()
        .ok()?;

    if reply.type_ != 0 && !reply.value.is_empty() {
        return Some(String::from_utf8_lossy(&reply.value).into_owned());
    }

    // Fall back to WM_NAME
    let reply = conn
        .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 1024)
        .ok()?
        .reply()
        .ok()?;

    if reply.type_ != 0 && !reply.value.is_empty() {
        return Some(String::from_utf8_lossy(&reply.value).into_owned());
    }

    None
}

fn get_window_class(conn: &RustConnection, window: u32) -> Option<String> {
    let reply = conn
        .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)
        .ok()?
        .reply()
        .ok()?;

    if reply.type_ != 0 && !reply.value.is_empty() {
        // WM_CLASS contains two null-terminated strings: instance and class
        let parts: Vec<&[u8]> = reply.value.split(|&b| b == 0).collect();
        if parts.len() >= 2 && !parts[1].is_empty() {
            return Some(String::from_utf8_lossy(parts[1]).into_owned());
        } else if !parts.is_empty() && !parts[0].is_empty() {
            return Some(String::from_utf8_lossy(parts[0]).into_owned());
        }
    }

    None
}
