use crate::capture_backend::CaptureBackend;
use std::sync::{Arc, Mutex};

/// Frame data shared between PipeWire thread and main thread
struct SharedFrame {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

pub struct PipeWireCapture {
    shared: Arc<Mutex<SharedFrame>>,
    /// Local copy of the latest frame for returning references
    local_frame: Vec<u8>,
    local_width: u32,
    local_height: u32,
    _pw_thread: std::thread::JoinHandle<()>,
}

impl PipeWireCapture {
    /// Start a screencast session via xdg-desktop-portal and capture frames via PipeWire.
    ///
    /// This blocks briefly while the portal dialog is shown to the user, then
    /// spawns a background thread for the PipeWire main loop.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        // Run the async portal session on a temporary tokio runtime
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let (pw_fd, node_id, width, height) = rt.block_on(start_portal_session())?;

        let shared = Arc::new(Mutex::new(SharedFrame {
            data: vec![0u8; (width * height * 4) as usize],
            width,
            height,
        }));

        let shared_clone = Arc::clone(&shared);

        let pw_thread = std::thread::spawn(move || {
            if let Err(e) = run_pipewire_loop(pw_fd, node_id, shared_clone) {
                eprintln!("PipeWire thread error: {}", e);
            }
        });

        Ok(Self {
            shared,
            local_frame: vec![0u8; (width * height * 4) as usize],
            local_width: width,
            local_height: height,
            _pw_thread: pw_thread,
        })
    }
}

impl CaptureBackend for PipeWireCapture {
    fn width(&self) -> u32 {
        self.local_width
    }

    fn height(&self) -> u32 {
        self.local_height
    }

    fn capture_frame(&mut self) -> Result<&[u8], Box<dyn std::error::Error>> {
        let frame = self.shared.lock().map_err(|e| e.to_string())?;

        // Update local dimensions if they changed
        if frame.width != self.local_width || frame.height != self.local_height {
            self.local_width = frame.width;
            self.local_height = frame.height;
            self.local_frame.resize((frame.width * frame.height * 4) as usize, 0);
        }

        self.local_frame.copy_from_slice(&frame.data);
        drop(frame);

        Ok(&self.local_frame)
    }
}

/// Use ashpd to create a screencast session via xdg-desktop-portal.
/// Returns (pipewire_fd, node_id, width, height).
async fn start_portal_session() -> Result<(std::os::fd::OwnedFd, u32, u32, u32), Box<dyn std::error::Error>>
{
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};

    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;

    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Window,
            false, // multiple
            None,  // restore_token
            None,  // persist_mode
        )
        .await?;

    let response = proxy.start(&session, None).await?;
    let streams = response.streams();

    if streams.is_empty() {
        return Err("No streams returned from portal".into());
    }

    let stream = &streams[0];
    let node_id = stream.pipe_wire_node_id();
    let (width, height) = stream.size().unwrap_or((800, 600));

    let fd = proxy.open_pipe_wire_remote(&session).await?;

    Ok((fd, node_id, width as u32, height as u32))
}

/// Run the PipeWire main loop, receiving frames and writing them to shared memory.
fn run_pipewire_loop(
    fd: std::os::fd::OwnedFd,
    node_id: u32,
    shared: Arc<Mutex<SharedFrame>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use pipewire as pw;
    use pw::spa;
    use std::os::fd::AsRawFd;

    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None)?;
    let context = pw::context::Context::new(&mainloop)?;
    let core = context.connect_fd(fd.as_raw_fd(), None)?;

    let stream = pw::stream::Stream::new(
        &core,
        "pip-viewer-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let shared_for_cb = Arc::clone(&shared);

    let _listener = stream
        .add_local_listener_with_user_data(())
        .param_changed(move |_, _user_data, id, pod| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if let Some(pod) = pod {
                // Try to parse the video format to update dimensions
                if let Ok(value) = spa::pod::deserialize::PodDeserializer::deserialize_from::<
                    spa::param::video::VideoInfoRaw,
                >(pod.as_bytes())
                {
                    let w = value.size().width;
                    let h = value.size().height;
                    if w > 0 && h > 0 {
                        if let Ok(mut frame) = shared_for_cb.lock() {
                            frame.width = w as u32;
                            frame.height = h as u32;
                            frame.data.resize((w * h * 4) as usize, 0);
                        }
                    }
                }
            }
        })
        .process(move |stream, _user_data| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &datas[0];
                if let Some(slice) = data.data() {
                    if let Ok(mut frame) = shared.lock() {
                        let expected = (frame.width * frame.height * 4) as usize;
                        if slice.len() >= expected {
                            frame.data[..expected].copy_from_slice(&slice[..expected]);
                        }
                    }
                }
            }
        })
        .register()?;

    // Build format parameters for the stream
    let format_params = build_video_params();

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut format_params.iter().collect::<Vec<_>>(),
    )?;

    mainloop.run();

    Ok(())
}

/// Build SPA video format parameters requesting BGRx format.
fn build_video_params() -> Vec<Vec<u8>> {
    use pipewire::spa;

    // Request BGRx (BGRA without alpha) which is common for screen capture
    let mut params = Vec::new();

    let mut builder = spa::pod::builder::Builder::new_bytes();
    let format = spa::param::video::VideoInfoRaw::new();

    // We'll use a minimal approach - just request any video format
    // PipeWire will negotiate the best format available
    if let Ok(pod_bytes) = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: vec![
                spa::pod::Property {
                    key: spa::format::FormatProperties::MediaType.as_raw(),
                    flags: spa::pod::PropertyFlags::empty(),
                    value: spa::pod::Value::Id(spa::utils::Id(
                        spa::param::video::MediaType::Video.as_raw(),
                    )),
                },
                spa::pod::Property {
                    key: spa::format::FormatProperties::MediaSubtype.as_raw(),
                    flags: spa::pod::PropertyFlags::empty(),
                    value: spa::pod::Value::Id(spa::utils::Id(
                        spa::param::video::MediaSubtype::Raw.as_raw(),
                    )),
                },
                spa::pod::Property {
                    key: spa::format::FormatProperties::VideoFormat.as_raw(),
                    flags: spa::pod::PropertyFlags::empty(),
                    value: spa::pod::Value::Id(spa::utils::Id(
                        spa::param::video::VideoFormat::BGRx.as_raw(),
                    )),
                },
            ],
        }),
    ) {
        params.push(pod_bytes.0.into_inner());
    }

    params
}
