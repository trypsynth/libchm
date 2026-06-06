use std::{io, result, string};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChmError {
	#[error("I/O error: {0}")]
	Io(#[from] io::Error),
	#[error("not a valid CHM file: bad ITSF signature or version")]
	BadItsf,
	#[error("bad ITSP directory header")]
	BadItsp,
	#[error("bad PMGL block signature")]
	BadPmgl,
	#[error("bad PMGI block signature")]
	BadPmgi,
	#[error("bad LZXC control data")]
	BadLzxc,
	#[error("bad LZXC reset table")]
	BadResetTable,
	#[error("entry not found: {0}")]
	NotFound(String),
	#[error("compressed data unavailable (file has no MSCompressed section)")]
	NoCompression,
	#[error("LZX decompression error: {0}")]
	Lzx(#[from] LzxError),
	#[error("entry path exceeds maximum length")]
	PathTooLong,
	#[error("invalid UTF-8 in entry path")]
	Utf8(#[from] string::FromUtf8Error),
	#[error("integer overflow in block/offset calculation")]
	Overflow,
}

pub type Result<T> = result::Result<T, ChmError>;

#[derive(Debug, Error)]
pub enum LzxError {
	#[error("illegal data in LZX stream")]
	IllegalData,
	#[error("data format error in LZX stream")]
	DataFormat,
	#[error("invalid LZX window bits: must be 15-21, got {0}")]
	InvalidWindow(u8),
}
