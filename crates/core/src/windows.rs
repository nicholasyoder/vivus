//! Open windows, behind a backend interface so the standard Wayland protocols
//! and a future custom protocol look the same to the daemon.

/// A toplevel window as the compositor describes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Window {
    /// Ours, unique for the daemon's lifetime.
    pub id: u64,
    pub title: String,
    pub app_id: String,
    /// `None` when the backend can't tell (ext-foreign-toplevel-list has no state).
    pub active: Option<bool>,
    pub minimized: bool,
    pub maximized: bool,
    pub fullscreen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    /// Raise and focus, unminimizing if needed.
    Activate,
    Close,
    Minimize,
}

impl WindowAction {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Activate => "switch to",
            Self::Close => "close",
            Self::Minimize => "minimize",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    #[error("that window has closed")]
    Gone,
    #[error("this compositor doesn't let Vivus {} windows", .0.verb())]
    Unsupported(WindowAction),
    #[error("{0}")]
    Backend(String),
}

pub trait WindowBackend: Send + Sync {
    /// Open windows, most recently focused first.
    fn windows(&self) -> Vec<Window>;
    fn act(&self, id: u64, action: WindowAction) -> Result<(), WindowError>;
}
