//! BW-15-01-05: memory mapping and shared-library lookup end to end.
//!
//! The acceptance: **the three platforms load the same test resource**
//! (`三端加载相同测试资源`). This test is the rerunnable fixture: this
//! host measures the Linux leg, the same code compiles on the other
//! legs, and the row stays open for the three-end acceptance.
//!
//! Four faces:
//!
//! - **Fixed resource through `map_file`.** A fixed 64 KiB pattern file
//!   is mapped and every byte compared — the same bytes a windows/macOS
//!   runner reads from the same fixture shape.
//! - **Anonymous mapping.** `anonymous(len)`: the requested length, and
//!   the pages read zeroed (the type exposes the bytes read-only — the
//!   observable contract).
//! - **Shared library open + symbol.** The C runtime library, by the
//!   platform's own name (posix shells and windows differ, spelled in
//!   the cfg helpers), opened through `DynamicLibrary::open`; a symbol
//!   every C runtime exports is non-null, and a name no library exports
//!   is an error — both data faces.
//! - **Lookup determinism.** The same library opened twice resolves the
//!   same symbol to the same address (the dynamic loader dedupes by
//!   soname), which is what makes a plugin graph stable across loads.
//!
//! Wall-clock readings (map/open/symbol cost) are recorded, never
//! asserted. The record sink writes `metric<TAB>value` rows to stdout
//! and, when `PLATFORM_MMAP_OUT` is set, to that path:
//!
//! ```sh
//! PLATFORM_MMAP_OUT=<abs path> cargo test -p platform \
//!     --test mmap_library_baseline -- --nocapture
//! ```

use std::fs;
use std::io::Write as _;
use std::time::Instant;

use platform::mmap::{DynamicLibrary, Mmap};

struct Record {
    rows: Vec<(String, String)>,
}

impl Record {
    fn new() -> Self {
        Self { rows: Vec::new() }
    }
    fn add(&mut self, key: &str, value: String) {
        println!("{}\t{}", key, value);
        self.rows.push((key.to_string(), value));
    }
    fn flush(self) {
        if let Ok(path) = std::env::var("PLATFORM_MMAP_OUT") {
            if let Some(dir) = std::path::Path::new(&path).parent() {
                fs::create_dir_all(dir).expect("create output dir");
            }
            let mut f = fs::File::create(&path).expect("create output file");
            for (k, v) in &self.rows {
                writeln!(f, "{}\t{}", k, v).expect("write output file");
            }
        }
    }
}

/// The payload every leg maps: a fixed 64 KiB pattern derived from the
/// index, so any flipped byte is a named failure.
const PAYLOAD: usize = 64 * 1024;

fn pattern() -> Vec<u8> {
    (0..PAYLOAD).map(|i| (i % 251) as u8).collect()
}

/// The C runtime library each platform's loader already has resident,
/// by the name its own loader resolves.
#[cfg(target_os = "linux")]
const C_RUNTIME: &str = "libc.so.6";
#[cfg(target_os = "macos")]
const C_RUNTIME: &str = "/usr/lib/libSystem.B.dylib";
#[cfg(windows)]
const C_RUNTIME: &str = "kernel32.dll";

/// A symbol every C runtime exports, the lookup's positive face.
#[cfg(not(windows))]
const PRESENT_SYMBOL: &str = "malloc";
#[cfg(windows)]
const PRESENT_SYMBOL: &str = "GetCurrentProcessId";

#[test]
fn the_same_resource_maps_and_the_library_resolves() {
    let mut rec = Record::new();
    let dir = std::env::temp_dir().join(format!("bw15-mmap-baseline-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("mkdir scratch");
    let path = dir.join("resource.bin");

    // --- face 1: the fixed resource maps byte-identical -------------------
    let want = pattern();
    fs::write(&path, &want).expect("write the resource");
    let start = Instant::now();
    let view = Mmap::map_file(path.to_str().expect("utf8 path")).expect("map the resource");
    let map_us = start.elapsed().as_micros() as u64;
    assert_eq!(view.len(), PAYLOAD, "the mapping covers the resource");
    assert!(
        view.iter().zip(&want).all(|(a, b)| a == b),
        "every mapped byte matches the fixture"
    );
    rec.add("map_file_us", map_us.to_string());
    rec.add("map_bytes", view.len().to_string());

    // --- face 2: the anonymous mapping is zeroed on read ------------------
    // The type exposes the pages read-only (Deref to [u8]); the
    // observable contract is the length and the zeroed first read.
    let anon = Mmap::anonymous(4096).expect("map anonymous");
    assert_eq!(anon.len(), 4096, "the anonymous mapping has its length");
    assert!(
        anon.iter().all(|&b| b == 0),
        "an anonymous mapping starts zeroed"
    );
    rec.add("anon_len", anon.len().to_string());

    // --- face 3: the shared library opens and resolves --------------------
    let start = Instant::now();
    let lib = DynamicLibrary::open(C_RUNTIME).expect("open the C runtime");
    let open_us = start.elapsed().as_micros() as u64;
    let start = Instant::now();
    let sym = lib.symbol(PRESENT_SYMBOL).expect("resolve the symbol");
    let sym_us = start.elapsed().as_micros() as u64;
    assert!(
        !sym.is_null(),
        "{PRESENT_SYMBOL} resolves to a real address"
    );
    let missing = lib.symbol("no_such_symbol_anywhere_12345");
    assert!(missing.is_err(), "a name nothing exports is an error");
    rec.add("lib_open_us", open_us.to_string());
    rec.add("symbol_us", sym_us.to_string());
    rec.add("symbol_found", PRESENT_SYMBOL.to_string());

    // --- face 4: the same library resolves the same address ---------------
    let lib_again = DynamicLibrary::open(C_RUNTIME).expect("reopen the C runtime");
    let sym_again = lib_again
        .symbol(PRESENT_SYMBOL)
        .expect("resolve the symbol again");
    assert_eq!(
        sym, sym_again,
        "two opens land the same loaded image and symbol address"
    );

    rec.add(
        "host",
        format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    );
    rec.flush();
    let _ = fs::remove_dir_all(&dir);
}
