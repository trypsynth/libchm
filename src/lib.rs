#![warn(clippy::all, clippy::cargo, clippy::nursery, clippy::pedantic)]
use std::{
	ffi::{CStr, CString},
	mem,
	os::raw::{c_char, c_int, c_longlong, c_uchar, c_void},
	path::Path,
	result,
};

use thiserror::Error;

unsafe extern "C" {
	fn chm_open(filename: *const c_char) -> *mut ChmFile;
	fn chm_close(file: *mut ChmFile);
	fn chm_enumerate(file: *mut ChmFile, what: c_int, callback: ChmEnumerateCallback, context: *mut c_void) -> c_int;
	fn chm_resolve_object(file: *mut ChmFile, path: *const c_char, ui: *mut ChmUnitInfo) -> c_int;
	fn chm_retrieve_object(
		file: *mut ChmFile,
		ui: *const ChmUnitInfo,
		buf: *mut c_uchar,
		addr: c_longlong,
		len: c_longlong,
	) -> c_longlong;
}

#[repr(C)]
/// Opaque file handle provided by libchm.
pub struct ChmFile {
	_private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Clone)]
/// Metadata describing an entry inside a CHM archive.
pub struct ChmUnitInfo {
	pub start: c_longlong,
	pub length: c_longlong,
	pub space: c_int,
	pub flags: c_int,
	pub path: [c_char; 512],
}

pub type ChmEnumerateCallback = extern "C" fn(*mut ChmFile, *mut ChmUnitInfo, *mut c_void) -> c_int;

/// Enumerate every file and directory.
pub const CHM_ENUMERATE_ALL: c_int = 3;
/// Continue enumerating after a callback.
pub const CHM_ENUMERATOR_CONTINUE: c_int = 1;
/// Stop enumerating after a callback.
pub const CHM_ENUMERATOR_SUCCESS: c_int = 0;
/// Successful resolution of a CHM object.
pub const CHM_RESOLVE_SUCCESS: c_int = 0;

#[derive(Debug, Error)]
/// Errors produced by the CHM wrapper.
pub enum ChmError {
	#[error("Invalid path for CHM file: {0}")]
	InvalidPath(String),
	#[error("Failed to open CHM file: {0}")]
	OpenFailed(String),
	#[error("CHM enumeration failed")]
	EnumerateFailed,
	#[error("Failed to resolve CHM object: {0}")]
	ResolveFailed(String),
	#[error("Failed to read complete CHM file (expected {expected} bytes, got {actual})")]
	ShortRead { expected: i64, actual: i64 },
	#[error("CHM object length overflows usize: {0}")]
	LengthOverflow(i64),
}

pub type Result<T> = result::Result<T, ChmError>;

/// Safe wrapper around a libchm file handle.
#[derive(Debug)]
pub struct ChmHandle {
	handle: *mut ChmFile,
}

impl ChmHandle {
	/// Open a CHM file at `path`.
	///
	/// # Errors
	///
	/// Returns [`ChmError::InvalidPath`] if `path` contains interior null bytes, or
	/// [`ChmError::OpenFailed`] if the underlying `chm_open` call fails.
	pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
		let path_str = path.as_ref().to_string_lossy().to_string();
		let c_path = CString::new(path_str.as_str()).map_err(|_| ChmError::InvalidPath(path_str.clone()))?;
		unsafe {
			let handle = chm_open(c_path.as_ptr());
			if handle.is_null() {
				return Err(ChmError::OpenFailed(path_str));
			}
			Ok(Self { handle })
		}
	}

	/// Enumerate entries using the supplied callback.
	///
	/// # Errors
	///
	/// Returns [`ChmError::EnumerateFailed`] if the underlying `chm_enumerate` call fails.
	pub fn enumerate<F>(&mut self, what: c_int, mut callback: F) -> Result<()>
	where
		F: FnMut(&ChmUnitInfo) -> bool,
	{
		extern "C" fn trampoline<F>(_file: *mut ChmFile, ui: *mut ChmUnitInfo, context: *mut c_void) -> c_int
		where
			F: FnMut(&ChmUnitInfo) -> bool,
		{
			unsafe {
				let cb: &mut F = &mut *context.cast::<F>();
				if cb(&*ui) { CHM_ENUMERATOR_CONTINUE } else { CHM_ENUMERATOR_SUCCESS }
			}
		}
		unsafe {
			let context = (&raw mut callback).cast::<c_void>();
			let result = chm_enumerate(self.handle, what, trampoline::<F>, context);
			if result != 0 { Ok(()) } else { Err(ChmError::EnumerateFailed) }
		}
	}

	/// Read an entire file from the archive into memory.
	///
	/// # Errors
	///
	/// Returns [`ChmError::InvalidPath`] if `path` contains interior null bytes,
	/// [`ChmError::ResolveFailed`] if the object cannot be found in the archive,
	/// [`ChmError::LengthOverflow`] if the file length overflows `usize`, or
	/// [`ChmError::ShortRead`] if fewer bytes than expected were retrieved.
	pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
		let c_path = CString::new(path).map_err(|_| ChmError::InvalidPath(path.to_string()))?;
		unsafe {
			let mut ui: ChmUnitInfo = mem::zeroed();
			if chm_resolve_object(self.handle, c_path.as_ptr(), &raw mut ui) != CHM_RESOLVE_SUCCESS {
				return Err(ChmError::ResolveFailed(path.to_string()));
			}
			if ui.length == 0 {
				return Ok(Vec::new());
			}
			let len = usize::try_from(ui.length).map_err(|_| ChmError::LengthOverflow(ui.length))?;
			let mut buffer = vec![0u8; len];
			let bytes_read = chm_retrieve_object(self.handle, &raw const ui, buffer.as_mut_ptr(), 0, ui.length);
			if bytes_read != ui.length {
				return Err(ChmError::ShortRead { expected: ui.length, actual: bytes_read });
			}
			Ok(buffer)
		}
	}
}

impl Drop for ChmHandle {
	fn drop(&mut self) {
		if !self.handle.is_null() {
			unsafe {
				chm_close(self.handle);
			}
		}
	}
}

// SAFETY: the handle is uniquely owned and CHM operations are file-level thread-safe.
unsafe impl Send for ChmHandle {}
unsafe impl Sync for ChmHandle {}

#[must_use]
/// Convert a `ChmUnitInfo` path into a Rust `String`.
pub fn unit_info_path(ui: &ChmUnitInfo) -> String {
	unsafe { CStr::from_ptr(ui.path.as_ptr()).to_string_lossy().into_owned() }
}
