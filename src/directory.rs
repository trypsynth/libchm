#![allow(clippy::cast_possible_truncation)]

use std::{
	fs::File,
	io::{Read, Seek, SeekFrom},
};

use bitflags::bitflags;

use crate::{
	error::{ChmError, Result},
	format::{PMGI_HEADER_LEN, PMGL_HEADER_LEN, PmglEntry, parse_pmgi, parse_pmgi_entry, parse_pmgl, parse_pmgl_entry},
};

bitflags! {
	/// Filter flags for CHM entry enumeration.
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	pub struct EntrySel: u8 {
		/// Paths starting with `/` but not `/#` or `/$`.
		const NORMAL  = 0x01;
		/// Paths starting with `/#` or `/$`.
		const SPECIAL = 0x02;
		/// Paths not starting with `/` (internal metadata).
		const META    = 0x04;
		/// Non-directory entries (path does not end with `/`).
		const FILES   = 0x08;
		/// Directory entries (path ends with `/`).
		const DIRS    = 0x10;
		/// All entries.
		const ALL     = 0x1F;
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
	File,
	Dir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryCategory {
	Normal,
	Special,
	Meta,
}

#[derive(Debug, Clone)]
pub struct Entry {
	pub path: String,
	pub length: u64,
	pub(crate) start: u64,
	pub(crate) space: u8,
	pub kind: EntryKind,
	pub category: EntryCategory,
}

impl Entry {
	pub(crate) fn from_pmgl(e: PmglEntry) -> Self {
		let kind = if e.path.ends_with('/') { EntryKind::Dir } else { EntryKind::File };
		let category = classify_category(&e.path);
		Self { path: e.path, length: e.length, start: e.start, space: e.space, kind, category }
	}
}

fn classify_category(path: &str) -> EntryCategory {
	let bytes = path.as_bytes();
	if bytes.first() != Some(&b'/') {
		return EntryCategory::Meta;
	}
	match bytes.get(1) {
		Some(&b'#' | &b'$') => EntryCategory::Special,
		_ => EntryCategory::Normal,
	}
}

fn entry_sel_bits(kind: EntryKind, category: EntryCategory) -> EntrySel {
	let kind_bit = match kind {
		EntryKind::File => EntrySel::FILES,
		EntryKind::Dir => EntrySel::DIRS,
	};
	let cat_bit = match category {
		EntryCategory::Normal => EntrySel::NORMAL,
		EntryCategory::Special => EntrySel::SPECIAL,
		EntryCategory::Meta => EntrySel::META,
	};
	kind_bit | cat_bit
}

pub struct Directory {
	/// Byte offset of the first PMGL block (after ITSP header).
	dir_offset: u64,
	block_len: u32,
	index_root: i32,
	index_head: i32,
}

impl Directory {
	pub fn new(dir_offset: u64, itsp_header_len: u32, block_len: u32, index_root: i32, index_head: i32) -> Self {
		let dir_offset = dir_offset + u64::from(itsp_header_len);
		// If index_root == -1 there are no PMGI blocks; use index_head as root.
		let index_root = if index_root < 0 { index_head } else { index_root };
		Self { dir_offset, block_len, index_root, index_head }
	}

	fn fetch_block(&self, file: &mut File, idx: i32, buf: &mut [u8]) -> Result<()> {
		if idx < 0 {
			return Err(ChmError::BadPmgl);
		}
		let offset = self.dir_offset + u64::from(idx.cast_unsigned()) * u64::from(self.block_len);
		file.seek(SeekFrom::Start(offset))?;
		file.read_exact(buf)?;
		Ok(())
	}

	/// Find an entry by exact path (case-insensitive).
	pub fn find(&self, file: &mut File, path: &str) -> Result<Entry> {
		let mut buf = vec![0u8; self.block_len as usize];
		let mut cur = self.index_root;
		loop {
			self.fetch_block(file, cur, &mut buf)?;
			if buf.starts_with(b"PMGL") {
				return self.scan_pmgl(&buf, path).and_then(|e| e.ok_or_else(|| ChmError::NotFound(path.to_owned())));
			} else if buf.starts_with(b"PMGI") {
				cur = self.descend_pmgi(&buf, path)?;
				if cur < 0 {
					return Err(ChmError::NotFound(path.to_owned()));
				}
			} else {
				return Err(ChmError::BadPmgl);
			}
		}
	}

	/// Scan a PMGL leaf block for `path`. Returns `Ok(None)` if not found.
	fn scan_pmgl(&self, buf: &[u8], target: &str) -> Result<Option<Entry>> {
		let header = parse_pmgl(buf)?;
		let end = (self.block_len as usize)
			.checked_sub(header.free_space as usize)
			.ok_or(ChmError::BadPmgl)?;
		let mut pos = PMGL_HEADER_LEN;
		while pos < end {
			let (entry, next_pos) = parse_pmgl_entry(buf, pos)?;
			if entry.path.eq_ignore_ascii_case(target) {
				return Ok(Some(Entry::from_pmgl(entry)));
			}
			pos = next_pos;
		}
		Ok(None)
	}

	/// Walk a PMGI index block to find which child block to descend into. Returns the child block index, or -1 if none.
	fn descend_pmgi(&self, buf: &[u8], target: &str) -> Result<i32> {
		let header = parse_pmgi(buf)?;
		let end = (self.block_len as usize)
			.checked_sub(header.free_space as usize)
			.ok_or(ChmError::BadPmgi)?;
		let mut pos = PMGI_HEADER_LEN;
		let mut last_child: i32 = -1;
		while pos < end {
			let (key, child, next_pos) = parse_pmgi_entry(buf, pos)?;
			if key.to_ascii_lowercase() > target.to_ascii_lowercase() {
				return Ok(last_child);
			}
			last_child = child;
			pos = next_pos;
		}
		Ok(last_child)
	}

	/// Walk all PMGL leaf blocks and return matching entries.
	pub fn enumerate(&self, file: &mut File, prefix: Option<&str>, sel: EntrySel) -> Result<Vec<Entry>> {
		let prefix_norm: Option<String> = prefix.map(|p| {
			let p = p.to_ascii_lowercase();
			if !p.is_empty() && !p.ends_with('/') { format!("{p}/") } else { p }
		});
		let mut entries = Vec::new();
		let mut buf = vec![0u8; self.block_len as usize];
		let mut cur = self.index_head;
		while cur >= 0 {
			self.fetch_block(file, cur, &mut buf)?;
			let header = parse_pmgl(&buf)?;
			let end = (self.block_len as usize)
				.checked_sub(header.free_space as usize)
				.ok_or(ChmError::BadPmgl)?;
			let mut pos = PMGL_HEADER_LEN;
			while pos < end {
				let (pmgl_entry, next_pos) = parse_pmgl_entry(&buf, pos)?;
				pos = next_pos;
				if prefix_norm.as_deref().is_some_and(|pfx| !pmgl_entry.path.to_ascii_lowercase().starts_with(pfx)) {
					continue;
				}
				let kind = if pmgl_entry.path.ends_with('/') { EntryKind::Dir } else { EntryKind::File };
				let category = classify_category(&pmgl_entry.path);
				let bits = entry_sel_bits(kind, category);
				if sel.contains(bits) {
					entries.push(Entry::from_pmgl(pmgl_entry));
				}
			}
			cur = header.block_next;
		}
		Ok(entries)
	}
}
