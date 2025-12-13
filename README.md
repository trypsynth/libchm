# libchm

Tiny Rust wrapper around `libchm` for reading CHM archives with safe-ish helpers.

## Status

- Minimal API: open a file, enumerate entries, read a file, and inspect `ChmUnitInfo`.
- Bundles and builds upstream `chmlib` during `build.rs` (downloads `chmlib-0.40` from `jedrea.com`).
- Tested only on Windows and Linux x64, treat as experimental.

## Installation

```sh
cargo ad libchm
```

## Usage

```rust
use libchm::{ChmHandle, CHM_ENUMERATE_ALL, Result, unit_info_path};

fn main() -> Result<()> {
	let mut chm = ChmHandle::open("docs.chm")?;
	chm.enumerate(CHM_ENUMERATE_ALL, |ui| {
		println!("{}", unit_info_path(ui));
		true // keep going
	})?;
	let bytes = chm.read_file("/index.html")?;
	println!("index size: {}", bytes.len());
	Ok(())
}
```

## Building

- Requires a C toolchain.
- Build script downloads and compiles `chmlib-0.40` automatically (no system install needed).
- No network? Pre-download the tarball and point Cargo to a cached source via `CARGO_NET_OFFLINE` or a mirror.

## License

MIT for this crate. Upstream `chmlib` is LGPL-2.0-or-later; ensure compatibility for your use case.
