#![warn(clippy::all, clippy::cargo, clippy::nursery, clippy::pedantic)]

mod chm;
mod decompress;
mod directory;
mod error;
mod format;
mod lzx;

pub use chm::ChmFile;
pub use directory::{Entry, EntryCategory, EntryKind, EntrySel};
pub use error::{ChmError, LzxError, Result};
