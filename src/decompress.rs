#![allow(clippy::cast_possible_truncation)]

use std::{
	fs::File,
	io::{Read, Seek, SeekFrom},
};

use crate::{
	error::{ChmError, Result},
	format::{LzxcControlData, LzxcResetTable},
	lzx::LzxState,
};

const CACHE_SLOTS: usize = 5;

struct BlockCache {
	slots: Vec<Option<(u64, Vec<u8>)>>,
}

impl BlockCache {
	fn new() -> Self {
		Self { slots: vec![None; CACHE_SLOTS] }
	}

	fn get(&self, block_idx: u64) -> Option<&[u8]> {
		let slot = (block_idx as usize) % CACHE_SLOTS;
		match &self.slots[slot] {
			Some((idx, data)) if *idx == block_idx => Some(data),
			_ => None,
		}
	}

	fn insert(&mut self, block_idx: u64, data: Vec<u8>) {
		let slot = (block_idx as usize) % CACHE_SLOTS;
		self.slots[slot] = Some((block_idx, data));
	}
}

pub struct Decompressor {
	/// Absolute file offset of the compressed content stream start.
	cn_abs_start: u64,
	/// Absolute file offset of the first reset-table entry (block 0 offset).
	rt_table_abs: u64,
	reset_table: LzxcResetTable,
	reset_blkcount: u32,
	lzx: Box<LzxState>,
	lzx_last_block: Option<u64>,
	cache: BlockCache,
}

impl Decompressor {
	pub fn new(
		data_offset: u64,
		cn_unit_start: u64,
		rt_unit_start: u64,
		reset_table: LzxcResetTable,
		ctl: &LzxcControlData,
	) -> Result<Self> {
		let window_bits = ctl.window_size.trailing_zeros() as u8;
		let reset_blkcount = (ctl.reset_interval / (ctl.window_size / 2)) * ctl.windows_per_reset;
		let cn_abs_start = data_offset + cn_unit_start;
		let rt_table_abs = data_offset + rt_unit_start + u64::from(reset_table.table_offset);
		let lzx = LzxState::new(window_bits)?;
		Ok(Self {
			cn_abs_start,
			rt_table_abs,
			reset_table,
			reset_blkcount,
			lzx,
			lzx_last_block: None,
			cache: BlockCache::new(),
		})
	}

	/// Read the compressed block offset bounds from the reset table. Returns `(abs_file_start, compressed_len)`.
	fn block_bounds(&self, file: &mut File, block: u64) -> Result<(u64, u64)> {
		// Read start offset for this block.
		let mut tmp = [0u8; 8];
		file.seek(SeekFrom::Start(self.rt_table_abs + block * 8))?;
		file.read_exact(&mut tmp)?;
		let start = u64::from_le_bytes(tmp);
		// Read end offset (start of next block, or compressed_len for the last).
		let end = if block + 1 < u64::from(self.reset_table.block_count) {
			file.seek(SeekFrom::Start(self.rt_table_abs + (block + 1) * 8))?;
			file.read_exact(&mut tmp)?;
			u64::from_le_bytes(tmp)
		} else {
			self.reset_table.compressed_len
		};
		let compressed_len = end.checked_sub(start).ok_or(ChmError::Overflow)?;
		let abs_start = self.cn_abs_start + start;
		Ok((abs_start, compressed_len))
	}

	/// Decompress `block` into the cache, decompressing any predecessor blocks in the same reset window first (LZX is context-dependent).
	fn decompress_block(&mut self, file: &mut File, block: u64) -> Result<()> {
		let reset_blkcount = u64::from(self.reset_blkcount);
		let block_len = self.reset_table.block_len as usize;
		let block_align = block % reset_blkcount;
		// Optimisation: if lzx_last_block falls within [window_start, block), we can start from the block after it instead of from window_start.
		let window_start = block - block_align;
		let effective_start = match self.lzx_last_block {
			Some(last) if last >= window_start && last < block => last + 1,
			_ => window_start,
		};
		for b in effective_start..block {
			if b.is_multiple_of(reset_blkcount) {
				self.lzx.reset();
			}
			let (abs_start, clen) = self.block_bounds(file, b)?;
			let mut compressed = vec![0u8; clen as usize];
			file.seek(SeekFrom::Start(abs_start))?;
			file.read_exact(&mut compressed)?;
			// LZX is stateful: we must decompress every predecessor block in sequence to
			// advance the window/r-registers, even if the output is already cached.
			if self.cache.get(b).is_some() {
				let mut scratch = vec![0u8; block_len];
				self.lzx.decompress(&compressed, &mut scratch)?;
			} else {
				let mut decompressed = vec![0u8; block_len];
				self.lzx.decompress(&compressed, &mut decompressed)?;
				self.cache.insert(b, decompressed);
			}
			self.lzx_last_block = Some(b);
		}
		if block.is_multiple_of(reset_blkcount) {
			self.lzx.reset();
		}
		let (abs_start, clen) = self.block_bounds(file, block)?;
		let mut compressed = vec![0u8; clen as usize];
		file.seek(SeekFrom::Start(abs_start))?;
		file.read_exact(&mut compressed)?;
		let mut decompressed = vec![0u8; block_len];
		self.lzx.decompress(&compressed, &mut decompressed)?;
		self.cache.insert(block, decompressed);
		self.lzx_last_block = Some(block);
		Ok(())
	}

	/// Read `len` bytes from the compressed address space starting at `start`.
	pub fn read(&mut self, file: &mut File, start: u64, len: u64) -> Result<Vec<u8>> {
		let block_len = self.reset_table.block_len;
		let mut result = Vec::with_capacity(len as usize);
		let mut remaining = len;
		let mut pos = start;
		while remaining > 0 {
			let block = pos / block_len;
			let offset = (pos % block_len) as usize;
			let avail = (block_len - offset as u64).min(remaining) as usize;
			if self.cache.get(block).is_none() {
				self.decompress_block(file, block)?;
			}
			let data = self.cache.get(block).ok_or(ChmError::NoCompression)?;
			result.extend_from_slice(&data[offset..offset + avail]);
			pos += avail as u64;
			remaining -= avail as u64;
		}
		Ok(result)
	}
}
