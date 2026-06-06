use std::{
	fs::File,
	io::{Read, Seek, SeekFrom},
	path::Path,
};

use crate::{
	decompress::Decompressor,
	directory::{Directory, Entry, EntrySel},
	error::{ChmError, Result},
	format::{
		ITSF_V3_LEN, ITSP_V1_LEN, ItsfHeader, LZXC_RESET_TABLE_LEN, parse_itsf, parse_itsp, parse_lzxc_control_data,
		parse_lzxc_reset_table,
	},
};

// Metadata paths for the MSCompressed section
const PATH_RESET_TABLE: &str =
	"::DataSpace/Storage/MSCompressed/Transform/{7FC28940-9D31-11D0-9B27-00A0C91E9C7C}/InstanceData/ResetTable";
const PATH_CONTROL_DATA: &str = "::DataSpace/Storage/MSCompressed/ControlData";
const PATH_CONTENT: &str = "::DataSpace/Storage/MSCompressed/Content";

/// A parsed CHM archive that supports entry lookup, enumeration, and reading.
pub struct ChmFile {
	file: File,
	data_offset: u64,
	directory: Directory,
	decompressor: Option<Decompressor>,
}

impl ChmFile {
	/// Open a CHM archive at `path`.
	///
	/// # Errors
	///
	/// Returns an error if the file cannot be opened, has an invalid CHM header, or the directory structure is malformed.
	#[allow(clippy::similar_names)]
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		let mut file = File::open(path)?;
		let header_bytes = read_at(&mut file, 0, ITSF_V3_LEN)?;
		let itsf = parse_itsf(&header_bytes)?;
		let dir_bytes = read_at(&mut file, itsf.dir_offset, ITSP_V1_LEN)?;
		let itsp = parse_itsp(&dir_bytes)?;
		let directory =
			Directory::new(itsf.dir_offset, itsp.header_len, itsp.block_len, itsp.index_root, itsp.index_head);
		let decompressor = Self::load_decompressor(&mut file, &itsf, &directory)?;
		Ok(Self { file, data_offset: itsf.data_offset, directory, decompressor })
	}

	/// Try to load the `MSCompressed` decompression machinery. Returns `Ok(None)` if the file has no compressed section.
	fn load_decompressor(file: &mut File, itsf: &ItsfHeader, dir: &Directory) -> Result<Option<Decompressor>> {
		let rt_entry = match dir.find(file, PATH_RESET_TABLE) {
			Ok(e) => e,
			Err(ChmError::NotFound(_)) => return Ok(None),
			Err(e) => return Err(e),
		};
		if rt_entry.space != 0 {
			return Ok(None);
		}
		let cn_entry = match dir.find(file, PATH_CONTENT) {
			Ok(e) => e,
			Err(ChmError::NotFound(_)) => return Ok(None),
			Err(e) => return Err(e),
		};
		if cn_entry.space != 0 {
			return Ok(None);
		}
		let lzxc_entry = match dir.find(file, PATH_CONTROL_DATA) {
			Ok(e) => e,
			Err(ChmError::NotFound(_)) => return Ok(None),
			Err(e) => return Err(e),
		};
		if lzxc_entry.space != 0 {
			return Ok(None);
		}
		let rt_abs = itsf.data_offset + rt_entry.start;
		let rt_buf = read_at(file, rt_abs, LZXC_RESET_TABLE_LEN)?;
		let Ok(reset_table) = parse_lzxc_reset_table(&rt_buf) else { return Ok(None) };
		let lzxc_abs = itsf.data_offset + lzxc_entry.start;
		let lzxc_len = usize::try_from(lzxc_entry.length).map_err(|_| ChmError::Overflow)?;
		let lzxc_buf = read_at(file, lzxc_abs, lzxc_len)?;
		let Ok(ctl) = parse_lzxc_control_data(&lzxc_buf) else { return Ok(None) };
		let decomp = Decompressor::new(itsf.data_offset, cn_entry.start, rt_entry.start, reset_table, &ctl)?;
		Ok(Some(decomp))
	}

	/// Find an entry by path (case-insensitive).
	///
	/// # Errors
	///
	/// Returns [`ChmError::NotFound`] if no entry with that path exists.
	pub fn find(&mut self, path: &str) -> Result<Entry> {
		self.directory.find(&mut self.file, path)
	}

	/// Read an entire entry into memory.
	///
	/// # Errors
	///
	/// Returns an error if the entry is compressed and compression is unavailable, or if an I/O or decompression error occurs.
	pub fn read(&mut self, entry: &Entry) -> Result<Vec<u8>> {
		if entry.length == 0 {
			return Ok(Vec::new());
		}
		match entry.space {
			0 => {
				let offset = self.data_offset + entry.start;
				let len = usize::try_from(entry.length).map_err(|_| ChmError::Overflow)?;
				read_at(&mut self.file, offset, len)
			}
			1 => {
				let decomp = self.decompressor.as_mut().ok_or(ChmError::NoCompression)?;
				decomp.read(&mut self.file, entry.start, entry.length)
			}
			_ => Err(ChmError::NoCompression),
		}
	}

	/// Enumerate all entries matching `sel`.
	///
	/// # Errors
	///
	/// Returns an error if the directory structure cannot be read.
	pub fn entries(&mut self, sel: EntrySel) -> Result<Vec<Entry>> {
		self.directory.enumerate(&mut self.file, None, sel)
	}

	/// Enumerate entries whose path starts with `prefix`, matching `sel`.
	///
	/// # Errors
	///
	/// Returns an error if the directory structure cannot be read.
	pub fn entries_in(&mut self, prefix: &str, sel: EntrySel) -> Result<Vec<Entry>> {
		self.directory.enumerate(&mut self.file, Some(prefix), sel)
	}
}

fn read_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
	let mut buf = vec![0u8; len];
	file.seek(SeekFrom::Start(offset))?;
	file.read_exact(&mut buf)?;
	Ok(buf)
}
