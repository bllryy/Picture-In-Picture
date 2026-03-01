use x11rb::connection::Connection;
use x11rb::protocol::composite::{ConnectionExt as CompositeExt, Redirect};
use x11rb::protocol::shm::ConnectionExt as ShmExt;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

#[derive(Debug)]
pub enum CaptureError {
    ConnectionFailed(String),
    CompositeNotSupported,
    ShmNotSupported,
    ShmCreateFailed(String),
    CaptureFailed(String),
    WindowGone,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionFailed(e) => write!(f, "X11 connection failed: {}", e),
            Self::CompositeNotSupported => write!(f, "XComposite extension not supported"),
            Self::ShmNotSupported => write!(f, "XShm extension not supported"),
            Self::ShmCreateFailed(e) => write!(f, "Failed to create shared memory: {}", e),
            Self::CaptureFailed(e) => write!(f, "Capture failed: {}", e),
            Self::WindowGone => write!(f, "Target window no longer exists"),
        }
    }
}

impl std::error::Error for CaptureError {}

#[allow(dead_code)]
pub struct WindowCapture {
    conn: RustConnection,
    target_window: u32,
    pixmap: u32,
    shm_seg: u32,
    shm_id: i32,
    shm_ptr: *mut u8,
    width: u16,
    height: u16,
    depth: u8,
}

// SAFETY: The shared memory pointer is only accessed from one thread
unsafe impl Send for WindowCapture {}

impl WindowCapture {
    pub fn new(window_id: u32) -> Result<Self, CaptureError> {
        let (conn, screen_num) = RustConnection::connect(None)
            .map_err(|e| CaptureError::ConnectionFailed(e.to_string()))?;

        // Check for Composite extension
        conn.composite_query_version(0, 4)
            .map_err(|e| CaptureError::ConnectionFailed(e.to_string()))?
            .reply()
            .map_err(|_| CaptureError::CompositeNotSupported)?;

        // Check for SHM extension
        conn.shm_query_version()
            .map_err(|e| CaptureError::ConnectionFailed(e.to_string()))?
            .reply()
            .map_err(|_| CaptureError::ShmNotSupported)?;

        let _screen = &conn.setup().roots[screen_num];

        // Get window geometry
        let geom = conn
            .get_geometry(window_id)
            .map_err(|e| CaptureError::CaptureFailed(e.to_string()))?
            .reply()
            .map_err(|_| CaptureError::WindowGone)?;

        let width = geom.width;
        let height = geom.height;
        let depth = geom.depth;

        // Redirect window to offscreen storage
        conn.composite_redirect_window(window_id, Redirect::AUTOMATIC)
            .map_err(|e| CaptureError::CaptureFailed(e.to_string()))?;

        // Create a pixmap for the window
        let pixmap = conn
            .generate_id()
            .map_err(|e| CaptureError::ConnectionFailed(e.to_string()))?;

        conn.composite_name_window_pixmap(window_id, pixmap)
            .map_err(|e| CaptureError::CaptureFailed(e.to_string()))?;

        // Create shared memory segment for efficient capture
        let bytes_per_pixel = 4; // BGRA
        let shm_size = (width as usize) * (height as usize) * bytes_per_pixel;

        // Create SysV shared memory
        let shm_id = unsafe {
            libc::shmget(
                libc::IPC_PRIVATE,
                shm_size,
                libc::IPC_CREAT | 0o600,
            )
        };

        if shm_id < 0 {
            return Err(CaptureError::ShmCreateFailed("shmget failed".to_string()));
        }

        let shm_ptr = unsafe { libc::shmat(shm_id, std::ptr::null(), 0) };
        if shm_ptr == (-1isize) as *mut libc::c_void {
            unsafe { libc::shmctl(shm_id, libc::IPC_RMID, std::ptr::null_mut()) };
            return Err(CaptureError::ShmCreateFailed("shmat failed".to_string()));
        }

        // Attach to X server
        let shm_seg = conn
            .generate_id()
            .map_err(|e| CaptureError::ConnectionFailed(e.to_string()))?;

        conn.shm_attach(shm_seg, shm_id as u32, false)
            .map_err(|e| CaptureError::ShmCreateFailed(e.to_string()))?;

        // Mark for deletion when all processes detach
        unsafe { libc::shmctl(shm_id, libc::IPC_RMID, std::ptr::null_mut()) };

        conn.flush()
            .map_err(|e| CaptureError::ConnectionFailed(e.to_string()))?;

        Ok(Self {
            conn,
            target_window: window_id,
            pixmap,
            shm_seg,
            shm_id,
            shm_ptr: shm_ptr as *mut u8,
            width,
            height,
            depth,
        })
    }

    pub fn width(&self) -> u32 {
        self.width as u32
    }

    pub fn height(&self) -> u32 {
        self.height as u32
    }

    /// Capture current frame into shared memory, returns BGRA pixel data
    pub fn capture_frame(&mut self) -> Result<&[u8], CaptureError> {
        // Check if window still exists and get current geometry
        let geom = self
            .conn
            .get_geometry(self.target_window)
            .map_err(|e| CaptureError::CaptureFailed(e.to_string()))?
            .reply()
            .map_err(|_| CaptureError::WindowGone)?;

        // If size changed, we need to recreate the pixmap
        if geom.width != self.width || geom.height != self.height {
            self.resize(geom.width, geom.height)?;
        }

        // Update the pixmap from the window
        self.conn
            .composite_name_window_pixmap(self.target_window, self.pixmap)
            .map_err(|e| CaptureError::CaptureFailed(e.to_string()))?;

        // Use SHM to get the image
        self.conn
            .shm_get_image(
                self.pixmap,
                0,
                0,
                self.width,
                self.height,
                0xFFFFFFFF, // plane_mask
                ImageFormat::Z_PIXMAP.into(),
                self.shm_seg,
                0, // offset
            )
            .map_err(|e| CaptureError::CaptureFailed(e.to_string()))?
            .reply()
            .map_err(|e| CaptureError::CaptureFailed(e.to_string()))?;

        let size = (self.width as usize) * (self.height as usize) * 4;
        let data = unsafe { std::slice::from_raw_parts(self.shm_ptr, size) };

        Ok(data)
    }

    fn resize(&mut self, new_width: u16, new_height: u16) -> Result<(), CaptureError> {
        // Detach old SHM
        self.conn
            .shm_detach(self.shm_seg)
            .map_err(|e| CaptureError::CaptureFailed(e.to_string()))?;

        unsafe { libc::shmdt(self.shm_ptr as *const libc::c_void) };

        // Free old pixmap
        self.conn
            .free_pixmap(self.pixmap)
            .map_err(|e| CaptureError::CaptureFailed(e.to_string()))?;

        self.width = new_width;
        self.height = new_height;

        // Create new pixmap
        self.pixmap = self
            .conn
            .generate_id()
            .map_err(|e| CaptureError::ConnectionFailed(e.to_string()))?;

        self.conn
            .composite_name_window_pixmap(self.target_window, self.pixmap)
            .map_err(|e| CaptureError::CaptureFailed(e.to_string()))?;

        // Create new SHM
        let shm_size = (new_width as usize) * (new_height as usize) * 4;

        let shm_id = unsafe {
            libc::shmget(libc::IPC_PRIVATE, shm_size, libc::IPC_CREAT | 0o600)
        };

        if shm_id < 0 {
            return Err(CaptureError::ShmCreateFailed("shmget failed".to_string()));
        }

        let shm_ptr = unsafe { libc::shmat(shm_id, std::ptr::null(), 0) };
        if shm_ptr == (-1isize) as *mut libc::c_void {
            unsafe { libc::shmctl(shm_id, libc::IPC_RMID, std::ptr::null_mut()) };
            return Err(CaptureError::ShmCreateFailed("shmat failed".to_string()));
        }

        self.shm_seg = self
            .conn
            .generate_id()
            .map_err(|e| CaptureError::ConnectionFailed(e.to_string()))?;

        self.conn
            .shm_attach(self.shm_seg, shm_id as u32, false)
            .map_err(|e| CaptureError::ShmCreateFailed(e.to_string()))?;

        unsafe { libc::shmctl(shm_id, libc::IPC_RMID, std::ptr::null_mut()) };

        self.shm_id = shm_id;
        self.shm_ptr = shm_ptr as *mut u8;

        self.conn
            .flush()
            .map_err(|e| CaptureError::ConnectionFailed(e.to_string()))?;

        Ok(())
    }
}

impl Drop for WindowCapture {
    fn drop(&mut self) {
        // Detach SHM from X server
        let _ = self.conn.shm_detach(self.shm_seg);

        // Detach from our process
        unsafe { libc::shmdt(self.shm_ptr as *const libc::c_void) };

        // Free pixmap
        let _ = self.conn.free_pixmap(self.pixmap);

        // Unredirect window
        let _ = self
            .conn
            .composite_unredirect_window(self.target_window, Redirect::AUTOMATIC);

        let _ = self.conn.flush();
    }
}
