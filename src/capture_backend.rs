pub trait CaptureBackend {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    /// Capture current frame, returns BGRA pixel data
    fn capture_frame(&mut self) -> Result<&[u8], Box<dyn std::error::Error>>;
}
