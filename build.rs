use std::{fs, path::PathBuf};

use cc::Build;

fn main() {
	let src_dir = PathBuf::from("vendor/chmlib/src");
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

fn apply_patches(src_dir: &std::path::Path) {
	let chm_lib_path = src_dir.join("chm_lib.c");
	let mut contents = fs::read_to_string(&chm_lib_path).expect("Failed to read chm_lib.c");
	contents = contents.replace("/* yielding an error is preferable to yielding incorrect behavior */\n#error \"Please define the sized types for your platform in chm_lib.c\"", "typedef unsigned char           UChar;\ntypedef int16_t                 Int16;\ntypedef uint16_t                UInt16;\ntypedef int32_t                 Int32;\ntypedef uint32_t                UInt32;\ntypedef int64_t                 Int64;\ntypedef uint64_t                UInt64;");
	contents = contents
		.replace("#if __sun || __sgi\n#include <strings.h>", "#ifdef CHMLIB_HAVE_STRINGS_H\n#include <strings.h>");
	fs::write(&chm_lib_path, contents).expect("Failed to write patched chm_lib.c");
}
