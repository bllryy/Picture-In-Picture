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
    use ashpd::desktop::{
        PersistMode,
        screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
    };

    let proxy = Screencast::new().await?;
    let session = proxy.create_session(Default::default()).await?;

    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(SourceType::Window | SourceType::Window)
                .set_multiple(false)
                .set_persist_mode(PersistMode::DoNot),
        )
        .await?;

    let response = proxy
        .start(&session, None, Default::default())
        .await?
        .response()?;

    let streams = response.streams();

    if streams.is_empty() {
        return Err("No streams returned from portal".into());
    }

    let stream = &streams[0];
    let node_id = stream.pipe_wire_node_id();
    let (width, height) = stream.size().unwrap_or((800, 600));

    let fd = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await?;

    Ok((fd, node_id, width as u32, height as u32))
}

struct UserData {
    format: pipewire::spa::param::video::VideoInfoRaw,
}

/// Run the PipeWire main loop, receiving frames and writing them to shared memory.
fn run_pipewire_loop(
    fd: std::os::fd::OwnedFd,
    node_id: u32,
    shared: Arc<Mutex<SharedFrame>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use pipewire as pw;
    use pw::spa;

    pw::init();

    let mainloop = pw::main_loop::MainLoopBox::new(None)?;
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect_fd(fd, None)?;

    let stream = pw::stream::StreamBox::new(
        &core,
        "pip-viewer-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let data = UserData {
        format: Default::default(),
    };

    let shared_for_cb = Arc::clone(&shared);

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(move |_, user_data, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }

            let (media_type, media_subtype) =
                match spa::param::format_utils::parse_format(param) {
                    Ok(v) => v,
                    Err(_) => return,
                };

            if media_type != spa::param::format::MediaType::Video
                || media_subtype != spa::param::format::MediaSubtype::Raw
            {
                return;
            }

            user_data
                .format
                .parse(param)
                .expect("Failed to parse VideoInfoRaw");

            let w = user_data.format.size().width;
            let h = user_data.format.size().height;
            if w > 0 && h > 0 {
                if let Ok(mut frame) = shared_for_cb.lock() {
                    frame.width = w;
                    frame.height = h;
                    frame.data.resize((w * h * 4) as usize, 0);
                }
            }
        })
        .process(move |stream, _user_data| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }
                let data = &mut datas[0];
                if let Some(slice) = data.data() {
                    if let Ok(mut frame) = shared.lock() {
                        let expected = (frame.width * frame.height * 4) as usize;
                        if slice.len() >= expected && expected > 0 {
                            frame.data[..expected].copy_from_slice(&slice[..expected]);
                        }
                    }
                }
            }
        })
        .register()?;

    // Build format parameters for the stream using the spa object! macro
    let obj = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::BGRA,
            pw::spa::param::video::VideoFormat::RGBx,
            pw::spa::param::video::VideoFormat::RGBA,
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle { width: 320, height: 240 },
            pw::spa::utils::Rectangle { width: 1, height: 1 },
            pw::spa::utils::Rectangle { width: 4096, height: 4096 }
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            pw::spa::utils::Fraction { num: 30, denom: 1 },
            pw::spa::utils::Fraction { num: 0, denom: 1 },
            pw::spa::utils::Fraction { num: 60, denom: 1 }
        ),
    );

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )?
    .0
    .into_inner();

    let mut params = [spa::pod::Pod::from_bytes(&values).unwrap()];

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    mainloop.run();

    Ok(())
}
