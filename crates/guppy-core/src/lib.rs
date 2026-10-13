//! Guppy core: filesystem access, directory sizes, jump targets and theme, with no GUI dependencies.

pub mod config;
pub mod du;
pub mod fs;
pub mod jump;
pub mod ops;
pub mod platform;
pub mod theme;

pub use config::Config;
pub use du::{Cancel, DirSize, DirSizes};
pub use fs::{Entry, EntryKind, FileClass, read_dir};
pub use jump::Jumper;
pub use theme::Theme;
