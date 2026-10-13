//! Guppy core: filesystem access, directory sizes, jump targets and theme, with no GUI dependencies.

pub mod actions;
pub mod archive;
pub mod attrs;
pub mod config;
pub mod drives;
pub mod du;
pub mod fs;
pub mod git;
pub mod jump;
pub mod ops;
pub mod platform;
pub mod remote;
pub mod theme;

pub use config::Config;
pub use du::{Cancel, DirSize, DirSizes};
pub use fs::{Entry, EntryKind, FileClass, read_dir};
pub use jump::Jumper;
pub use theme::Theme;
