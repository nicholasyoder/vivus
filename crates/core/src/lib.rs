//! Pipeline, orchestration, config and tool registry.

pub mod config;
pub mod documents;
pub mod state;
pub mod windows;

pub use config::Config;
pub use documents::{Document, DocumentStore};
pub use state::AssistantState;
pub use windows::{Window, WindowAction, WindowBackend, WindowError};
