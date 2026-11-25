//! The clipboard, behind a backend interface like windows.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    Empty,
    Text(String),
    /// A password manager marked it secret (`x-kde-passwordManagerHint`); never read.
    Secret,
    /// Only non-text types (an image, files), listed by MIME type.
    Other(Vec<String>),
}

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("the app that owns the clipboard didn't send it in time")]
    Timeout,
    #[error("{0}")]
    Backend(String),
}

pub trait ClipboardBackend: Send + Sync {
    /// Blocks until the owning app has sent the text, or a short timeout.
    fn read(&self) -> Result<ClipboardContent, ClipboardError>;
    /// Vivus owns the clipboard from then on, until something else is copied.
    fn write(&self, text: &str) -> Result<(), ClipboardError>;
}
