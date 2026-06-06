use crate::error::{ChmError, Result};

pub const ITSF_V2_LEN: usize = 0x58;
pub const ITSF_V3_LEN: usize = 0x60;
pub const ITSP_V1_LEN: usize = 0x54;
pub const PMGL_HEADER_LEN: usize = 0x14;
pub const PMGI_HEADER_LEN: usize = 0x08;
pub const LZXC_RESET_TABLE_LEN: usize = 0x28;
pub const LZXC_MIN_LEN: usize = 0x18;
pub const MAX_PATH_LEN: usize = 512;

#[derive(Debug, Clone)]
pub struct ItsfHeader {
	pub dir_offset: u64,
	pub data_offset: u64,
}

#[derive(Debug, Clone)]
pub struct ItspHeader {
	pub block_len: u32,
	pub index_root: i32,
	pub index_head: i32,
	pub header_len: u32,
}

#[derive(Debug, Clone)]
pub struct PmglHeader {
	pub free_space: u32,
	pub block_next: i32,
}

#[derive(Debug, Clone)]
pub struct PmgiHeader {
	pub free_space: u32,
}

#[derive(Debug, Clone)]
pub struct LzxcResetTable {
	pub block_count: u32,
	pub table_offset: u32,
	pub compressed_len: u64,
	pub block_len: u64,
}

#[derive(Debug, Clone)]
pub struct LzxcControlData {
	pub reset_interval: u32,
	pub window_size: u32,
	pub windows_per_reset: u32,
}

#[derive(Debug, Clone)]
pub struct PmglEntry {
	pub path: String,
	pub space: u8,
	pub start: u64,
	pub length: u64,
}

#[inline]
fn u32_le(b: &[u8], o: usize) -> u32 {
	u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

#[inline]
fn i32_le(b: &[u8], o: usize) -> i32 {
	i32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

#[inline]
fn u64_le(b: &[u8], o: usize) -> u64 {
	u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

pub fn parse_itsf(buf: &[u8]) -> Result<ItsfHeader> {
	if buf.len() < ITSF_V2_LEN {
		return Err(ChmError::BadItsf);
	}
	if &buf[0..4] != b"ITSF" {
		return Err(ChmError::BadItsf);
	}
	let version = u32_le(buf, 4);
	let header_len = u32_le(buf, 8);
	match version {
		2 => {
			if (header_len as usize) < ITSF_V2_LEN {
				return Err(ChmError::BadItsf);
			}
		}
		3 => {
			if (header_len as usize) < ITSF_V3_LEN {
				return Err(ChmError::BadItsf);
			}
		}
		_ => return Err(ChmError::BadItsf),
	}
	let dir_offset = u64_le(buf, 0x48);
	let dir_len = u64_le(buf, 0x50);
	let data_offset = if version == 3 {
		if buf.len() < ITSF_V3_LEN {
			return Err(ChmError::BadItsf);
		}
		u64_le(buf, 0x58)
	} else {
		dir_offset + dir_len
	};
	Ok(ItsfHeader { dir_offset, data_offset })
}

pub fn parse_itsp(buf: &[u8]) -> Result<ItspHeader> {
	if buf.len() < ITSP_V1_LEN {
		return Err(ChmError::BadItsp);
	}
	if &buf[0..4] != b"ITSP" {
		return Err(ChmError::BadItsp);
	}
	let version = u32_le(buf, 4);
	if version != 1 {
		return Err(ChmError::BadItsp);
	}
	let header_len = u32_le(buf, 8);
	if (header_len as usize) != ITSP_V1_LEN {
		return Err(ChmError::BadItsp);
	}
	Ok(ItspHeader {
		block_len: u32_le(buf, 0x10),
		index_root: i32_le(buf, 0x1c),
		index_head: i32_le(buf, 0x20),
		header_len,
	})
}

pub fn parse_pmgl(buf: &[u8]) -> Result<PmglHeader> {
	if buf.len() < PMGL_HEADER_LEN {
		return Err(ChmError::BadPmgl);
	}
	if &buf[0..4] != b"PMGL" {
		return Err(ChmError::BadPmgl);
	}
	Ok(PmglHeader { free_space: u32_le(buf, 4), block_next: i32_le(buf, 0x10) })
}

pub fn parse_pmgi(buf: &[u8]) -> Result<PmgiHeader> {
	if buf.len() < PMGI_HEADER_LEN {
		return Err(ChmError::BadPmgi);
	}
	if &buf[0..4] != b"PMGI" {
		return Err(ChmError::BadPmgi);
	}
	Ok(PmgiHeader { free_space: u32_le(buf, 4) })
}

pub fn parse_lzxc_reset_table(buf: &[u8]) -> Result<LzxcResetTable> {
	if buf.len() < LZXC_RESET_TABLE_LEN {
		return Err(ChmError::BadResetTable);
	}
	let version = u32_le(buf, 0);
	if version != 2 {
		return Err(ChmError::BadResetTable);
	}
	Ok(LzxcResetTable {
		block_count: u32_le(buf, 4),
		table_offset: u32_le(buf, 0x0c),
		compressed_len: u64_le(buf, 0x18),
		block_len: u64_le(buf, 0x20),
	})
}

pub fn parse_lzxc_control_data(buf: &[u8]) -> Result<LzxcControlData> {
	if buf.len() < LZXC_MIN_LEN {
		return Err(ChmError::BadLzxc);
	}
	if &buf[4..8] != b"LZXC" {
		return Err(ChmError::BadLzxc);
	}
	let version = u32_le(buf, 8);
	let mut reset_interval = u32_le(buf, 0x0c);
	let mut window_size = u32_le(buf, 0x10);
	let windows_per_reset = u32_le(buf, 0x14);
	if version == 2 {
		reset_interval = reset_interval.saturating_mul(0x8000);
		window_size = window_size.saturating_mul(0x8000);
	}
	if window_size == 0 || reset_interval == 0 || windows_per_reset == 0 {
		return Err(ChmError::BadLzxc);
	}
	if !window_size.is_power_of_two() {
		return Err(ChmError::BadLzxc);
	}
	if !reset_interval.is_multiple_of(window_size / 2) {
		return Err(ChmError::BadLzxc);
	}
	Ok(LzxcControlData { reset_interval, window_size, windows_per_reset })
}

/// Parse one cword from `buf` starting at byte index `offset`. Returns `(value, new_offset)`.
pub fn parse_cword(buf: &[u8], offset: usize) -> Result<(u64, usize)> {
	let mut accum: u64 = 0;
	let mut i = offset;
	loop {
		if i >= buf.len() {
			return Err(ChmError::BadPmgl);
		}
		let b = buf[i];
		i += 1;
		if b >= 0x80 {
			accum = (accum << 7) | u64::from(b & 0x7f);
		} else {
			return Ok(((accum << 7) | u64::from(b), i));
		}
	}
}

/// Parse one PMGL entry from `buf` starting at byte `offset`. Returns `(entry, new_offset)`.
pub fn parse_pmgl_entry(buf: &[u8], offset: usize) -> Result<(PmglEntry, usize)> {
	let (path_len, mut pos) = parse_cword(buf, offset)?;
	if path_len > MAX_PATH_LEN as u64 {
		return Err(ChmError::PathTooLong);
	}
	let path_len = usize::try_from(path_len).map_err(|_| ChmError::Overflow)?;
	if pos + path_len > buf.len() {
		return Err(ChmError::BadPmgl);
	}
	let path = String::from_utf8(buf[pos..pos + path_len].to_vec())?;
	pos += path_len;
	let (space, pos) = parse_cword(buf, pos)?;
	let (start, pos) = parse_cword(buf, pos)?;
	let (length, pos) = parse_cword(buf, pos)?;
	let space = u8::try_from(space).map_err(|_| ChmError::BadPmgl)?;
	Ok((PmglEntry { path, space, start, length }, pos))
}

/// Parse one PMGI entry (key + child block index) from `buf` at `offset`. Returns `(key, child_block, new_offset)`.
pub fn parse_pmgi_entry(buf: &[u8], offset: usize) -> Result<(String, i32, usize)> {
	let (path_len, mut pos) = parse_cword(buf, offset)?;
	if path_len > MAX_PATH_LEN as u64 {
		return Err(ChmError::PathTooLong);
	}
	let path_len = usize::try_from(path_len).map_err(|_| ChmError::Overflow)?;
	if pos + path_len > buf.len() {
		return Err(ChmError::BadPmgi);
	}
	let key = String::from_utf8(buf[pos..pos + path_len].to_vec())?;
	pos += path_len;
	let (child, pos) = parse_cword(buf, pos)?;
	let child = i32::try_from(child).map_err(|_| ChmError::Overflow)?;
	Ok((key, child, pos))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cword_single_byte() {
		let buf = [0x42u8];
		assert_eq!(parse_cword(&buf, 0).unwrap(), (0x42, 1));
	}

	#[test]
	fn cword_two_bytes() {
		let buf = [0x81u8, 0x00];
		assert_eq!(parse_cword(&buf, 0).unwrap(), (128, 2));
	}

	#[test]
	fn cword_zero() {
		let buf = [0x00u8];
		assert_eq!(parse_cword(&buf, 0).unwrap(), (0, 1));
	}
}
