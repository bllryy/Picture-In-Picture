use ksni::{self, menu::*, Icon, ToolTip};
use std::sync::mpsc::{self, Receiver, Sender};

use crate::window_list::{self, WindowEntry};

const ICON_DATA: &[u8] = include_bytes!("../pictureinpicture.png");

#[derive(Debug, Clone)]
pub enum TrayAction {
    SelectWindow(u32), // Window ID
    ClickToSelect,
    Quit,
}

pub struct PipTray {
    tx: Sender<TrayAction>,
    windows: Vec<WindowEntry>,
    icon: Vec<Icon>,
}

impl PipTray {
    pub fn new(tx: Sender<TrayAction>) -> Self {
        let windows = window_list::list_windows().unwrap_or_default();
        let icon = load_icon();
        Self { tx, windows, icon }
    }

    fn refresh_windows(&mut self) {
        self.windows = window_list::list_windows().unwrap_or_default();
    }
}

fn load_icon() -> Vec<Icon> {
    let img = match image::load_from_memory(ICON_DATA) {
        Ok(img) => img.into_rgba8(),
        Err(_) => return Vec::new(),
    };

    let width = img.width() as i32;
    let height = img.height() as i32;

    // Convert RGBA to ARGB (network byte order for DBus)
    let mut argb_data = Vec::with_capacity((width * height * 4) as usize);
    for pixel in img.pixels() {
        let [r, g, b, a] = pixel.0;
        // ARGB32 in network byte order (big-endian)
        argb_data.push(a);
        argb_data.push(r);
        argb_data.push(g);
        argb_data.push(b);
    }

    vec![Icon {
        width,
        height,
        data: argb_data,
    }]
}

impl ksni::Tray for PipTray {
    fn id(&self) -> String {
        "pip-viewer".to_string()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icon.clone()
    }

    fn title(&self) -> String {
        "PiP Viewer".to_string()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "PiP Viewer".to_string(),
            description: "Click to select a window for picture-in-picture".to_string(),
            icon_name: String::new(),
            icon_pixmap: self.icon.clone(),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = Vec::new();

        // Header
        items.push(MenuItem::Standard(StandardItem {
            label: "── Select Window ──".to_string(),
            enabled: false,
            ..Default::default()
        }));

        // Window list
        for window in &self.windows {
            let window_id = window.id;
            let label = if window.name.len() > 40 {
                format!("{}... ({})", &window.name[..37], window.class)
            } else {
                format!("{} ({})", window.name, window.class)
            };

            items.push(MenuItem::Standard(StandardItem {
                label,
                activate: Box::new(move |tray: &mut Self| {
                    let _ = tray.tx.send(TrayAction::SelectWindow(window_id));
                }),
                ..Default::default()
            }));
        }

        // Separator
        items.push(MenuItem::Separator);

        // Refresh windows
        items.push(MenuItem::Standard(StandardItem {
            label: "↻ Refresh Window List".to_string(),
            activate: Box::new(|tray: &mut Self| {
                tray.refresh_windows();
            }),
            ..Default::default()
        }));

        // Click to select option
        items.push(MenuItem::Standard(StandardItem {
            label: "⊕ Click to Select...".to_string(),
            activate: Box::new(|tray: &mut Self| {
                let _ = tray.tx.send(TrayAction::ClickToSelect);
            }),
            ..Default::default()
        }));

        // Separator
        items.push(MenuItem::Separator);

        // Quit
        items.push(MenuItem::Standard(StandardItem {
            label: "Quit".to_string(),
            activate: Box::new(|tray: &mut Self| {
                let _ = tray.tx.send(TrayAction::Quit);
            }),
            ..Default::default()
        }));

        items
    }
}

pub fn run_tray() -> Receiver<TrayAction> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let tray = PipTray::new(tx);
        let service = ksni::TrayService::new(tray);
        let _ = service.run();
    });

    rx
}
