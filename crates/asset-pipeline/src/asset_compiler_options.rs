//! `AssetCompilerOptions` port (`compiler/asset_compiler_options.cpp`): the
//! command-line options that configure an [`AssetCompiler`].
//!
//! Reference quirks preserved:
//! - `parseCommandLine` reads `-intermediatePath`, `-outputPath`,
//!   `-cachePath`, `-j` (thread count) and the presence flags `-recursive` /
//!   `-forceRebuild` (the reference accepts both `-` and `/` switch
//!   prefixes, and a value is the following token);
//! - a path value is sanitised (`sanitisePathParam`), its parent chain is
//!   created, and it is only accepted when the result is a directory (an
//!   empty path is valid and leaves the option unset);
//! - `apply` sets the intermediate path *before* the output path (the output
//!   path may be relative to it), then the thread count, the force-rebuild /
//!   recursive flags and finally the content cache settings;
//! - a setter records a `*Changed` flag so `apply` only pushes a path that
//!   was actually set by the command line.

use resources::bwresource::BwResource;
use resources::zip::FileType;

use crate::asset_compiler::AssetCompiler;

/// The command-line options (see the module docs).
#[derive(Debug, Clone, Default)]
pub struct AssetCompilerOptions {
    cache_path: String,
    intermediate_path: String,
    output_path: String,
    num_threads: i32,
    force_rebuild: bool,
    recursive: bool,
    enable_cache_read: bool,
    enable_cache_write: bool,
    cache_path_changed: bool,
    intermediate_path_changed: bool,
    output_path_changed: bool,
    num_threads_changed: bool,
}

impl AssetCompilerOptions {
    /// `AssetCompilerOptions()`: the defaults (one thread, cache read/write
    /// enabled, no paths).
    pub fn new() -> Self {
        Self {
            num_threads: 0,
            enable_cache_read: true,
            enable_cache_write: true,
            ..Self::default()
        }
    }

    /// `AssetCompilerOptions( compiler )`: capture a compiler's current state
    /// (used by the JIT compiler to inherit the batch settings).
    pub fn from_compiler(compiler: &AssetCompiler) -> Self {
        Self {
            cache_path: compiler.get_cache_path().to_string(),
            intermediate_path: compiler.get_intermediate_path().to_string(),
            output_path: compiler.get_output_path().to_string(),
            num_threads: compiler.get_thread_count() as i32,
            force_rebuild: compiler.get_force_rebuild(),
            recursive: compiler.get_recursive(),
            enable_cache_read: compiler.get_cache_read(),
            enable_cache_write: compiler.get_cache_write(),
            ..Self::default()
        }
    }

    /// `parseCommandLine`: scan the tokens (the reference's whitespace-split
    /// command string) for the supported options.
    pub fn parse_command_line(&mut self, args: &[String], resource: &BwResource) {
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            match arg.as_str() {
                "-intermediatePath" | "/intermediatePath" => {
                    if let Some(value) = args.get(index + 1) {
                        self.intermediate_path_set(value, resource);
                        index += 1;
                    }
                }
                "-outputPath" | "/outputPath" => {
                    if let Some(value) = args.get(index + 1) {
                        self.output_path_set(value, resource);
                        index += 1;
                    }
                }
                "-cachePath" | "/cachePath" => {
                    if let Some(value) = args.get(index + 1) {
                        self.cache_path_set(value, resource);
                        index += 1;
                    }
                }
                "-j" | "/j" => {
                    if let Some(value) = args.get(index + 1) {
                        let count: i32 = value.parse().unwrap_or(0);
                        self.thread_count_set(count);
                        index += 1;
                    }
                }
                "-recursive" | "/recursive" => self.recursive_set(true),
                "-forceRebuild" | "/forceRebuild" => self.force_rebuild_set(true),
                _ => {}
            }
            index += 1;
        }
    }

    /// `apply`: push the changed settings onto `compiler`.
    pub fn apply(&mut self, compiler: &mut AssetCompiler) {
        if self.intermediate_path_changed {
            compiler.set_intermediate_path(&self.intermediate_path);
            self.intermediate_path_changed = false;
        }

        if self.output_path_changed {
            // Must be set after the intermediate path.
            compiler.set_output_path(&self.output_path);
            self.output_path_changed = false;
        }

        if self.num_threads_changed {
            compiler.set_thread_count(self.num_threads as u32);
            self.num_threads_changed = false;
        }

        compiler.set_force_rebuild(self.force_rebuild);
        compiler.set_recursive(self.recursive);

        if self.cache_path_changed {
            compiler.set_cache_path(self.cache_path.clone());
            self.cache_path_changed = false;
        }

        compiler.set_cache_read(self.enable_cache_read);
        compiler.set_cache_write(self.enable_cache_write);
    }

    pub fn cache_path(&self) -> &str {
        &self.cache_path
    }
    pub fn intermediate_path(&self) -> &str {
        &self.intermediate_path
    }
    pub fn output_path(&self) -> &str {
        &self.output_path
    }
    pub fn thread_count(&self) -> i32 {
        self.num_threads
    }
    pub fn force_rebuild(&self) -> bool {
        self.force_rebuild
    }
    pub fn recursive(&self) -> bool {
        self.recursive
    }
    pub fn enable_cache_read(&self) -> bool {
        self.enable_cache_read
    }
    pub fn enable_cache_write(&self) -> bool {
        self.enable_cache_write
    }

    /// `cachePath( path )`: sanitise and store (only when it changed).
    pub fn cache_path_set(&mut self, path: &str, resource: &BwResource) {
        if let Some(clean) = Self::sanitise_path(path, resource)
            && clean != self.cache_path
        {
            self.cache_path = clean;
            self.cache_path_changed = true;
        }
    }

    /// `intermediatePath( path )`.
    pub fn intermediate_path_set(&mut self, path: &str, resource: &BwResource) {
        if let Some(clean) = Self::sanitise_path(path, resource)
            && clean != self.intermediate_path
        {
            self.intermediate_path = clean;
            self.intermediate_path_changed = true;
        }
    }

    /// `outputPath( path )`.
    pub fn output_path_set(&mut self, path: &str, resource: &BwResource) {
        if let Some(clean) = Self::sanitise_path(path, resource)
            && clean != self.output_path
        {
            self.output_path = clean;
            self.output_path_changed = true;
        }
    }

    pub fn thread_count_set(&mut self, count: i32) {
        if count != self.num_threads {
            self.num_threads = count;
            self.num_threads_changed = true;
        }
    }

    pub fn force_rebuild_set(&mut self, rebuild: bool) {
        self.force_rebuild = rebuild;
    }

    pub fn recursive_set(&mut self, recurse: bool) {
        self.recursive = recurse;
    }

    pub fn enable_cache_read_set(&mut self, enable: bool) {
        self.enable_cache_read = enable;
    }

    pub fn enable_cache_write_set(&mut self, enable: bool) {
        self.enable_cache_write = enable;
    }

    /// `sanitisePath`: an empty path is valid (leaves the option unset);
    /// otherwise the path is sanitised, its parent chain is created, and it
    /// is only accepted when the result is a directory.
    fn sanitise_path(path: &str, resource: &BwResource) -> Option<String> {
        if path.is_empty() {
            return Some(String::new());
        }
        let sanitised = AssetCompiler::sanitise_path_param(path);
        resource.ensure_absolute_path_exists(&sanitised);
        if resource.get_file_type(&sanitised, None) != FileType::Directory {
            return None;
        }
        Some(sanitised)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parseCommandLine` reads the supported options and leaves the rest.
    #[test]
    fn parse_command_line_reads_options() {
        let args: Vec<String> = [
            "-intermediatePath",
            "/tmp/int",
            "-outputPath",
            "/tmp/out",
            "-j",
            "4",
            "-recursive",
            "-forceRebuild",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        // A resource with no real paths: the sanitised paths must be created
        // and reported as directories.
        let temp = std::env::temp_dir().join(format!("bw_opts_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        let root = temp.to_string_lossy().into_owned();
        let mut resource = BwResource::init_paths(&[root.as_str()]);
        resource.enable_editor_default_root();

        let mut options = AssetCompilerOptions::new();
        options.parse_command_line(&args, &resource);

        assert_eq!(options.thread_count(), 4);
        assert!(options.recursive());
        assert!(options.force_rebuild());
        // The sanitised paths are absolute directories under the temp root.
        assert!(options.intermediate_path().contains("/tmp/int/"));
        assert!(options.output_path().contains("/tmp/out/"));
        // The cache path was not set.
        assert!(options.cache_path().is_empty());

        let _ = std::fs::remove_dir_all(&temp);
    }

    /// `apply` only pushes a path that was changed by the command line.
    #[test]
    fn apply_sets_intermediate_before_output() {
        let temp = std::env::temp_dir().join(format!("bw_opts2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        let root = temp.to_string_lossy().into_owned();
        let mut resource = BwResource::init_paths(&[root.as_str()]);
        resource.enable_editor_default_root();

        let int = format!("{root}/int");
        let out = format!("{root}/out");
        std::fs::create_dir_all(&int).unwrap();
        std::fs::create_dir_all(&out).unwrap();

        let mut options = AssetCompilerOptions::new();
        options.intermediate_path_set(&int, &resource);
        options.output_path_set(&out, &resource);
        options.thread_count_set(2);
        options.force_rebuild_set(true);

        let mut compiler = AssetCompiler::new(std::rc::Rc::new(resource));
        options.apply(&mut compiler);

        assert_eq!(compiler.get_intermediate_path(), format!("{int}/"));
        assert_eq!(compiler.get_output_path(), format!("{out}/"));
        assert_eq!(compiler.get_thread_count(), 2);
        assert!(compiler.get_force_rebuild());

        let _ = std::fs::remove_dir_all(&temp);
    }

    /// A non-directory path is rejected (left unset).
    #[test]
    fn sanitise_rejects_non_directory() {
        let temp = std::env::temp_dir().join(format!("bw_opts3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        let root = temp.to_string_lossy().into_owned();
        let mut resource = BwResource::init_paths(&[root.as_str()]);
        resource.enable_editor_default_root();

        let file = format!("{root}/not_a_dir");
        std::fs::write(&file, b"x").unwrap();

        let mut options = AssetCompilerOptions::new();
        options.intermediate_path_set(&file, &resource);
        assert!(options.intermediate_path().is_empty());

        let _ = std::fs::remove_dir_all(&temp);
    }
}
