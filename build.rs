use std::{env, path::PathBuf};

use cc::Build;

fn main() {
	let src_dir = PathBuf::from("vendor/chmlib/src");
	let mut build = Build::new();
	build.file(src_dir.join("chm_lib.c")).file(src_dir.join("lzx.c")).include(&src_dir).warnings(false);
	let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
	if target_os == "windows" {
		build.define("WIN32", None);
		build.define("_WINDOWS", None);
	} else {
		build.define("CHMLIB_HAVE_STRINGS_H", None);
	}
	build.compile("chm");
	println!("cargo:rustc-link-lib=static=chm");
}
