//! Error types returned by this crate.

use std::{io, result, string};

use thiserror::Error;

/// The error type for all fallible operations in this crate.
#[derive(Debug, Error)]
pub enum ChmError {
	/// An underlying I/O operation failed.
	#[error("I/O error: {0}")]
	Io(#[from] io::Error),
	/// The file does not start with a valid ITSF header.
	#[error("not a valid CHM file: bad ITSF signature or version")]
	BadItsf,
	/// The ITSP directory header is malformed.
	#[error("bad ITSP directory header")]
	BadItsp,
	/// A PMGL directory block has a bad signature or is otherwise malformed.
	#[error("bad PMGL block signature")]
	BadPmgl,
	/// A PMGI index block has a bad signature or is otherwise malformed.
	#[error("bad PMGI block signature")]
	BadPmgi,
	/// The LZXC control data block is malformed.
	#[error("bad LZXC control data")]
	BadLzxc,
	/// The LZXC reset table is malformed.
	#[error("bad LZXC reset table")]
	BadResetTable,
	/// No entry exists at the requested path.
	#[error("entry not found: {0}")]
	NotFound(String),
	/// The entry is compressed, but the file has no `MSCompressed` section to decompress it with.
	#[error("compressed data unavailable (file has no MSCompressed section)")]
	NoCompression,
	/// LZX decompression failed.
	#[error("LZX decompression error: {0}")]
	Lzx(#[from] LzxError),
	/// An entry path exceeds the maximum length allowed in a CHM directory entry (512 bytes).
	#[error("entry path exceeds maximum length")]
	PathTooLong,
	/// An entry path is not valid UTF-8.
	#[error("invalid UTF-8 in entry path")]
	Utf8(#[from] string::FromUtf8Error),
	/// A block/offset calculation would overflow the target integer type.
	#[error("integer overflow in block/offset calculation")]
	Overflow,
}

/// A specialized [`std::result::Result`] type using [`ChmError`].
pub type Result<T> = result::Result<T, ChmError>;

/// Errors specific to LZX decompression.
#[derive(Debug, Error)]
pub enum LzxError {
	/// The compressed stream contains illegal data (e.g. an out-of-range Huffman code).
	#[error("illegal data in LZX stream")]
	IllegalData,
	/// The compressed stream violates the expected LZX bitstream format.
	#[error("data format error in LZX stream")]
	DataFormat,
	/// The LZX window size is outside the valid range of 15-21 bits.
	#[error("invalid LZX window bits: must be 15-21, got {0}")]
	InvalidWindow(u8),
}
