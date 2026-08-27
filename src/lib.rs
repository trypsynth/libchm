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
//! let bytes = chm.read_path("/index.html")?;
//! println!("index size: {}", bytes.len());
//! # Ok(())
//! # }
//! ```
//!
//! # Paths
//!
//! Entry paths are absolute within the archive and matched case-insensitively, so
//! `/Index.html` and `/index.html` name the same entry. Directory entries end with `/`.
//!
//! # Selecting entries
//!
//! [`EntrySel`] filters enumeration by namespace category (`NORMAL`, `SPECIAL`, `META`)
//! and by kind (`FILES`, `DIRS`). An entry matches only if the selector names both its
//! category and its kind, so use [`EntrySel::ALL`] for everything, or combine flags:
//!
//! ```no_run
//! # use libchm::{ChmFile, EntrySel};
//! # fn main() -> libchm::Result<()> {
//! # let mut chm = ChmFile::open("docs.chm")?;
//! // Ordinary content files, skipping directories and CHM-internal metadata.
//! let pages = chm.entries(EntrySel::NORMAL | EntrySel::FILES)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Robustness
//!
//! Archives are untrusted input. Every header field that feeds an allocation, an index,
//! or a divisor is validated before use, and malformed files are reported as a
//! [`ChmError`] rather than causing a panic.

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
