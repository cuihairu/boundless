//! `ContentAddressableCache` port (`conversion/content_addressable_cache.cpp`):
//! a file cache that stores files under their hash.
//!
//! Reference quirks preserved:
//! - the cache file name is `{cachePath}{low byte of hash in hex}/{file name}.{hash in hex}`
//!   — the sub-directory is the hash's low byte in *unpadded* uppercase hex
//!   (`%hhX`), one of at most 256 buckets;
//! - reads and writes both stage through a temporary file in the destination
//!   directory and then move it into place, erasing the temporary on any
//!   failure;
//! - a disabled cache (or an empty cache path) answers `false` without any
//!   callback;
//! - every outcome is reported to the compiler (`onCacheRead`, miss, write,
//!   write-miss).
//!
//! The reference keeps the configuration in statics and reaches
//! `BWResource::instance()`; here the configuration lives on the instance and
//! the file operations go straight to the native layer (the reference's
//! `BWResource` multiplexer resolves absolute cache paths to plain file I/O
//! anyway).

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use resources::bwresource::{get_file_path, get_filename};

/// The `Compiler` cache callbacks (`onCacheRead` & co.).
pub trait CacheEvents {
    /// `onCacheRead`: a cache read was served.
    fn on_cache_read(&self, filename: &str);
    /// `onCacheReadMiss`: a cache read failed (disabled, absent or I/O error).
    fn on_cache_read_miss(&self, filename: &str);
    /// `onCacheWrite`: a cache write stored the file.
    fn on_cache_write(&self, filename: &str);
    /// `onCacheWriteMiss`: a cache write failed.
    fn on_cache_write_miss(&self, filename: &str);
}

/// The no-op compiler callback set.
#[derive(Clone, Copy, Default)]
pub struct NoCacheEvents;

impl CacheEvents for NoCacheEvents {
    fn on_cache_read(&self, _filename: &str) {}
    fn on_cache_read_miss(&self, _filename: &str) {}
    fn on_cache_write(&self, _filename: &str) {}
    fn on_cache_write_miss(&self, _filename: &str) {}
}

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// `GetTempFileNameA`: a unique path in `directory` starting with `prefix`.
/// The reference's zero-length file is created by the copy that follows.
fn temp_file_name(directory: &str, prefix: &str) -> String {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}/{}.{}.{}.tmp",
        directory.trim_end_matches('/'),
        prefix,
        std::process::id(),
        id
    )
}

/// A cache that stores files by their hash.
pub struct ContentAddressableCache<E: CacheEvents> {
    /// `cachePath_` (empty until set; an empty path disables the cache).
    cache_path: String,
    /// `readFromCache_` (default `true`).
    read_enabled: bool,
    /// `writeToCache_` (default `true`).
    write_enabled: bool,
    events: E,
}

impl<E: CacheEvents> ContentAddressableCache<E> {
    /// The defaults: empty path, read and write enabled.
    pub fn new(events: E) -> Self {
        Self {
            cache_path: String::new(),
            read_enabled: true,
            write_enabled: true,
            events,
        }
    }

    /// `getCachePath`.
    pub fn cache_path(&self) -> &str {
        &self.cache_path
    }

    /// `setCachePath`. The reference stores the value verbatim (its
    /// `AssetCompilerOptions::cachePath` is already sanitised to a trailing
    /// slash), so a caller wanting the `%s%hhX/` bucket layout must pass a
    /// path ending in `/`.
    pub fn set_cache_path(&mut self, cache_path: impl Into<String>) {
        self.cache_path = cache_path.into();
    }

    /// `getReadFromCache`.
    pub fn read_enabled(&self) -> bool {
        self.read_enabled
    }

    /// `setReadFromCache`.
    pub fn set_read_enabled(&mut self, read: bool) {
        self.read_enabled = read;
    }

    /// `getWriteToCache`.
    pub fn write_enabled(&self) -> bool {
        self.write_enabled
    }

    /// `setWriteToCache`.
    pub fn set_write_enabled(&mut self, write: bool) {
        self.write_enabled = write;
    }

    /// The cache file for `filename` at `hash`:
    /// `{cachePath}{low byte of hash in hex}/{file name}.{hash in hex}`.
    fn cache_filename(&self, filename: &str, hash: u64) -> String {
        format!(
            "{}{:X}/{}.{:X}",
            self.cache_path,
            hash as u8,
            get_filename(filename),
            hash
        )
    }

    /// `readFromCache`: restore `filename` from its cache entry. `true` when
    /// the file was served from the cache.
    pub fn read_from_cache(&self, filename: &str, hash: u64) -> bool {
        if !self.read_enabled || self.cache_path.is_empty() {
            return false;
        }

        let directory = get_file_path(filename);
        let cache_filename = self.cache_filename(filename, hash);

        // Check if the file exists in the cache.
        if !Path::new(&cache_filename).exists() {
            self.events.on_cache_read_miss(filename);
            return false;
        }

        // Copy from the cache file to a temporary file, then move it into
        // place (the temporary is erased when either step fails).
        let temp_filename = temp_file_name(&directory, get_filename(filename));
        if std::fs::copy(&cache_filename, &temp_filename).is_err() {
            let _ = std::fs::remove_file(&temp_filename);
            self.events.on_cache_read_miss(filename);
            return false;
        }
        if std::fs::rename(&temp_filename, filename).is_err() {
            let _ = std::fs::remove_file(&temp_filename);
            self.events.on_cache_read_miss(filename);
            return false;
        }

        self.events.on_cache_read(filename);
        true
    }

    /// `writeToCache`: store `filename` under its hash. `true` when the file
    /// was written to the cache.
    pub fn write_to_cache(&self, filename: &str, hash: u64) -> bool {
        if !self.write_enabled || self.cache_path.is_empty() {
            return false;
        }

        // Copy the file to a temporary file on the cache, then move it to its
        // content-addressed name (creating the bucket directory first).
        let temp_cache_filename = temp_file_name(&self.cache_path, get_filename(filename));
        if std::fs::copy(filename, &temp_cache_filename).is_err() {
            let _ = std::fs::remove_file(&temp_cache_filename);
            self.events.on_cache_write_miss(filename);
            return false;
        }

        let cache_filename = self.cache_filename(filename, hash);
        if let Some(parent) = Path::new(&cache_filename).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::rename(&temp_cache_filename, &cache_filename).is_err() {
            let _ = std::fs::remove_file(&temp_cache_filename);
            self.events.on_cache_write_miss(filename);
            return false;
        }

        self.events.on_cache_write(filename);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "bw_asset_pipeline_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    /// A callback recorder.
    #[derive(Default)]
    struct Recorder {
        reads: RefCell<Vec<&'static str>>,
        misses: RefCell<Vec<&'static str>>,
        writes: RefCell<Vec<&'static str>>,
        write_misses: RefCell<Vec<&'static str>>,
    }

    const FILE: &str = "out.model";

    impl CacheEvents for Recorder {
        fn on_cache_read(&self, _filename: &str) {
            self.reads.borrow_mut().push("read");
        }
        fn on_cache_read_miss(&self, _filename: &str) {
            self.misses.borrow_mut().push("miss");
        }
        fn on_cache_write(&self, _filename: &str) {
            self.writes.borrow_mut().push("write");
        }
        fn on_cache_write_miss(&self, _filename: &str) {
            self.write_misses.borrow_mut().push("write-miss");
        }
    }

    /// The acceptance: the second run with the same input reuses the cached
    /// artifact.
    #[test]
    fn second_run_with_same_input_reuses() {
        let root = temp_root("reuse");
        let cache = format!("{root}/cache");
        std::fs::create_dir_all(&cache).unwrap();
        let target = format!("{root}/{FILE}");
        std::fs::write(&target, b"converted bytes").unwrap();

        let mut cac = ContentAddressableCache::new(Recorder::default());
        cac.set_cache_path(format!("{cache}/"));

        // First run: nothing cached yet (read miss), the output is stored.
        assert!(!cac.read_from_cache(&target, 0xABCD));
        assert!(cac.write_to_cache(&target, 0xABCD));
        assert_eq!(cac.events.reads.borrow().len(), 0);
        assert_eq!(cac.events.misses.borrow().len(), 1);
        assert_eq!(cac.events.writes.borrow().len(), 1);

        // Second run: the same input hash restores the artifact byte for byte.
        std::fs::remove_file(&target).unwrap();
        assert!(cac.read_from_cache(&target, 0xABCD));
        assert_eq!(std::fs::read(&target).unwrap(), b"converted bytes");
        assert_eq!(cac.events.reads.borrow().len(), 1);
        // A different hash is a different entry: miss.
        assert!(!cac.read_from_cache(&target, 0x1234));
        assert_eq!(cac.events.misses.borrow().len(), 2);
    }

    /// The cache file lands in the low-byte bucket with the unpadded hex
    /// name the reference builds with `%hhX`/`%llX`.
    #[test]
    fn cache_file_naming_matches_reference() {
        let root = temp_root("naming");
        let cache = format!("{root}/cache");
        std::fs::create_dir_all(&cache).unwrap();
        let target = format!("{root}/{FILE}");
        std::fs::write(&target, b"x").unwrap();

        let mut cac = ContentAddressableCache::new(NoCacheEvents);
        cac.set_cache_path(format!("{cache}/"));
        assert!(cac.write_to_cache(&target, 0x12340B));

        // 0x12340B: low byte 0x0B -> bucket "B" (unpadded, `%hhX`),
        // full hash -> file "out.model.12340B".
        let cached = format!("{cache}/B/{FILE}.12340B");
        assert!(Path::new(&cached).exists(), "{cached} should exist");
    }

    /// A disabled cache (or an empty path) answers false with no callbacks.
    #[test]
    fn disabled_cache_is_silent() {
        let root = temp_root("disabled");
        let target = format!("{root}/{FILE}");
        std::fs::write(&target, b"x").unwrap();

        // Empty path: no callbacks at all.
        let mut cac = ContentAddressableCache::new(Recorder::default());
        assert!(!cac.read_from_cache(&target, 1));
        assert!(!cac.write_to_cache(&target, 1));
        assert_eq!(cac.events.misses.borrow().len(), 0);
        assert_eq!(cac.events.write_misses.borrow().len(), 0);

        // Read disabled.
        cac.set_cache_path(&root);
        cac.set_read_enabled(false);
        assert!(!cac.read_from_cache(&target, 1));
        assert_eq!(cac.events.misses.borrow().len(), 0);

        // Write disabled.
        cac.set_read_enabled(true);
        cac.set_write_enabled(false);
        assert!(!cac.write_to_cache(&target, 1));
        assert_eq!(cac.events.write_misses.borrow().len(), 0);
    }

    /// Defaults: read and write enabled, empty path.
    #[test]
    fn defaults_match_reference() {
        let cac = ContentAddressableCache::new(NoCacheEvents);
        assert_eq!(cac.cache_path(), "");
        assert!(cac.read_enabled());
        assert!(cac.write_enabled());
    }
}
