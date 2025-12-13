use std::{
	env, fs,
	io::Cursor,
	path::{Path, PathBuf},
};

use bzip2::read::BzDecoder;
use cc::Build;
use tar::Archive;

fn main() {
	let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
	let chmlib_dir = out_dir.join("chmlib-0.40");
	let src_dir = chmlib_dir.join("src");
	if !chmlib_dir.exists() {
		download_and_extract(&out_dir);
	}
	apply_patches(&src_dir);
	let mut build = Build::new();
	build.file(src_dir.join("chm_lib.c")).file(src_dir.join("lzx.c")).include(&src_dir).warnings(false);
	if cfg!(target_os = "windows") {
		build.define("WIN32", None);
		build.define("_WINDOWS", None);
	} else {
		build.define("CHMLIB_HAVE_STRINGS_H", None);
	}
	build.compile("chm");
	println!("cargo:rustc-link-lib=static=chm");
}

fn download_and_extract(out_dir: &Path) {
	let url = "http://www.jedrea.com/chmlib/chmlib-0.40.tar.bz2";
	let response = ureq::get(url).call().expect("Failed to download chmlib");
	let buf = response.into_body().read_to_vec().expect("Failed to read chmlib tarball");
	let decompressor = BzDecoder::new(Cursor::new(buf));
	let mut archive = Archive::new(decompressor);
	archive.unpack(out_dir).expect("Failed to extract chmlib");
}

fn apply_patches(src_dir: &Path) {
	let chm_lib_path = src_dir.join("chm_lib.c");
	let mut contents = fs::read_to_string(&chm_lib_path).expect("Failed to read chm_lib.c");
	contents = contents.replace("/* yielding an error is preferable to yielding incorrect behavior */\n#error \"Please define the sized types for your platform in chm_lib.c\"", "typedef unsigned char           UChar;\ntypedef int16_t                 Int16;\ntypedef uint16_t                UInt16;\ntypedef int32_t                 Int32;\ntypedef uint32_t                UInt32;\ntypedef int64_t                 Int64;\ntypedef uint64_t                UInt64;");
	contents = contents
		.replace("#if __sun || __sgi\n#include <strings.h>", "#ifdef CHMLIB_HAVE_STRINGS_H\n#include <strings.h>");
	fs::write(&chm_lib_path, contents).expect("Failed to write patched chm_lib.c");
}
