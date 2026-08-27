//! The CHM directory: a B-tree of PMGI index blocks over a linked list of PMGL leaf
//! blocks, each holding length-prefixed entries sorted by path.

use std::{
	cmp::Ordering,
	fs::File,
	io::{Read, Seek, SeekFrom},
};

use bitflags::bitflags;

use crate::{
	error::{ChmError, Result},
	format::{
		ItspHeader, PMGI_HEADER_LEN, PMGL_HEADER_LEN, PmglEntry, parse_pmgi, parse_pmgi_entry, parse_pmgl,
		parse_pmgl_entry,
	},
};

/// Deepest PMGI index chain that will be followed before declaring the tree malformed.
///
/// Real directories are two or three levels deep; the limit only exists so a block that
/// points back at itself cannot spin forever.
const MAX_INDEX_DEPTH: u32 = 64;

bitflags! {
	/// Filter flags for CHM entry enumeration.
	///
	/// A selector must name both a category (`NORMAL`, `SPECIAL`, `META`) and a kind
	/// (`FILES`, `DIRS`) for an entry to match, so `NORMAL | FILES` yields ordinary
	/// files while `NORMAL` alone yields nothing.
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

/// Whether an [`Entry`] is a file or a directory, based on whether its path ends with `/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
	/// A regular file entry.
	File,
	/// A directory entry (its path ends with `/`).
	Dir,
}

/// Which part of the CHM namespace an [`Entry`] belongs to, based on its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryCategory {
	/// A user-visible entry: path starts with `/` but not `/#` or `/$`.
	Normal,
	/// A special entry: path starts with `/#` or `/$`.
	Special,
	/// An internal metadata entry: path does not start with `/`.
	Meta,
}

/// A single entry in a CHM archive's directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
	/// The entry's path within the archive, as stored in the directory.
	pub path: String,
	/// The entry's uncompressed length in bytes.
	pub length: u64,
	pub(crate) start: u64,
	pub(crate) space: u8,
	/// Whether this entry is a file or a directory.
	pub kind: EntryKind,
	/// Which part of the namespace this entry belongs to.
	pub category: EntryCategory,
}

impl Entry {
	fn from_pmgl(e: PmglEntry) -> Self {
		let kind = classify_kind(&e.path);
		let category = classify_category(&e.path);
		Self { path: e.path, length: e.length, start: e.start, space: e.space, kind, category }
	}

	/// Whether this entry is a directory.
	#[must_use]
	pub const fn is_dir(&self) -> bool {
		matches!(self.kind, EntryKind::Dir)
	}

	/// Whether this entry is a regular file.
	#[must_use]
	pub const fn is_file(&self) -> bool {
		matches!(self.kind, EntryKind::File)
	}

	/// Whether this entry is stored compressed inside the `MSCompressed` section.
	#[must_use]
	pub const fn is_compressed(&self) -> bool {
		self.space != 0
	}

	/// The selector bits this entry matches.
	fn sel_bits(&self) -> EntrySel {
		let kind_bit = match self.kind {
			EntryKind::File => EntrySel::FILES,
			EntryKind::Dir => EntrySel::DIRS,
		};
		let cat_bit = match self.category {
			EntryCategory::Normal => EntrySel::NORMAL,
			EntryCategory::Special => EntrySel::SPECIAL,
			EntryCategory::Meta => EntrySel::META,
		};
		kind_bit | cat_bit
	}
}

fn classify_kind(path: &str) -> EntryKind {
	if path.ends_with('/') { EntryKind::Dir } else { EntryKind::File }
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

/// Order two paths the way CHM sorts its directory: byte-wise, ignoring ASCII case.
///
/// Comparing lowercased bytes lazily matches what the reference implementation gets from
/// `strcasecmp`, without allocating a lowercased copy of either side.
fn cmp_ignore_ascii_case(a: &str, b: &str) -> Ordering {
	a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// Whether `path` begins with `prefix`, ignoring ASCII case.
fn starts_with_ignore_ascii_case(path: &str, prefix: &str) -> bool {
	path.len() >= prefix.len() && path.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

#[derive(Debug)]
pub struct Directory {
	/// Byte offset of the first PMGL block (after ITSP header).
	dir_offset: u64,
	block_len: usize,
	index_root: i32,
	index_head: i32,
}

impl Directory {
	pub fn new(dir_offset: u64, itsp: &ItspHeader) -> Self {
		Self {
			dir_offset: dir_offset + u64::from(itsp.header_len),
			// Validated by `parse_itsp` to be within `MAX_BLOCK_LEN`, so this is lossless.
			block_len: itsp.block_len as usize,
			// If index_root == -1 there are no PMGI blocks; use index_head as root.
			index_root: if itsp.index_root < 0 { itsp.index_head } else { itsp.index_root },
			index_head: itsp.index_head,
		}
	}

	fn fetch_block(&self, file: &mut File, idx: i32, buf: &mut [u8]) -> Result<()> {
		if idx < 0 {
			return Err(ChmError::BadPmgl);
		}
		let offset = self.dir_offset + u64::from(idx.cast_unsigned()) * self.block_len as u64;
		file.seek(SeekFrom::Start(offset))?;
		file.read_exact(buf)?;
		Ok(())
	}

	/// The end of the used region of a block, given the free space its header reports.
	fn used_end(&self, free_space: u32, malformed: ChmError) -> Result<usize> {
		let free = usize::try_from(free_space).map_err(|_| ChmError::Overflow)?;
		self.block_len.checked_sub(free).ok_or(malformed)
	}

	/// Find an entry by exact path (case-insensitive).
	pub fn find(&self, file: &mut File, path: &str) -> Result<Entry> {
		let mut buf = vec![0u8; self.block_len];
		let mut cur = self.index_root;
		for _ in 0..MAX_INDEX_DEPTH {
			self.fetch_block(file, cur, &mut buf)?;
			if buf.starts_with(b"PMGL") {
				return self.scan_pmgl(&buf, path)?.ok_or_else(|| ChmError::NotFound(path.to_owned()));
			}
			if !buf.starts_with(b"PMGI") {
				return Err(ChmError::BadPmgl);
			}
			cur = self.descend_pmgi(&buf, path)?;
			if cur < 0 {
				return Err(ChmError::NotFound(path.to_owned()));
			}
		}
		// Deeper than any real directory, so the index blocks must reference each other.
		Err(ChmError::BadPmgi)
	}

	/// Scan a PMGL leaf block for `path`. Returns `Ok(None)` if not found.
	fn scan_pmgl(&self, buf: &[u8], target: &str) -> Result<Option<Entry>> {
		let header = parse_pmgl(buf)?;
		let end = self.used_end(header.free_space, ChmError::BadPmgl)?;
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
		let end = self.used_end(header.free_space, ChmError::BadPmgi)?;
		let mut pos = PMGI_HEADER_LEN;
		let mut last_child: i32 = -1;
		while pos < end {
			let (key, child, next_pos) = parse_pmgi_entry(buf, pos)?;
			// Keys are sorted, so the last key not past the target owns the subtree.
			if cmp_ignore_ascii_case(&key, target) == Ordering::Greater {
				return Ok(last_child);
			}
			last_child = child;
			pos = next_pos;
		}
		Ok(last_child)
	}

	/// Walk all PMGL leaf blocks and return matching entries.
	pub fn enumerate(&self, file: &mut File, prefix: Option<&str>, sel: EntrySel) -> Result<Vec<Entry>> {
		// Directory paths are stored with a trailing slash, so `/a` and `/a/` select the
		// same subtree.
		let prefix = prefix.map(|p| if p.is_empty() || p.ends_with('/') { p.to_owned() } else { format!("{p}/") });
		let mut entries = Vec::new();
		let mut buf = vec![0u8; self.block_len];
		let mut cur = self.index_head;
		while cur >= 0 {
			self.fetch_block(file, cur, &mut buf)?;
			let header = parse_pmgl(&buf)?;
			let end = self.used_end(header.free_space, ChmError::BadPmgl)?;
			let mut pos = PMGL_HEADER_LEN;
			while pos < end {
				let (pmgl_entry, next_pos) = parse_pmgl_entry(&buf, pos)?;
				pos = next_pos;
				if prefix.as_deref().is_some_and(|p| !starts_with_ignore_ascii_case(&pmgl_entry.path, p)) {
					continue;
				}
				let entry = Entry::from_pmgl(pmgl_entry);
				if sel.contains(entry.sel_bits()) {
					entries.push(entry);
				}
			}
			// Leaf blocks are chained in file order. Requiring the chain to advance keeps
			// a corrupt `block_next` from looping forever.
			if header.block_next >= 0 && header.block_next <= cur {
				return Err(ChmError::BadPmgl);
			}
			cur = header.block_next;
		}
		Ok(entries)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn categories_follow_the_path_prefix() {
		assert_eq!(classify_category("/index.html"), EntryCategory::Normal);
		assert_eq!(classify_category("/"), EntryCategory::Normal);
		assert_eq!(classify_category("/#IDXHDR"), EntryCategory::Special);
		assert_eq!(classify_category("/$OBJINST"), EntryCategory::Special);
		assert_eq!(classify_category("::DataSpace/NameList"), EntryCategory::Meta);
		assert_eq!(classify_category(""), EntryCategory::Meta);
	}

	#[test]
	fn kind_follows_the_trailing_slash() {
		assert_eq!(classify_kind("/a/b.html"), EntryKind::File);
		assert_eq!(classify_kind("/a/"), EntryKind::Dir);
		assert_eq!(classify_kind(""), EntryKind::File);
	}

	fn entry(path: &str) -> Entry {
		Entry::from_pmgl(PmglEntry { path: path.to_owned(), space: 0, start: 0, length: 0 })
	}

	#[test]
	fn selector_requires_both_a_category_and_a_kind() {
		let file = entry("/index.html");
		assert!(EntrySel::ALL.contains(file.sel_bits()));
		assert!((EntrySel::NORMAL | EntrySel::FILES).contains(file.sel_bits()));
		// A category with no kind (or vice versa) matches nothing.
		assert!(!EntrySel::NORMAL.contains(file.sel_bits()));
		assert!(!EntrySel::FILES.contains(file.sel_bits()));
		// Wrong kind, right category.
		assert!(!(EntrySel::NORMAL | EntrySel::DIRS).contains(file.sel_bits()));
	}

	#[test]
	fn selector_bits_cover_every_category_and_kind() {
		assert_eq!(entry("/a.html").sel_bits(), EntrySel::NORMAL | EntrySel::FILES);
		assert_eq!(entry("/a/").sel_bits(), EntrySel::NORMAL | EntrySel::DIRS);
		assert_eq!(entry("/#SYSTEM").sel_bits(), EntrySel::SPECIAL | EntrySel::FILES);
		assert_eq!(entry("::DataSpace/NameList").sel_bits(), EntrySel::META | EntrySel::FILES);
		assert_eq!(entry("::DataSpace/").sel_bits(), EntrySel::META | EntrySel::DIRS);
	}

	#[test]
	fn entry_predicates_match_kind_and_space() {
		let dir = entry("/a/");
		assert!(dir.is_dir() && !dir.is_file());
		let file = entry("/a.html");
		assert!(file.is_file() && !file.is_dir());
		assert!(!file.is_compressed());
		let compressed = Entry::from_pmgl(PmglEntry { path: "/a".into(), space: 1, start: 0, length: 0 });
		assert!(compressed.is_compressed());
	}

	#[test]
	fn case_insensitive_ordering_matches_a_lowercased_comparison() {
		assert_eq!(cmp_ignore_ascii_case("ABC", "abc"), Ordering::Equal);
		assert_eq!(cmp_ignore_ascii_case("/Index.HTML", "/index.html"), Ordering::Equal);
		assert_eq!(cmp_ignore_ascii_case("/a", "/b"), Ordering::Less);
		assert_eq!(cmp_ignore_ascii_case("/B", "/a"), Ordering::Greater);
		// A prefix sorts before the longer string that extends it.
		assert_eq!(cmp_ignore_ascii_case("/a", "/ab"), Ordering::Less);
		// Lowercasing both sides moves the boundary: underscore (0x5F) sorts after a raw
		// uppercase 'A' (0x41) but before the lowercase 'a' (0x61) it is compared against.
		assert_eq!(cmp_ignore_ascii_case("_", "A"), Ordering::Less);
		assert_eq!(cmp_ignore_ascii_case("_", "a"), Ordering::Less);
	}

	#[test]
	fn case_insensitive_prefix_matching() {
		assert!(starts_with_ignore_ascii_case("/Docs/a.html", "/docs/"));
		assert!(starts_with_ignore_ascii_case("/docs/", "/docs/"));
		assert!(starts_with_ignore_ascii_case("/anything", ""));
		assert!(!starts_with_ignore_ascii_case("/docs", "/docs/"));
		assert!(!starts_with_ignore_ascii_case("/other/a.html", "/docs/"));
	}
}
