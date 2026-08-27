# libchm

Pure-Rust reader for CHM (Compiled HTML Help) archives, including LZX decompression. No unsafe code, no C dependencies.

## Installation

```sh
cargo add libchm
```

Requires Rust 1.87 or newer.

## Usage

```rust
use libchm::{ChmFile, EntrySel};

fn main() -> libchm::Result<()> {
    let mut chm = ChmFile::open("docs.chm")?;
    for entry in chm.entries(EntrySel::ALL)? {
        println!("{}", entry.path);
    }
    let bytes = chm.read_path("/index.html")?;
    println!("index size: {}", bytes.len());
    Ok(())
}
```

Paths are absolute within the archive and matched case-insensitively; directory entries end with `/`.

Use `find` and `read` separately when you want the entry's metadata before reading it:

```rust
let entry = chm.find("/index.html")?;
println!("{} is {} bytes", entry.path, entry.length);
let bytes = chm.read(&entry)?;
```

### Selecting entries

`EntrySel` is a bitflag that filters by category (`NORMAL`, `SPECIAL`, `META`) and kind (`FILES`, `DIRS`). An entry matches only when the selector names both its category and its kind, so `EntrySel::NORMAL` alone yields nothing. Use `EntrySel::ALL` to get everything, or combine flags:

```rust
// Ordinary content files, skipping directories and CHM-internal metadata.
let pages = chm.entries(EntrySel::NORMAL | EntrySel::FILES)?;
// Everything under one directory.
let images = chm.entries_in("/images", EntrySel::NORMAL | EntrySel::FILES)?;
```

## Robustness

CHM archives are untrusted input. Every header field that feeds an allocation, an index, or a divisor is validated before use, so a malformed or hostile file is reported as a `ChmError` rather than panicking or attempting a huge allocation.

## Bindings

- [pychmrs](https://github.com/joshuashaffer/pychmrs): Python wrapper

## License

MIT
