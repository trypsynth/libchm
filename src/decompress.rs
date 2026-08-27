//! Random access into the LZX-compressed `MSCompressed` section.
//!
//! The section is split into fixed-size blocks whose compressed offsets live in a reset
//! table. LZX carries its window and match offsets across block boundaries, so reading
//! block *n* means replaying every block since the last reset point; a small cache of
//! decoded blocks keeps sequential reads from replaying the same work.

use std::{
	fs::File,
	io::{Read, Seek, SeekFrom},
};

use crate::{
	error::{ChmError, Result},
	format::{LzxcControlData, LzxcResetTable},
	lzx::LzxState,
};

/// Number of decoded blocks kept around, indexed directly by `block % CACHE_SLOTS`.
const CACHE_SLOTS: usize = 5;

/// Direct-mapped cache of decoded blocks.
struct BlockCache {
	slots: [Option<(u64, Vec<u8>)>; CACHE_SLOTS],
}

impl BlockCache {
	const fn new() -> Self {
		Self { slots: [const { None }; CACHE_SLOTS] }
	}

	// The modulus is always < CACHE_SLOTS, so it fits in a usize on every target.
	#[allow(clippy::cast_possible_truncation)]
	const fn slot(block_idx: u64) -> usize {
		(block_idx % CACHE_SLOTS as u64) as usize
	}

	fn get(&self, block_idx: u64) -> Option<&[u8]> {
		match &self.slots[Self::slot(block_idx)] {
			Some((idx, data)) if *idx == block_idx => Some(data),
			_ => None,
		}
	}

	fn contains(&self, block_idx: u64) -> bool {
		self.get(block_idx).is_some()
	}

	fn insert(&mut self, block_idx: u64, data: Vec<u8>) {
		self.slots[Self::slot(block_idx)] = Some((block_idx, data));
	}
}

pub struct Decompressor {
	/// Absolute file offset of the compressed content stream start.
	cn_abs_start: u64,
	/// Compressed start offset of every block, plus a final entry for the stream end, so
	/// block `n` spans `block_offsets[n]..block_offsets[n + 1]`.
	block_offsets: Vec<u64>,
	/// Uncompressed size of every block but (possibly) the last.
	block_len: usize,
	/// Number of blocks between LZX resets.
	reset_blkcount: u64,
	lzx: Box<LzxState>,
	lzx_last_block: Option<u64>,
	cache: BlockCache,
	/// Scratch buffer reused for reading compressed blocks.
	compressed: Vec<u8>,
}

impl Decompressor {
	/// Build a decompressor for the `MSCompressed` section.
	///
	/// `rt_entry_len` is the stored length of the reset-table entry, which bounds how many
	/// block offsets the table can legitimately contain.
	pub fn new(
		file: &mut File,
		data_offset: u64,
		cn_unit_start: u64,
		rt_unit_start: u64,
		rt_entry_len: u64,
		reset_table: &LzxcResetTable,
		ctl: &LzxcControlData,
	) -> Result<Self> {
		// `window_size` is a validated power of two, so this is its log2.
		let window_bits = u8::try_from(ctl.window_size.trailing_zeros()).map_err(|_| ChmError::BadLzxc)?;
		// A block is one LZX frame, which must fit inside the window for the frame copy
		// at the end of each `decompress` call to be meaningful.
		let block_len = usize::try_from(reset_table.block_len).map_err(|_| ChmError::Overflow)?;
		if reset_table.block_len > u64::from(ctl.window_size) {
			return Err(ChmError::BadResetTable);
		}
		// `parse_lzxc_control_data` guarantees the division is at least one, so the reset
		// interval this yields is non-zero and safe to use as a divisor.
		let reset_blkcount =
			u64::from(ctl.reset_interval / (ctl.window_size / 2)).saturating_mul(u64::from(ctl.windows_per_reset));
		debug_assert!(reset_blkcount > 0);
		// Keeping the compressed stream inside the file bounds every per-block read below
		// by a length the file can actually back.
		let cn_abs_start = data_offset + cn_unit_start;
		let stream_end = cn_abs_start.checked_add(reset_table.compressed_len).ok_or(ChmError::Overflow)?;
		if stream_end > file.metadata()?.len() {
			return Err(ChmError::BadResetTable);
		}
		let block_offsets = read_block_offsets(file, data_offset + rt_unit_start, rt_entry_len, reset_table)?;
		Ok(Self {
			cn_abs_start,
			block_offsets,
			block_len,
			reset_blkcount,
			lzx: LzxState::new(window_bits)?,
			lzx_last_block: None,
			cache: BlockCache::new(),
			compressed: Vec::new(),
		})
	}

	/// Total uncompressed size of the section, which bounds any valid read.
	const fn uncompressed_len(&self) -> u64 {
		// `block_offsets` holds one entry per block plus the end sentinel.
		(self.block_offsets.len() as u64 - 1) * self.block_len as u64
	}

	/// Read and decode one block, appending it to the cache.
	fn decode_one(&mut self, file: &mut File, block: u64) -> Result<()> {
		let idx = usize::try_from(block).map_err(|_| ChmError::Overflow)?;
		let (Some(&start), Some(&end)) = (self.block_offsets.get(idx), self.block_offsets.get(idx + 1)) else {
			return Err(ChmError::BadResetTable);
		};
		let clen = end.checked_sub(start).ok_or(ChmError::BadResetTable)?;
		let clen = usize::try_from(clen).map_err(|_| ChmError::Overflow)?;
		self.compressed.clear();
		self.compressed.resize(clen, 0);
		file.seek(SeekFrom::Start(self.cn_abs_start + start))?;
		file.read_exact(&mut self.compressed)?;
		let mut decompressed = vec![0u8; self.block_len];
		self.lzx.decompress(&self.compressed, &mut decompressed)?;
		self.cache.insert(block, decompressed);
		self.lzx_last_block = Some(block);
		Ok(())
	}

	/// Decompress `block` into the cache, replaying any predecessor blocks in the same
	/// reset window first (LZX carries state across blocks).
	fn decompress_block(&mut self, file: &mut File, block: u64) -> Result<()> {
		let window_start = block - block % self.reset_blkcount;
		// If the last block decoded already sits inside this reset window, resume from it
		// rather than replaying the whole window.
		let start = match self.lzx_last_block {
			Some(last) if last >= window_start && last < block => last + 1,
			_ => window_start,
		};
		for b in start..=block {
			if b.is_multiple_of(self.reset_blkcount) {
				self.lzx.reset();
			}
			// Every predecessor must still be decoded to advance the LZX window and match
			// offsets, even when its output is already cached and will be overwritten.
			self.decode_one(file, b)?;
		}
		Ok(())
	}

	/// Read `len` bytes from the compressed address space starting at `start`.
	pub fn read(&mut self, file: &mut File, start: u64, len: u64) -> Result<Vec<u8>> {
		let end = start.checked_add(len).ok_or(ChmError::Overflow)?;
		if end > self.uncompressed_len() {
			return Err(ChmError::BadResetTable);
		}
		let block_len = self.block_len as u64;
		let mut result = Vec::with_capacity(usize::try_from(len).map_err(|_| ChmError::Overflow)?);
		let mut pos = start;
		while pos < end {
			let block = pos / block_len;
			// Both fit in a usize: the remainder is below `self.block_len`, and the amount
			// left to copy is clamped to what remains of this block.
			let offset = usize::try_from(pos % block_len).map_err(|_| ChmError::Overflow)?;
			let remaining = usize::try_from(end - pos).unwrap_or(usize::MAX);
			let avail = (self.block_len - offset).min(remaining);
			if !self.cache.contains(block) {
				self.decompress_block(file, block)?;
			}
			let data = self.cache.get(block).ok_or(ChmError::NoCompression)?;
			result.extend_from_slice(&data[offset..offset + avail]);
			pos += avail as u64;
		}
		Ok(result)
	}
}

/// Read the reset table's array of per-block compressed offsets into memory.
///
/// The table is read once so that decoding a block does not need a pair of seeks per
/// predecessor. A trailing entry holding the total compressed length is appended, which
/// lets callers derive every block's length by subtracting adjacent entries.
fn read_block_offsets(
	file: &mut File,
	rt_abs_start: u64,
	rt_entry_len: u64,
	reset_table: &LzxcResetTable,
) -> Result<Vec<u64>> {
	let block_count = u64::from(reset_table.block_count);
	let table_offset = u64::from(reset_table.table_offset);
	// The offsets must lie inside the reset-table entry, which bounds the allocation
	// below by the size of a real entry in the file.
	let table_len = block_count.checked_mul(8).ok_or(ChmError::Overflow)?;
	let table_end = table_offset.checked_add(table_len).ok_or(ChmError::Overflow)?;
	if table_end > rt_entry_len {
		return Err(ChmError::BadResetTable);
	}
	let count = usize::try_from(block_count).map_err(|_| ChmError::Overflow)?;
	let mut raw = vec![0u8; usize::try_from(table_len).map_err(|_| ChmError::Overflow)?];
	file.seek(SeekFrom::Start(rt_abs_start + table_offset))?;
	file.read_exact(&mut raw)?;
	let mut offsets = Vec::with_capacity(count + 1);
	for chunk in raw.chunks_exact(8) {
		offsets.push(u64::from_le_bytes(chunk.try_into().unwrap()));
	}
	// The last block runs to the end of the compressed stream.
	offsets.push(reset_table.compressed_len);
	// Offsets must be non-decreasing for block lengths to make sense.
	if offsets.windows(2).any(|w| w[0] > w[1]) {
		return Err(ChmError::BadResetTable);
	}
	Ok(offsets)
}

#[cfg(test)]
mod tests {
	#![allow(clippy::cast_possible_truncation)]

	use super::*;

	#[test]
	fn cache_maps_blocks_to_slots_by_modulus() {
		let mut cache = BlockCache::new();
		cache.insert(0, vec![1, 2, 3]);
		assert_eq!(cache.get(0), Some(&[1u8, 2, 3][..]));
		assert!(cache.contains(0));
		assert!(!cache.contains(1));
	}

	#[test]
	fn cache_entry_is_evicted_by_a_colliding_block() {
		let mut cache = BlockCache::new();
		cache.insert(1, vec![1]);
		// 1 and 1 + CACHE_SLOTS share a slot, so the newer block wins.
		cache.insert(1 + CACHE_SLOTS as u64, vec![2]);
		assert!(!cache.contains(1));
		assert_eq!(cache.get(1 + CACHE_SLOTS as u64), Some(&[2u8][..]));
	}

	#[test]
	fn cache_holds_distinct_slots_simultaneously() {
		let mut cache = BlockCache::new();
		for i in 0..CACHE_SLOTS as u64 {
			cache.insert(i, vec![i as u8]);
		}
		for i in 0..CACHE_SLOTS as u64 {
			assert_eq!(cache.get(i), Some(&[i as u8][..]));
		}
	}
}
