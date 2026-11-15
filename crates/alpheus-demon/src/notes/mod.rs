//! The quick notepad (quick notes): a hidden pad that `alpheus-launch --notes` toggles. The pure
//! parts live here: the text buffer, its soft wrap, the keys, the model and the view.

pub mod keys;
pub mod lines;
pub mod model;
pub mod store;
pub mod text;
pub mod view;
