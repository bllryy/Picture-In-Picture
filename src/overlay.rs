use softbuffer::Surface;
use std::num::NonZeroU32;
use std::rc::Rc;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Icon, Window, WindowAttributes, WindowId, WindowLevel};

use crate::capture::WindowCapture;

const ICON_DATA: &[u8] = include_bytes!("../pictureinpicture.png");

fn load_window_icon() -> Option<Icon> {
    let img = image::load_from_memory(ICON_DATA).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Icon::from_rgba(img.into_raw(), width, height).ok()
}

pub struct PipOverlay {
    target_window_id: u32,
    initial_width: u32,
    initial_height: u32,
}

struct App {
    window: Option<Rc<Window>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    capture: Option<WindowCapture>,
    target_window_id: u32,
    source_width: u32,
    source_height: u32,
}

impl PipOverlay {
    pub fn new(window_id: u32, width: u32, height: u32) -> Self {
        Self {
            target_window_id: window_id,
            initial_width: width,
            initial_height: height,
        }
    }

    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let event_loop = EventLoop::new()?;
        event_loop.set_control_flow(ControlFlow::Poll);

        let mut app = App {
            window: None,
            surface: None,
            capture: None,
            target_window_id: self.target_window_id,
            source_width: self.initial_width,
            source_height: self.initial_height,
        };

        event_loop.run_app(&mut app)?;
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        // Calculate initial PiP size (1/4 of source, min 200px)
        let pip_width = (self.source_width / 4).max(200);
        let pip_height = (self.source_height / 4).max(150);

        let mut window_attrs = WindowAttributes::default()
            .with_title("PiP Viewer")
            .with_inner_size(LogicalSize::new(pip_width, pip_height))
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_resizable(true);

        if let Some(icon) = load_window_icon() {
            window_attrs = window_attrs.with_window_icon(Some(icon));
        }

        let window = Rc::new(
            event_loop
                .create_window(window_attrs)
                .expect("Failed to create window"),
        );

        // Create softbuffer surface
        let context = softbuffer::Context::new(window.clone()).expect("Failed to create context");
        let mut surface = Surface::new(&context, window.clone()).expect("Failed to create surface");

        // Resize surface to match window
        let size = window.inner_size();
        if size.width > 0 && size.height > 0 {
            let _ = surface.resize(
                NonZeroU32::new(size.width).unwrap(),
                NonZeroU32::new(size.height).unwrap(),
            );
        }

        // Create capture
        let capture = match WindowCapture::new(self.target_window_id) {
            Ok(c) => {
                self.source_width = c.width();
                self.source_height = c.height();
                Some(c)
            }
            Err(e) => {
                eprintln!("Failed to initialize capture: {}", e);
                None
            }
        };

        self.window = Some(window);
        self.surface = Some(surface);
        self.capture = capture;
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(surface) = &mut self.surface {
                    if size.width > 0 && size.height > 0 {
                        let _ = surface.resize(
                            NonZeroU32::new(size.width).unwrap(),
                            NonZeroU32::new(size.height).unwrap(),
                        );
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.render();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Request redraw for next frame (this drives our ~60fps loop)
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl App {
    fn render(&mut self) {
        let Some(window) = &self.window else { return };
        let Some(surface) = &mut self.surface else { return };
        let Some(capture) = &mut self.capture else { return };

        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }

        // Get dimensions before borrowing for capture
        let src_width = capture.width() as usize;
        let src_height = capture.height() as usize;
        let dst_width = size.width as usize;
        let dst_height = size.height as usize;

        // Capture frame from source window
        let frame_data = match capture.capture_frame() {
            Ok(data) => data,
            Err(e) => {
                eprintln!("Capture error: {}", e);
                return;
            }
        };

        // Ensure surface is properly sized
        let _ = surface.resize(
            NonZeroU32::new(size.width).unwrap(),
            NonZeroU32::new(size.height).unwrap(),
        );

        // Get buffer
        let mut buffer = match surface.buffer_mut() {
            Ok(b) => b,
            Err(_) => return,
        };

        // Verify buffer size
        let expected_size = dst_width * dst_height;
        if buffer.len() < expected_size {
            return;
        }

        // Scale and copy the image
        // Using nearest-neighbor for speed (good enough for PiP)
        for dst_y in 0..dst_height {
            let src_y = (dst_y * src_height) / dst_height;
            for dst_x in 0..dst_width {
                let src_x = (dst_x * src_width) / dst_width;

                let src_idx = (src_y * src_width + src_x) * 4;
                let dst_idx = dst_y * dst_width + dst_x;

                if src_idx + 3 < frame_data.len() && dst_idx < buffer.len() {
                    // Source is BGRA, softbuffer expects RGB in u32 (0x00RRGGBB)
                    let b = frame_data[src_idx] as u32;
                    let g = frame_data[src_idx + 1] as u32;
                    let r = frame_data[src_idx + 2] as u32;
                    buffer[dst_idx] = (r << 16) | (g << 8) | b;
                }
            }
        }

        let _ = buffer.present();
    }
}
