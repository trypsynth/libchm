//! Binary layout of the CHM container.
//!
//! Parsers for the ITSF/ITSP headers, the PMGL/PMGI directory blocks, and the LZXC
//! control-data and reset-table structures that describe the compressed section.
//!
//! Every parser validates signatures and lengths up front, so the fixed-offset readers
//! below can rely on the buffer already being long enough.

use crate::error::{ChmError, Result};

pub const ITSF_V2_LEN: usize = 0x58;
pub const ITSF_V3_LEN: usize = 0x60;
pub const ITSP_V1_LEN: usize = 0x54;
pub const PMGL_HEADER_LEN: usize = 0x14;
pub const PMGI_HEADER_LEN: usize = 0x08;
pub const LZXC_RESET_TABLE_LEN: usize = 0x28;
pub const LZXC_MIN_LEN: usize = 0x18;
pub const MAX_PATH_LEN: usize = 512;

/// Upper bound accepted for the ITSP directory block size.
///
/// Real files always use 0x1000. The cap exists so a corrupt header cannot make us
/// allocate a multi-gigabyte block buffer.
pub const MAX_BLOCK_LEN: u32 = 0x10_0000;

/// Largest accumulator a cword may reach before the next 7-bit shift would overflow.
const MAX_CWORD_ACCUM: u64 = u64::MAX >> 7;

/// Which kind of directory block is being parsed, so the shared cword and path helpers
/// can report the matching error variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
	Pmgl,
	Pmgi,
}

impl BlockKind {
	const fn malformed(self) -> ChmError {
		match self {
			Self::Pmgl => ChmError::BadPmgl,
			Self::Pmgi => ChmError::BadPmgi,
		}
	}
}

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
	if buf.len() < ITSF_V2_LEN || &buf[0..4] != b"ITSF" {
		return Err(ChmError::BadItsf);
	}
	let version = u32_le(buf, 4);
	let header_len = u32_le(buf, 8) as usize;
	let min_len = match version {
		2 => ITSF_V2_LEN,
		3 => ITSF_V3_LEN,
		_ => return Err(ChmError::BadItsf),
	};
	if header_len < min_len {
		return Err(ChmError::BadItsf);
	}
	let dir_offset = u64_le(buf, 0x48);
	let dir_len = u64_le(buf, 0x50);
	// V3 stores the content offset explicitly; V2 has no such field, so content starts
	// immediately after the directory.
	let data_offset = if version == 3 {
		if buf.len() < ITSF_V3_LEN {
			return Err(ChmError::BadItsf);
		}
		u64_le(buf, 0x58)
	} else {
		dir_offset.checked_add(dir_len).ok_or(ChmError::Overflow)?
	};
	Ok(ItsfHeader { dir_offset, data_offset })
}

pub fn parse_itsp(buf: &[u8]) -> Result<ItspHeader> {
	if buf.len() < ITSP_V1_LEN || &buf[0..4] != b"ITSP" {
		return Err(ChmError::BadItsp);
	}
	if u32_le(buf, 4) != 1 {
		return Err(ChmError::BadItsp);
	}
	let header_len = u32_le(buf, 8);
	if (header_len as usize) != ITSP_V1_LEN {
		return Err(ChmError::BadItsp);
	}
	// A block must at least hold a PMGL header, and must stay small enough that
	// allocating a block buffer per directory walk is harmless.
	let block_len = u32_le(buf, 0x10);
	if (block_len as usize) < PMGL_HEADER_LEN || block_len > MAX_BLOCK_LEN {
		return Err(ChmError::BadItsp);
	}
	Ok(ItspHeader { block_len, index_root: i32_le(buf, 0x1c), index_head: i32_le(buf, 0x20), header_len })
}

pub fn parse_pmgl(buf: &[u8]) -> Result<PmglHeader> {
	if buf.len() < PMGL_HEADER_LEN || &buf[0..4] != b"PMGL" {
		return Err(ChmError::BadPmgl);
	}
	Ok(PmglHeader { free_space: u32_le(buf, 4), block_next: i32_le(buf, 0x10) })
}

pub fn parse_pmgi(buf: &[u8]) -> Result<PmgiHeader> {
	if buf.len() < PMGI_HEADER_LEN || &buf[0..4] != b"PMGI" {
		return Err(ChmError::BadPmgi);
	}
	Ok(PmgiHeader { free_space: u32_le(buf, 4) })
}

pub fn parse_lzxc_reset_table(buf: &[u8]) -> Result<LzxcResetTable> {
	if buf.len() < LZXC_RESET_TABLE_LEN {
		return Err(ChmError::BadResetTable);
	}
	if u32_le(buf, 0) != 2 {
		return Err(ChmError::BadResetTable);
	}
	let table = LzxcResetTable {
		block_count: u32_le(buf, 4),
		table_offset: u32_le(buf, 0x0c),
		compressed_len: u64_le(buf, 0x18),
		block_len: u64_le(buf, 0x20),
	};
	// `block_len` divides every address in the compressed space and `block_count` bounds
	// every reset-table lookup, so neither may be zero.
	if table.block_len == 0 || table.block_count == 0 {
		return Err(ChmError::BadResetTable);
	}
	Ok(table)
}

pub fn parse_lzxc_control_data(buf: &[u8]) -> Result<LzxcControlData> {
	if buf.len() < LZXC_MIN_LEN || &buf[4..8] != b"LZXC" {
		return Err(ChmError::BadLzxc);
	}
	let version = u32_le(buf, 8);
	let mut reset_interval = u32_le(buf, 0x0c);
	let mut window_size = u32_le(buf, 0x10);
	let windows_per_reset = u32_le(buf, 0x14);
	// Version 2 stores both fields in 0x8000-byte units.
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
	// Guarantees `reset_interval / (window_size / 2) >= 1`, so the reset block count the
	// decompressor derives from these fields can never be zero.
	if !reset_interval.is_multiple_of(window_size / 2) {
		return Err(ChmError::BadLzxc);
	}
	Ok(LzxcControlData { reset_interval, window_size, windows_per_reset })
}

/// Parse one cword from `buf` starting at byte index `offset`. Returns `(value, new_offset)`.
///
/// A cword is a big-endian base-128 integer: each byte contributes seven bits, and the
/// high bit marks "another byte follows".
pub fn parse_cword(buf: &[u8], offset: usize, kind: BlockKind) -> Result<(u64, usize)> {
	let mut accum: u64 = 0;
	let mut i = offset;
	loop {
		let &b = buf.get(i).ok_or_else(|| kind.malformed())?;
		i += 1;
		// Reject a run of continuation bytes long enough to shift bits off the top.
		if accum > MAX_CWORD_ACCUM {
			return Err(ChmError::Overflow);
		}
		accum = (accum << 7) | u64::from(b & 0x7f);
		if b < 0x80 {
			return Ok((accum, i));
		}
	}
}

/// Parse a cword-length-prefixed path from `buf` at `offset`. Returns `(path, new_offset)`.
fn parse_path(buf: &[u8], offset: usize, kind: BlockKind) -> Result<(String, usize)> {
	let (path_len, pos) = parse_cword(buf, offset, kind)?;
	if path_len > MAX_PATH_LEN as u64 {
		return Err(ChmError::PathTooLong);
	}
	let path_len = usize::try_from(path_len).map_err(|_| ChmError::Overflow)?;
	let end = pos.checked_add(path_len).ok_or(ChmError::Overflow)?;
	let bytes = buf.get(pos..end).ok_or_else(|| kind.malformed())?;
	Ok((String::from_utf8(bytes.to_vec())?, end))
}

/// Parse one PMGL entry from `buf` starting at byte `offset`. Returns `(entry, new_offset)`.
pub fn parse_pmgl_entry(buf: &[u8], offset: usize) -> Result<(PmglEntry, usize)> {
	let kind = BlockKind::Pmgl;
	let (path, pos) = parse_path(buf, offset, kind)?;
	let (space, pos) = parse_cword(buf, pos, kind)?;
	let (start, pos) = parse_cword(buf, pos, kind)?;
	let (length, pos) = parse_cword(buf, pos, kind)?;
	let space = u8::try_from(space).map_err(|_| ChmError::BadPmgl)?;
	Ok((PmglEntry { path, space, start, length }, pos))
}

/// Parse one PMGI entry (key + child block index) from `buf` at `offset`. Returns `(key, child_block, new_offset)`.
pub fn parse_pmgi_entry(buf: &[u8], offset: usize) -> Result<(String, i32, usize)> {
	let kind = BlockKind::Pmgi;
	let (key, pos) = parse_path(buf, offset, kind)?;
	let (child, pos) = parse_cword(buf, pos, kind)?;
	let child = i32::try_from(child).map_err(|_| ChmError::Overflow)?;
	Ok((key, child, pos))
}

#[cfg(test)]
mod tests {
	#![allow(clippy::cast_possible_truncation)]

	use super::*;

	const PMGL: BlockKind = BlockKind::Pmgl;

	fn itsf_v3() -> Vec<u8> {
		let mut buf = vec![0u8; ITSF_V3_LEN];
		buf[0..4].copy_from_slice(b"ITSF");
		buf[4..8].copy_from_slice(&3u32.to_le_bytes());
		buf[8..12].copy_from_slice(&(ITSF_V3_LEN as u32).to_le_bytes());
		buf[0x48..0x50].copy_from_slice(&0x1000u64.to_le_bytes());
		buf[0x50..0x58].copy_from_slice(&0x2000u64.to_le_bytes());
		buf[0x58..0x60].copy_from_slice(&0x4000u64.to_le_bytes());
		buf
	}

	fn itsp_v1() -> Vec<u8> {
		let mut buf = vec![0u8; ITSP_V1_LEN];
		buf[0..4].copy_from_slice(b"ITSP");
		buf[4..8].copy_from_slice(&1u32.to_le_bytes());
		buf[8..12].copy_from_slice(&(ITSP_V1_LEN as u32).to_le_bytes());
		buf[0x10..0x14].copy_from_slice(&0x1000u32.to_le_bytes());
		buf[0x1c..0x20].copy_from_slice(&1i32.to_le_bytes());
		buf[0x20..0x24].copy_from_slice(&0i32.to_le_bytes());
		buf
	}

	fn lzxc_control(version: u32, reset_interval: u32, window_size: u32, wpr: u32) -> Vec<u8> {
		let mut buf = vec![0u8; LZXC_MIN_LEN];
		buf[4..8].copy_from_slice(b"LZXC");
		buf[8..12].copy_from_slice(&version.to_le_bytes());
		buf[0x0c..0x10].copy_from_slice(&reset_interval.to_le_bytes());
		buf[0x10..0x14].copy_from_slice(&window_size.to_le_bytes());
		buf[0x14..0x18].copy_from_slice(&wpr.to_le_bytes());
		buf
	}

	fn reset_table(block_count: u32, block_len: u64) -> Vec<u8> {
		let mut buf = vec![0u8; LZXC_RESET_TABLE_LEN];
		buf[0..4].copy_from_slice(&2u32.to_le_bytes());
		buf[4..8].copy_from_slice(&block_count.to_le_bytes());
		buf[0x0c..0x10].copy_from_slice(&0x28u32.to_le_bytes());
		buf[0x18..0x20].copy_from_slice(&0x1234u64.to_le_bytes());
		buf[0x20..0x28].copy_from_slice(&block_len.to_le_bytes());
		buf
	}

	#[test]
	fn cword_single_byte() {
		assert_eq!(parse_cword(&[0x42], 0, PMGL).unwrap(), (0x42, 1));
	}

	#[test]
	fn cword_two_bytes() {
		assert_eq!(parse_cword(&[0x81, 0x00], 0, PMGL).unwrap(), (128, 2));
	}

	#[test]
	fn cword_zero() {
		assert_eq!(parse_cword(&[0x00], 0, PMGL).unwrap(), (0, 1));
	}

	#[test]
	fn cword_respects_start_offset() {
		assert_eq!(parse_cword(&[0xff, 0xff, 0x7f], 2, PMGL).unwrap(), (0x7f, 3));
	}

	#[test]
	fn cword_unterminated_is_malformed() {
		// Every byte has the continuation bit set, so the value never ends.
		assert!(matches!(parse_cword(&[0x80, 0x80], 0, PMGL), Err(ChmError::BadPmgl)));
		assert!(matches!(parse_cword(&[0x80, 0x80], 0, BlockKind::Pmgi), Err(ChmError::BadPmgi)));
	}

	#[test]
	fn cword_overlong_run_does_not_silently_wrap() {
		// Ten continuation bytes would shift the accumulator past 64 bits.
		let buf = [0xffu8; 16];
		assert!(matches!(parse_cword(&buf, 0, PMGL), Err(ChmError::Overflow)));
	}

	#[test]
	fn itsf_v3_uses_explicit_content_offset() {
		let itsf = parse_itsf(&itsf_v3()).unwrap();
		assert_eq!(itsf.dir_offset, 0x1000);
		assert_eq!(itsf.data_offset, 0x4000);
	}

	#[test]
	fn itsf_v2_derives_content_offset_from_directory() {
		let mut buf = itsf_v3();
		buf[4..8].copy_from_slice(&2u32.to_le_bytes());
		buf[8..12].copy_from_slice(&(ITSF_V2_LEN as u32).to_le_bytes());
		let itsf = parse_itsf(&buf).unwrap();
		assert_eq!(itsf.data_offset, 0x1000 + 0x2000);
	}

	#[test]
	fn itsf_v2_rejects_overflowing_content_offset() {
		let mut buf = itsf_v3();
		buf[4..8].copy_from_slice(&2u32.to_le_bytes());
		buf[8..12].copy_from_slice(&(ITSF_V2_LEN as u32).to_le_bytes());
		buf[0x48..0x50].copy_from_slice(&u64::MAX.to_le_bytes());
		buf[0x50..0x58].copy_from_slice(&1u64.to_le_bytes());
		assert!(matches!(parse_itsf(&buf), Err(ChmError::Overflow)));
	}

	#[test]
	fn itsf_rejects_bad_signature_version_and_short_buffer() {
		let mut bad_sig = itsf_v3();
		bad_sig[0] = b'X';
		assert!(matches!(parse_itsf(&bad_sig), Err(ChmError::BadItsf)));
		let mut bad_ver = itsf_v3();
		bad_ver[4..8].copy_from_slice(&4u32.to_le_bytes());
		assert!(matches!(parse_itsf(&bad_ver), Err(ChmError::BadItsf)));
		assert!(matches!(parse_itsf(&itsf_v3()[..ITSF_V2_LEN - 1]), Err(ChmError::BadItsf)));
	}

	#[test]
	fn itsp_reads_block_geometry() {
		let itsp = parse_itsp(&itsp_v1()).unwrap();
		assert_eq!(itsp.block_len, 0x1000);
		assert_eq!(itsp.index_root, 1);
		assert_eq!(itsp.index_head, 0);
		assert_eq!(itsp.header_len, ITSP_V1_LEN as u32);
	}

	#[test]
	fn itsp_rejects_absurd_block_len() {
		let mut too_small = itsp_v1();
		too_small[0x10..0x14].copy_from_slice(&0u32.to_le_bytes());
		assert!(matches!(parse_itsp(&too_small), Err(ChmError::BadItsp)));
		let mut too_big = itsp_v1();
		too_big[0x10..0x14].copy_from_slice(&(MAX_BLOCK_LEN + 1).to_le_bytes());
		assert!(matches!(parse_itsp(&too_big), Err(ChmError::BadItsp)));
	}

	#[test]
	fn lzxc_v2_scales_fields_by_frame_size() {
		let ctl = parse_lzxc_control_data(&lzxc_control(2, 2, 2, 1)).unwrap();
		assert_eq!(ctl.reset_interval, 0x10000);
		assert_eq!(ctl.window_size, 0x10000);
		assert_eq!(ctl.windows_per_reset, 1);
	}

	#[test]
	fn lzxc_rejects_non_power_of_two_window() {
		assert!(matches!(parse_lzxc_control_data(&lzxc_control(1, 0x8000, 0xc000, 1)), Err(ChmError::BadLzxc)));
	}

	#[test]
	fn lzxc_rejects_zero_fields() {
		assert!(matches!(parse_lzxc_control_data(&lzxc_control(1, 0, 0x10000, 1)), Err(ChmError::BadLzxc)));
		assert!(matches!(parse_lzxc_control_data(&lzxc_control(1, 0x8000, 0, 1)), Err(ChmError::BadLzxc)));
		assert!(matches!(parse_lzxc_control_data(&lzxc_control(1, 0x8000, 0x10000, 0)), Err(ChmError::BadLzxc)));
	}

	#[test]
	fn lzxc_reset_interval_must_divide_half_window() {
		// 0x9000 is not a multiple of 0x10000 / 2, which would give a zero reset count.
		assert!(matches!(parse_lzxc_control_data(&lzxc_control(1, 0x9000, 0x10000, 1)), Err(ChmError::BadLzxc)));
	}

	#[test]
	fn reset_table_rejects_zero_block_len_and_count() {
		assert!(matches!(parse_lzxc_reset_table(&reset_table(4, 0)), Err(ChmError::BadResetTable)));
		assert!(matches!(parse_lzxc_reset_table(&reset_table(0, 0x8000)), Err(ChmError::BadResetTable)));
		let ok = parse_lzxc_reset_table(&reset_table(4, 0x8000)).unwrap();
		assert_eq!(ok.block_count, 4);
		assert_eq!(ok.block_len, 0x8000);
		assert_eq!(ok.compressed_len, 0x1234);
	}

	#[test]
	fn pmgl_entry_round_trips() {
		// "/a.htm" in space 0 at offset 0x10, length 0x200.
		let mut buf = vec![6u8];
		buf.extend_from_slice(b"/a.htm");
		buf.extend_from_slice(&[0x00, 0x10, 0x82, 0x00]);
		let (entry, pos) = parse_pmgl_entry(&buf, 0).unwrap();
		assert_eq!(entry.path, "/a.htm");
		assert_eq!(entry.space, 0);
		assert_eq!(entry.start, 0x10);
		assert_eq!(entry.length, 0x100);
		assert_eq!(pos, buf.len());
	}

	#[test]
	fn pmgl_entry_rejects_path_running_past_block() {
		let mut buf = vec![32u8];
		buf.extend_from_slice(b"short");
		assert!(matches!(parse_pmgl_entry(&buf, 0), Err(ChmError::BadPmgl)));
	}

	#[test]
	fn pmgl_entry_rejects_oversized_path() {
		// Cword 0x8404 == 516, just past MAX_PATH_LEN.
		let buf = [0x84u8, 0x04];
		assert!(matches!(parse_pmgl_entry(&buf, 0), Err(ChmError::PathTooLong)));
	}

	#[test]
	fn pmgl_entry_rejects_non_utf8_path() {
		let buf = [2u8, 0xff, 0xfe, 0x00, 0x00, 0x00];
		assert!(matches!(parse_pmgl_entry(&buf, 0), Err(ChmError::Utf8(_))));
	}

	#[test]
	fn pmgi_entry_round_trips() {
		let mut buf = vec![3u8];
		buf.extend_from_slice(b"/a/");
		buf.push(0x05);
		let (key, child, pos) = parse_pmgi_entry(&buf, 0).unwrap();
		assert_eq!(key, "/a/");
		assert_eq!(child, 5);
		assert_eq!(pos, buf.len());
	}

	#[test]
	fn pmgl_and_pmgi_headers_check_signatures() {
		let mut block = vec![0u8; PMGL_HEADER_LEN];
		block[0..4].copy_from_slice(b"PMGL");
		block[4..8].copy_from_slice(&0x40u32.to_le_bytes());
		block[0x10..0x14].copy_from_slice(&(-1i32).to_le_bytes());
		let header = parse_pmgl(&block).unwrap();
		assert_eq!(header.free_space, 0x40);
		assert_eq!(header.block_next, -1);
		assert!(matches!(parse_pmgi(&block), Err(ChmError::BadPmgi)));
		assert!(matches!(parse_pmgl(&block[..PMGL_HEADER_LEN - 1]), Err(ChmError::BadPmgl)));
	}
}
