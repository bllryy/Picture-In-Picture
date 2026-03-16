#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    X11,
    Wayland,
}

pub fn detect_session() -> SessionType {
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        SessionType::Wayland
    } else {
        SessionType::X11
    }
}
