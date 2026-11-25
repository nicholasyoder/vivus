//! Standard-protocol compositor backend (ext/wlr foreign-toplevel, data-control, xdg-activation).

mod activation;
mod windows;

pub use activation::activation_token;
pub use windows::{OnFocus, Windows};
