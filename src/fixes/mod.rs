//! Every correction this project applies for ultrawide play, one function per
//! concern, grouped by what part of the game they touch.
//!
//! Everything here is a `fix_*`: nothing in this project is a new feature,
//! only undoing behaviour the game gets wrong once the aspect ratio leaves
//! 16:9. A file groups fixes that target the same system; within a file, only
//! the entry point each `fix_*` name promises needs to be `pub` -- helpers
//! that exist to support one fix stay private to that file.

pub mod hud;
pub mod resolution;
pub mod visibility;
