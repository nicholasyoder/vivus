//! Standard-protocol compositor backend (ext/wlr foreign-toplevel, data-control, xdg-activation).

mod activation;
mod clipboard;
mod windows;

pub use activation::activation_token;
pub use clipboard::Clipboard;
pub use windows::{OnFocus, Windows};
