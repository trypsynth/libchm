//! LZX decompression, as used by the `MSCompressed` section of a CHM archive.
//!
//! This is the CHM dialect of LZX: a sliding-window LZ77 coder with canonical Huffman
//! trees whose code lengths are delta-coded against the previous block, three
//! most-recently-used match offsets, and an "Intel E8" call-target transform applied to
//! each finished frame.

use std::mem;

use crate::error::LzxError;

const LZX_MIN_MATCH: usize = 2;
const LZX_NUM_CHARS: usize = 256;
const LZX_PRETREE_NUM_ELEMENTS: usize = 20;
const LZX_ALIGNED_NUM_ELEMENTS: usize = 8;
const LZX_NUM_PRIMARY_LENGTHS: usize = 7;
const LZX_NUM_SECONDARY_LENGTHS: usize = 249;
const LZX_PRETREE_MAXSYMBOLS: usize = LZX_PRETREE_NUM_ELEMENTS; // 20
const LZX_PRETREE_TABLEBITS: usize = 6;
const LZX_MAINTREE_MAXSYMBOLS: usize = LZX_NUM_CHARS + 50 * 8; // 656
const LZX_MAINTREE_TABLEBITS: usize = 12;
const LZX_LENGTH_MAXSYMBOLS: usize = LZX_NUM_SECONDARY_LENGTHS + 1; // 250
const LZX_LENGTH_TABLEBITS: usize = 12;
const LZX_ALIGNED_MAXSYMBOLS: usize = LZX_ALIGNED_NUM_ELEMENTS; // 8
const LZX_ALIGNED_TABLEBITS: usize = 7;
const LZX_LENTABLE_SAFETY: usize = 64;

/// No block header has been read yet, so no block type is in effect.
const BLOCKTYPE_INVALID: u8 = 0;
const BLOCKTYPE_VERBATIM: u8 = 1;
const BLOCKTYPE_ALIGNED: u8 = 2;
const BLOCKTYPE_UNCOMPRESSED: u8 = 3;

/// The Intel E8 transform is only applied to the first 32768 frames of a stream.
const LZX_MAX_E8_FRAMES: u32 = 32768;

/// The E8 scan reads a four-byte operand after the opcode and stops short of the frame
/// end, mirroring the reference decoder's `frame_size - 10` bound.
const E8_TAIL_MARGIN: usize = 10;

static EXTRA_BITS: [u8; 51] = [
	0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15,
	16, 16, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17,
];

static POSITION_BASE: [u32; 51] = [
	0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1_024, 1_536, 2_048, 3_072, 4_096,
	6_144, 8_192, 12_288, 16_384, 24_576, 32_768, 49_152, 65_536, 98_304, 131_072, 196_608, 262_144, 393_216, 524_288,
	655_360, 786_432, 917_504, 1_048_576, 1_179_648, 1_310_720, 1_441_792, 1_572_864, 1_703_936, 1_835_008, 1_966_080,
	2_097_152,
];

const PRETREE_TABLE_SIZE: usize = (1 << LZX_PRETREE_TABLEBITS) + LZX_PRETREE_MAXSYMBOLS * 2;
const MAINTREE_TABLE_SIZE: usize = (1 << LZX_MAINTREE_TABLEBITS) + LZX_MAINTREE_MAXSYMBOLS * 2;
const LENGTH_TABLE_SIZE: usize = (1 << LZX_LENGTH_TABLEBITS) + LZX_LENGTH_MAXSYMBOLS * 2;
const ALIGNED_TABLE_SIZE: usize = (1 << LZX_ALIGNED_TABLEBITS) + LZX_ALIGNED_MAXSYMBOLS * 2;

/// The three most-recently-used match offsets (LZX's R0/R1/R2 registers).
#[derive(Debug, Clone, Copy)]
struct Offsets {
	r0: u32,
	r1: u32,
	r2: u32,
}

impl Offsets {
	const fn new() -> Self {
		Self { r0: 1, r1: 1, r2: 1 }
	}
}

/// Borrowed view of the Huffman tables in effect for the current block.
///
/// `aligned` is present only for aligned-offset blocks.
struct Trees<'a> {
	maintree_table: &'a [u16],
	maintree_len: &'a [u8],
	length_table: &'a [u16],
	length_len: &'a [u8],
	aligned: Option<(&'a [u16], &'a [u8])>,
}

pub struct LzxState {
	window: Vec<u8>,
	window_size: u32,
	window_posn: u32,
	offsets: Offsets,
	main_elements: u16,
	header_read: bool,
	block_type: u8,
	block_length: u32,
	block_remaining: u32,
	frames_read: u32,
	intel_filesize: i32,
	/// i64 to avoid wrapping for large (>2 GB) CHM decompression sequences.
	intel_curpos: i64,
	intel_started: bool,
	// Code-length tables (persist across decompress calls; deltas applied each block)
	pretree_len: [u8; LZX_PRETREE_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
	maintree_len: [u8; LZX_MAINTREE_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
	length_len: [u8; LZX_LENGTH_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
	aligned_len: [u8; LZX_ALIGNED_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
	// Fast-lookup decode tables (rebuilt at the start of each block)
	pretree_table: [u16; PRETREE_TABLE_SIZE],
	maintree_table: [u16; MAINTREE_TABLE_SIZE],
	length_table: [u16; LENGTH_TABLE_SIZE],
	aligned_table: [u16; ALIGNED_TABLE_SIZE],
}

impl LzxState {
	pub fn new(window_bits: u8) -> Result<Box<Self>, LzxError> {
		if !(15..=21).contains(&window_bits) {
			return Err(LzxError::InvalidWindow(window_bits));
		}
		let window_size = 1u32 << window_bits;
		let posn_slots: u16 = match window_bits {
			20 => 42,
			21 => 50,
			n => u16::from(n) * 2,
		};
		let main_elements = 256u16 + posn_slots * 8;
		let state = Self {
			window: vec![0u8; window_size as usize],
			window_size,
			window_posn: 0,
			offsets: Offsets::new(),
			main_elements,
			header_read: false,
			block_type: BLOCKTYPE_INVALID,
			block_length: 0,
			block_remaining: 0,
			frames_read: 0,
			intel_filesize: 0,
			intel_curpos: 0,
			intel_started: false,
			pretree_len: [0; LZX_PRETREE_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
			maintree_len: [0; LZX_MAINTREE_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
			length_len: [0; LZX_LENGTH_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
			aligned_len: [0; LZX_ALIGNED_MAXSYMBOLS + LZX_LENTABLE_SAFETY],
			pretree_table: [0; PRETREE_TABLE_SIZE],
			maintree_table: [0; MAINTREE_TABLE_SIZE],
			length_table: [0; LENGTH_TABLE_SIZE],
			aligned_table: [0; ALIGNED_TABLE_SIZE],
		};
		Ok(Box::new(state))
	}

	/// Return the stream to its initial state, as required at every LZX reset interval.
	///
	/// The window contents are deliberately left alone: nothing can reference them until
	/// they have been rewritten, because `window_posn` restarts at zero.
	pub fn reset(&mut self) {
		self.offsets = Offsets::new();
		self.header_read = false;
		self.frames_read = 0;
		self.block_remaining = 0;
		self.block_type = BLOCKTYPE_INVALID;
		self.intel_curpos = 0;
		self.intel_started = false;
		self.window_posn = 0;
		self.maintree_len.fill(0);
		self.length_len.fill(0);
	}

	/// Read the main and length trees that follow a verbatim or aligned block header.
	///
	/// The main tree is transmitted in two halves (literals, then match symbols) and each
	/// half is delta-coded against the lengths still held from the previous block.
	fn read_main_and_length_trees(&mut self, bits: &mut Bits) -> Result<(), LzxError> {
		read_lens(bits, &mut self.pretree_len, &mut self.pretree_table, &mut self.maintree_len, 0, LZX_NUM_CHARS)?;
		read_lens(
			bits,
			&mut self.pretree_len,
			&mut self.pretree_table,
			&mut self.maintree_len,
			LZX_NUM_CHARS,
			self.main_elements as usize,
		)?;
		make_decode_table(
			LZX_MAINTREE_MAXSYMBOLS,
			LZX_MAINTREE_TABLEBITS,
			&self.maintree_len,
			&mut self.maintree_table,
		)?;
		// A coded 0xE8 literal means the encoder used the Intel call transform.
		if self.maintree_len[0xE8] != 0 {
			self.intel_started = true;
		}
		read_lens(
			bits,
			&mut self.pretree_len,
			&mut self.pretree_table,
			&mut self.length_len,
			0,
			LZX_NUM_SECONDARY_LENGTHS,
		)?;
		make_decode_table(LZX_LENGTH_MAXSYMBOLS, LZX_LENGTH_TABLEBITS, &self.length_len, &mut self.length_table)
	}

	/// Read the header of the next block and set up its Huffman tables.
	// `bits.read(3)` yields at most 7, so narrowing the block type to u8 is lossless.
	#[allow(clippy::cast_possible_truncation)]
	fn start_block(&mut self, bits: &mut Bits, input: &[u8]) -> Result<(), LzxError> {
		if self.block_type == BLOCKTYPE_UNCOMPRESSED {
			// Re-align to word boundary after an uncompressed block
			if self.block_length & 1 != 0 {
				bits.skip_byte();
			}
			bits.reinit();
		}
		let block_type = bits.read(3) as u8;
		let blen = (bits.read(16) << 8) | bits.read(8);
		self.block_type = block_type;
		self.block_length = blen;
		self.block_remaining = blen;
		match block_type {
			BLOCKTYPE_ALIGNED => {
				for slot in &mut self.aligned_len[..LZX_ALIGNED_NUM_ELEMENTS] {
					*slot = bits.read(3) as u8;
				}
				make_decode_table(
					LZX_ALIGNED_MAXSYMBOLS,
					LZX_ALIGNED_TABLEBITS,
					&self.aligned_len,
					&mut self.aligned_table,
				)?;
				// Aligned blocks carry the main and length trees too.
				self.read_main_and_length_trees(bits)
			}
			BLOCKTYPE_VERBATIM => self.read_main_and_length_trees(bits),
			BLOCKTYPE_UNCOMPRESSED => {
				self.intel_started = true;
				// Align bitstream to 16-bit boundary
				bits.ensure(16);
				if bits.left > 16 {
					bits.pos -= 2;
				}
				let raw = input.get(bits.pos..bits.pos + 12).ok_or(LzxError::IllegalData)?;
				self.offsets = Offsets {
					r0: u32::from_le_bytes(raw[0..4].try_into().unwrap()),
					r1: u32::from_le_bytes(raw[4..8].try_into().unwrap()),
					r2: u32::from_le_bytes(raw[8..12].try_into().unwrap()),
				};
				bits.pos += 12;
				Ok(())
			}
			_ => Err(LzxError::IllegalData),
		}
	}

	/// Undo the Intel E8 call transform over a finished frame, in place.
	///
	/// The encoder rewrote the operand of every `call rel32` to an absolute target; this
	/// walks the frame and converts each one back to a displacement.
	// The rewritten displacement is bounded by `intel_filesize`, which is itself an i32.
	#[allow(clippy::cast_possible_truncation)]
	fn undo_intel_e8(&mut self, output: &mut [u8], frame_size: i32) {
		let curpos_start = self.intel_curpos;
		self.intel_curpos = curpos_start + i64::from(frame_size);
		// Frames too short to hold an opcode plus operand have nothing to undo.
		if !self.intel_started || output.len() <= E8_TAIL_MARGIN {
			return;
		}
		let mut curpos = curpos_start;
		let filesize = self.intel_filesize;
		let end = output.len() - E8_TAIL_MARGIN;
		let mut i = 0usize;
		while i < end {
			if output[i] != 0xE8 {
				i += 1;
				curpos += 1;
				continue;
			}
			let abs_off = i32::from_le_bytes(output[i + 1..i + 5].try_into().unwrap());
			if i64::from(abs_off) >= -curpos && abs_off < filesize {
				let rel_off = if abs_off >= 0 { (i64::from(abs_off) - curpos) as i32 } else { abs_off + filesize };
				output[i + 1..i + 5].copy_from_slice(&rel_off.to_le_bytes());
			}
			i += 5;
			curpos += 5;
		}
	}

	// Remaining casts are bounded by LZX spec invariants:
	//   - block sizes fit in i32 (LZX blocks <= 65535 bytes)
	//   - window_posn/window_size are u32 -> usize (lossless on all >=32-bit targets)
	//   - bits.read(N) as u8 with N <= 4 (max value 15, fits in u8)
	//   - this_run bounded by block_remaining (u32) and togo (i32 <= 65535)
	#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
	pub fn decompress(&mut self, input: &[u8], output: &mut [u8]) -> Result<(), LzxError> {
		let out_len = output.len();
		let out_len_i32 = i32::try_from(out_len).map_err(|_| LzxError::DataFormat)?;
		let mut bits = Bits::new(input);
		let mut togo = out_len_i32;
		if !self.header_read {
			// An optional 32-bit field giving the size of the original Intel executable.
			let filesize = if bits.read(1) == 0 { 0 } else { (bits.read(16) << 16) | bits.read(16) };
			self.intel_filesize = filesize.cast_signed();
			self.header_read = true;
		}
		let ws = self.window_size as usize;
		while togo > 0 {
			if self.block_remaining == 0 {
				self.start_block(&mut bits, input)?;
			}
			if bits.pos > input.len() + 2 || (bits.pos > input.len() && bits.left < 16) {
				return Err(LzxError::IllegalData);
			}
			// togo > 0 in this branch, so cast_unsigned is safe; min produces u32 <= block_remaining
			let this_run = self.block_remaining.min(togo.cast_unsigned()) as usize;
			togo -= this_run as i32;
			self.block_remaining -= this_run as u32;
			// Wrap to the start of the window; a run may not straddle the wraparound.
			self.window_posn &= self.window_size - 1;
			let wp = self.window_posn as usize;
			if wp + this_run > ws {
				return Err(LzxError::DataFormat);
			}
			match self.block_type {
				BLOCKTYPE_VERBATIM | BLOCKTYPE_ALIGNED => {
					let trees = Trees {
						maintree_table: &self.maintree_table,
						maintree_len: &self.maintree_len,
						length_table: &self.length_table,
						length_len: &self.length_len,
						aligned: (self.block_type == BLOCKTYPE_ALIGNED)
							.then_some((&self.aligned_table[..], &self.aligned_len[..])),
					};
					decode_matches(&mut bits, &mut self.window, wp, this_run, ws, &mut self.offsets, &trees)?;
				}
				BLOCKTYPE_UNCOMPRESSED => {
					let raw = input.get(bits.pos..bits.pos + this_run).ok_or(LzxError::IllegalData)?;
					self.window[wp..wp + this_run].copy_from_slice(raw);
					bits.pos += this_run;
				}
				_ => return Err(LzxError::IllegalData),
			}
			self.window_posn += this_run as u32;
		}
		// A window_posn of zero means the run ended exactly on the wraparound.
		let final_pos = if self.window_posn == 0 { ws } else { self.window_posn as usize };
		let src_start = final_pos.checked_sub(out_len).ok_or(LzxError::DataFormat)?;
		output.copy_from_slice(&self.window[src_start..final_pos]);
		if self.frames_read < LZX_MAX_E8_FRAMES && self.intel_filesize != 0 {
			self.undo_intel_e8(output, out_len_i32);
		}
		self.frames_read += 1;
		Ok(())
	}
}

struct Bits<'a> {
	buf: u32,
	left: i32,
	src: &'a [u8],
	pos: usize,
}

impl<'a> Bits<'a> {
	const fn new(src: &'a [u8]) -> Self {
		Self { buf: 0, left: 0, src, pos: 0 }
	}

	const fn reinit(&mut self) {
		self.buf = 0;
		self.left = 0;
	}

	const fn skip_byte(&mut self) {
		self.pos += 1;
	}

	/// Fill the bit buffer until it holds at least `n` bits. Pads zeros at end-of-input (matches C reference).
	#[inline]
	fn ensure(&mut self, n: i32) {
		while self.left < n {
			if self.pos + 1 >= self.src.len() {
				if self.pos >= self.src.len() {
					self.left += 16;
					continue;
				}
				let w = u32::from(self.src[self.pos]);
				// left is always >= 0 here (we only reach this after left < n with n > 0)
				let shift = 16u32.saturating_sub(self.left.cast_unsigned());
				self.buf |= w << shift;
				self.left += 8;
				self.pos += 1;
				continue;
			}
			let w = u32::from(self.src[self.pos]) | (u32::from(self.src[self.pos + 1]) << 8);
			let shift = 16u32.saturating_sub(self.left.cast_unsigned());
			self.buf |= w << shift;
			self.left += 16;
			self.pos += 2;
		}
	}

	#[inline]
	const fn peek(&self, n: i32) -> u32 {
		// n is always a small positive count (1-16); cast_unsigned is safe
		self.buf >> (32 - n.cast_unsigned())
	}

	#[inline]
	const fn remove(&mut self, n: i32) {
		// n is always a small positive count (1-16); cast_unsigned is safe
		self.buf = self.buf.wrapping_shl(n.cast_unsigned());
		self.left -= n;
	}

	/// Read `n` bits. `n` must be in 1..=16; reading zero bits would shift by the full
	/// width of the buffer.
	#[inline]
	fn read(&mut self, n: i32) -> u32 {
		debug_assert!((1..=16).contains(&n), "Bits::read out of range: {n}");
		self.ensure(n);
		let v = self.peek(n);
		self.remove(n);
		v
	}

	// tablebits is always <= LZX_MAINTREE_TABLEBITS (12): fits in both i32 and u32
	#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
	fn read_huffsym(
		&mut self,
		table: &[u16],
		lens: &[u8],
		tablebits: usize,
		maxsymbols: usize,
	) -> Result<usize, LzxError> {
		self.ensure(16);
		let mut i = table[self.peek(tablebits as i32) as usize] as usize;
		// Codes longer than `tablebits` continue down a bit-by-bit tree past the table.
		if i >= maxsymbols {
			let mut j = 1u32 << (32 - tablebits as u32 - 1);
			loop {
				i <<= 1;
				if self.buf & j != 0 {
					i |= 1;
				}
				j >>= 1;
				if j == 0 {
					return Err(LzxError::IllegalData);
				}
				i = *table.get(i).ok_or(LzxError::IllegalData)? as usize;
				if i < maxsymbols {
					break;
				}
			}
		}
		let sym = i;
		let code_len = i32::from(lens[sym]);
		self.remove(code_len);
		Ok(sym)
	}
}

// Remaining casts are bounded by construction:
//   - sym < nsyms <= LZX_MAINTREE_MAXSYMBOLS (656), fits in u16
//   - next_symbol < table.len() / 2 (guarded by the ns2+1 check), fits in u16
//   - pos/table_mask <= 1<<nbits <= 1<<16, fits in u32
//   - bit_mask <= 1<<15, fits in u32
#[allow(clippy::cast_possible_truncation)]
fn make_decode_table(nsyms: usize, nbits: usize, lengths: &[u8], table: &mut [u16]) -> Result<(), LzxError> {
	let table_mask = 1usize << nbits;
	let mut bit_mask = table_mask >> 1;
	let mut next_symbol = bit_mask; // base for long-code tree allocation
	let mut pos: usize = 0;
	let mut bit_num = 1usize;
	// Codes short enough to index the table directly get a run of identical entries.
	while bit_num <= nbits {
		for (sym, &len) in lengths.iter().enumerate().take(nsyms) {
			if len as usize == bit_num {
				let leaf = pos;
				pos += bit_mask;
				if pos > table_mask {
					return Err(LzxError::IllegalData);
				}
				for entry in &mut table[leaf..leaf + bit_mask] {
					*entry = sym as u16;
				}
			}
		}
		bit_mask >>= 1;
		bit_num += 1;
	}
	if pos != table_mask {
		for entry in &mut table[pos..table_mask] {
			*entry = 0;
		}
		// Longer codes are spilled into a binary tree stored after the direct table.
		let mut pos32 = (pos as u32) << 16;
		let table_mask32 = (table_mask as u32) << 16;
		bit_mask = 1 << 15;
		while bit_num <= 16 {
			for (sym, &len) in lengths.iter().enumerate().take(nsyms) {
				if len as usize == bit_num {
					let mut leaf = (pos32 >> 16) as usize;
					for fill in 0..(bit_num - nbits) {
						if table[leaf] == 0 {
							let ns2 = next_symbol << 1;
							if ns2 + 1 >= table.len() {
								return Err(LzxError::IllegalData);
							}
							table[ns2] = 0;
							table[ns2 + 1] = 0;
							table[leaf] = next_symbol as u16;
							next_symbol += 1;
						}
						leaf = (table[leaf] as usize) << 1;
						if (pos32 >> (15 - fill as u32)) & 1 != 0 {
							leaf += 1;
						}
					}
					table[leaf] = sym as u16;
					pos32 += bit_mask as u32;
					if pos32 > table_mask32 {
						return Err(LzxError::IllegalData);
					}
				}
			}
			bit_mask >>= 1;
			bit_num += 1;
		}
		// An incomplete table is only legal when no symbol is actually coded.
		if pos32 != table_mask32 {
			for &len in lengths.iter().take(nsyms) {
				if len != 0 {
					return Err(LzxError::IllegalData);
				}
			}
		}
	}
	Ok(())
}

// Remaining casts are bounded by construction:
//   - bits.read(N) as u8 with N <= 4: max value 15, fits in u8
//   - bits.read(N) as usize: u32 -> usize (lossless on all >=32-bit platforms)
//   - z/sym as i32: values <= LZX_PRETREE_MAXSYMBOLS (20), fit in i32
//   - .rem_euclid(17) as u8: result in [0, 16], fits in u8
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
fn read_lens(
	bits: &mut Bits,
	pretree_len: &mut [u8],
	pretree_table: &mut [u16],
	lens: &mut [u8],
	first: usize,
	last: usize,
) -> Result<(), LzxError> {
	for slot in &mut pretree_len[..LZX_PRETREE_NUM_ELEMENTS] {
		*slot = bits.read(4) as u8;
	}
	make_decode_table(LZX_PRETREE_MAXSYMBOLS, LZX_PRETREE_TABLEBITS, pretree_len, pretree_table)?;
	let mut x = first;
	while x < last {
		// Symbols 17-19 are run-length escapes; anything else is a delta against the
		// length this slot already holds from the previous block.
		let z = bits.read_huffsym(pretree_table, pretree_len, LZX_PRETREE_TABLEBITS, LZX_PRETREE_MAXSYMBOLS)?;
		match z {
			17 => {
				let run = bits.read(4) as usize + 4;
				let end = (x + run).min(last);
				lens[x..end].fill(0);
				x = end;
			}
			18 => {
				let run = bits.read(5) as usize + 20;
				let end = (x + run).min(last);
				lens[x..end].fill(0);
				x = end;
			}
			19 => {
				let run = bits.read(1) as usize + 4;
				let sym =
					bits.read_huffsym(pretree_table, pretree_len, LZX_PRETREE_TABLEBITS, LZX_PRETREE_MAXSYMBOLS)?;
				let val = (i32::from(lens[x]) - sym as i32).rem_euclid(17) as u8;
				let end = (x + run).min(last);
				lens[x..end].fill(val);
				x = end;
			}
			_ => {
				lens[x] = (i32::from(lens[x]) - z as i32).rem_euclid(17) as u8;
				x += 1;
			}
		}
	}
	Ok(())
}

/// Decode `this_run` bytes of literals and matches into `window` starting at `wp`.
///
/// `window_size` is a power of two, so the window index wraps with a mask rather than a
/// division. `wp + this_run <= window_size` is guaranteed by the caller.
// Remaining casts are bounded by construction:
//   - main_element < LZX_NUM_CHARS (256) when cast to u8
//   - aligned_bits < LZX_ALIGNED_MAXSYMBOLS (8) when cast to u32
#[allow(clippy::cast_possible_truncation)]
fn decode_matches(
	bits: &mut Bits,
	window: &mut [u8],
	mut wp: usize,
	mut this_run: usize,
	window_size: usize,
	offsets: &mut Offsets,
	trees: &Trees,
) -> Result<(), LzxError> {
	let window_mask = window_size - 1;
	while this_run > 0 {
		let main_element = bits.read_huffsym(
			trees.maintree_table,
			trees.maintree_len,
			LZX_MAINTREE_TABLEBITS,
			LZX_MAINTREE_MAXSYMBOLS,
		)?;
		if main_element < LZX_NUM_CHARS {
			window[wp] = main_element as u8;
			wp += 1;
			this_run -= 1;
			continue;
		}
		// Match symbols pack a length footer and an offset slot into one element.
		let me = main_element - LZX_NUM_CHARS;
		let mut match_length = me & LZX_NUM_PRIMARY_LENGTHS;
		if match_length == LZX_NUM_PRIMARY_LENGTHS {
			match_length +=
				bits.read_huffsym(trees.length_table, trees.length_len, LZX_LENGTH_TABLEBITS, LZX_LENGTH_MAXSYMBOLS)?;
		}
		match_length += LZX_MIN_MATCH;
		let match_offset_slot = me >> 3;
		let match_offset = match match_offset_slot {
			// Slots 0-2 replay a recently used offset, promoting it to most recent.
			0 => offsets.r0,
			1 => {
				mem::swap(&mut offsets.r0, &mut offsets.r1);
				offsets.r0
			}
			2 => {
				mem::swap(&mut offsets.r0, &mut offsets.r2);
				offsets.r0
			}
			_ => {
				let offset = read_match_offset(bits, match_offset_slot, trees.aligned)?;
				offsets.r2 = offsets.r1;
				offsets.r1 = offsets.r0;
				offsets.r0 = offset;
				offset
			}
		};
		let match_offset = match_offset as usize;
		if match_offset == 0 || match_offset > window_size {
			return Err(LzxError::IllegalData);
		}
		// A match may not run past the end of the window.
		if wp + match_length > window_size {
			return Err(LzxError::DataFormat);
		}
		// src == wp - match_offset at every iteration, so no separate counter is needed.
		for _ in 0..match_length {
			window[wp] = window[wp.wrapping_sub(match_offset) & window_mask];
			wp += 1;
		}
		this_run = this_run.saturating_sub(match_length);
	}
	Ok(())
}

/// Decode an explicit match offset for `slot` (always > 2).
///
/// Aligned-offset blocks split the low three bits into their own Huffman tree; verbatim
/// blocks read every extra bit literally.
// Aligned symbols are below LZX_ALIGNED_MAXSYMBOLS (8), so they fit in a u32.
#[allow(clippy::cast_possible_truncation)]
fn read_match_offset(bits: &mut Bits, slot: usize, aligned: Option<(&[u16], &[u8])>) -> Result<u32, LzxError> {
	let extra = i32::from(EXTRA_BITS[slot]);
	let base = POSITION_BASE[slot] - 2;
	let Some((table, lens)) = aligned else {
		// EXTRA_BITS[3] is 0 and POSITION_BASE[3] - 2 is 1, so slot 3 reads nothing.
		return Ok(if extra == 0 { base } else { base + bits.read(extra) });
	};
	Ok(match extra {
		0 => base,
		1..=2 => base + bits.read(extra),
		3 => base + bits.read_huffsym(table, lens, LZX_ALIGNED_TABLEBITS, LZX_ALIGNED_MAXSYMBOLS)? as u32,
		_ => {
			let verbatim_bits = bits.read(extra - 3);
			let aligned_bits = bits.read_huffsym(table, lens, LZX_ALIGNED_TABLEBITS, LZX_ALIGNED_MAXSYMBOLS)?;
			base + (verbatim_bits << 3) + aligned_bits as u32
		}
	})
}

#[cfg(test)]
mod tests {
	#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

	use super::*;

	#[test]
	fn window_bits_outside_15_to_21_are_rejected() {
		for bits in [0u8, 14, 22, 255] {
			assert!(matches!(LzxState::new(bits), Err(LzxError::InvalidWindow(b)) if b == bits));
		}
	}

	#[test]
	fn window_bits_in_range_are_accepted() {
		for bits in 15..=21u8 {
			let state = LzxState::new(bits).unwrap();
			assert_eq!(state.window_size, 1 << bits);
			assert_eq!(state.window.len(), 1 << bits);
		}
	}

	#[test]
	fn main_elements_follow_the_position_slot_table() {
		// Below 20 bits the slot count is 2 per window bit; 20 and 21 are special-cased.
		assert_eq!(LzxState::new(16).unwrap().main_elements, 256 + 32 * 8);
		assert_eq!(LzxState::new(20).unwrap().main_elements, 256 + 42 * 8);
		assert_eq!(LzxState::new(21).unwrap().main_elements, 256 + 50 * 8);
	}

	#[test]
	fn reset_restores_initial_stream_state() {
		let mut state = LzxState::new(16).unwrap();
		state.window_posn = 0x1234;
		state.offsets = Offsets { r0: 9, r1: 8, r2: 7 };
		state.header_read = true;
		state.intel_started = true;
		state.intel_curpos = 500;
		state.block_type = BLOCKTYPE_ALIGNED;
		state.maintree_len[0] = 5;
		state.length_len[0] = 5;
		state.reset();
		assert_eq!(state.window_posn, 0);
		assert_eq!((state.offsets.r0, state.offsets.r1, state.offsets.r2), (1, 1, 1));
		assert!(!state.header_read);
		assert!(!state.intel_started);
		assert_eq!(state.intel_curpos, 0);
		assert_eq!(state.block_type, BLOCKTYPE_INVALID);
		assert_eq!(state.maintree_len[0], 0);
		assert_eq!(state.length_len[0], 0);
	}

	#[test]
	fn decode_table_rejects_oversubscribed_lengths() {
		// Three one-bit codes cannot fit in a two-entry table.
		let lengths = [1u8, 1, 1];
		let mut table = [0u16; 64];
		assert!(matches!(make_decode_table(3, 4, &lengths, &mut table), Err(LzxError::IllegalData)));
	}

	#[test]
	fn decode_table_accepts_a_complete_table() {
		// Two one-bit codes exactly fill the table.
		let mut lengths = [0u8; LZX_ALIGNED_MAXSYMBOLS + LZX_LENTABLE_SAFETY];
		lengths[0] = 1;
		lengths[1] = 1;
		let mut table = [0u16; ALIGNED_TABLE_SIZE];
		make_decode_table(LZX_ALIGNED_MAXSYMBOLS, LZX_ALIGNED_TABLEBITS, &lengths, &mut table).unwrap();
		let half = 1 << (LZX_ALIGNED_TABLEBITS - 1);
		assert_eq!(table[0], 0);
		assert_eq!(table[half], 1);
	}

	#[test]
	fn decode_table_accepts_an_all_zero_table() {
		// No symbol is coded at all, which is legal and leaves the table empty.
		let lengths = [0u8; LZX_ALIGNED_MAXSYMBOLS + LZX_LENTABLE_SAFETY];
		let mut table = [0u16; ALIGNED_TABLE_SIZE];
		make_decode_table(LZX_ALIGNED_MAXSYMBOLS, LZX_ALIGNED_TABLEBITS, &lengths, &mut table).unwrap();
	}

	#[test]
	fn bits_reads_msb_first_across_16_bit_words() {
		// Little-endian word 0xF00F, so the bit buffer holds 1111_0000_0000_1111.
		let mut bits = Bits::new(&[0x0F, 0xF0]);
		assert_eq!(bits.read(4), 0xF);
		assert_eq!(bits.read(8), 0x00);
		assert_eq!(bits.read(4), 0xF);
	}

	#[test]
	fn bits_pads_with_zeros_past_end_of_input() {
		let mut bits = Bits::new(&[]);
		assert_eq!(bits.read(16), 0);
	}

	#[test]
	fn verbatim_offset_slot_3_consumes_no_bits() {
		let mut bits = Bits::new(&[0xFF, 0xFF]);
		assert_eq!(read_match_offset(&mut bits, 3, None).unwrap(), 1);
		assert_eq!(bits.left, 0, "slot 3 has no extra bits");
	}

	#[test]
	fn verbatim_offset_slot_adds_extra_bits_to_base() {
		// Slot 4 has one extra bit; the top bit of 0xFFFF is set, so it reads as 1.
		let mut bits = Bits::new(&[0xFF, 0xFF]);
		assert_eq!(read_match_offset(&mut bits, 4, None).unwrap(), POSITION_BASE[4] - 2 + 1);
	}

	#[test]
	fn intel_transform_skips_frames_too_short_to_scan() {
		// Frames of 7 to 10 bytes have no room for an opcode plus operand; the reference
		// decoder walks a zero-length range rather than scanning them.
		let mut state = LzxState::new(16).unwrap();
		state.intel_filesize = 0x1000;
		state.intel_started = true;
		for size in 0..=E8_TAIL_MARGIN {
			state.intel_curpos = 0;
			let mut frame = vec![0xE8u8; size];
			let expected = frame.clone();
			state.undo_intel_e8(&mut frame, size as i32);
			assert_eq!(frame, expected, "frame of {size} bytes must be left alone");
			assert_eq!(state.intel_curpos, size as i64);
		}
	}

	#[test]
	fn intel_transform_rewrites_call_targets() {
		let mut state = LzxState::new(16).unwrap();
		state.intel_filesize = 0x1000;
		state.intel_started = true;
		state.intel_curpos = 0;
		// An E8 at offset 0 with absolute target 0x100 becomes the displacement 0x100.
		let mut frame = vec![0u8; 32];
		frame[0] = 0xE8;
		frame[1..5].copy_from_slice(&0x100i32.to_le_bytes());
		state.undo_intel_e8(&mut frame, 32);
		assert_eq!(i32::from_le_bytes(frame[1..5].try_into().unwrap()), 0x100);
		assert_eq!(state.intel_curpos, 32);
	}

	#[test]
	fn intel_transform_leaves_out_of_range_targets_alone() {
		let mut state = LzxState::new(16).unwrap();
		state.intel_filesize = 0x100;
		state.intel_started = true;
		state.intel_curpos = 0;
		// 0x4000 is past the end of the original executable, so it is not a call target.
		let mut frame = vec![0u8; 32];
		frame[0] = 0xE8;
		frame[1..5].copy_from_slice(&0x4000i32.to_le_bytes());
		state.undo_intel_e8(&mut frame, 32);
		assert_eq!(i32::from_le_bytes(frame[1..5].try_into().unwrap()), 0x4000);
	}

	#[test]
	fn intel_transform_is_inert_until_an_e8_literal_is_coded() {
		let mut state = LzxState::new(16).unwrap();
		state.intel_filesize = 0x1000;
		state.intel_started = false;
		state.intel_curpos = 0;
		let mut frame = vec![0u8; 32];
		frame[0] = 0xE8;
		frame[1..5].copy_from_slice(&0x100i32.to_le_bytes());
		state.undo_intel_e8(&mut frame, 32);
		assert_eq!(i32::from_le_bytes(frame[1..5].try_into().unwrap()), 0x100);
		assert_eq!(state.intel_curpos, 32);
	}

	#[test]
	fn matches_wrap_around_the_start_of_the_window() {
		// A match whose source lies before position 0 must read from the window tail.
		let window_size = 1usize << 15;
		let mut window = vec![0u8; window_size];
		window[window_size - 2] = 0xAA;
		window[window_size - 1] = 0xBB;
		let mask = window_size - 1;
		let wp = 0usize;
		let match_offset = 2usize;
		let src = wp.wrapping_sub(match_offset) & mask;
		assert_eq!(window[src], 0xAA);
		assert_eq!(window[(src + 1) & mask], 0xBB);
	}
}
