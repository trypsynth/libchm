//! A pure-Rust reader for CHM (Compiled HTML Help) archives.
//!
//! [`ChmFile`] opens a `.chm` file, parses its ITSF/ITSP header and PMGL/PMGI directory,
//! and lets you look up entries by path, enumerate them, and read their (possibly
//! LZX-compressed) contents.
//!
//! ```no_run
//! use libchm::{ChmFile, EntrySel};
//!
//! # fn main() -> libchm::Result<()> {
//! let mut chm = ChmFile::open("docs.chm")?;
//! for entry in chm.entries(EntrySel::ALL)? {
//!     println!("{}", entry.path);
//! }
//! let entry = chm.find("/index.html")?;
//! let bytes = chm.read(&entry)?;
//! println!("index size: {}", bytes.len());
//! # Ok(())
//! # }
//! ```

#![warn(clippy::all, clippy::cargo, clippy::nursery, clippy::pedantic)]
#![warn(missing_docs)]

mod chm;
mod decompress;
mod directory;
mod error;
mod format;
mod lzx;

pub use chm::ChmFile;
pub use directory::{Entry, EntryCategory, EntryKind, EntrySel};
pub use error::{ChmError, LzxError, Result};
