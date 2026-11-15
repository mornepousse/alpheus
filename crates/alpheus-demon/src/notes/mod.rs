//! The quick notepad (quick notes): a hidden pad that `alpheus-launch --notes` toggles. The pure
//! parts live here: the text buffer, its soft wrap, the keys, the model and the view; then the
//! glue (`app`) and the Wayland side (`surface`).

pub mod app;
pub mod keys;
pub mod lines;
pub mod model;
pub mod paste;
pub mod store;
pub mod surface;
pub mod text;
pub mod view;
