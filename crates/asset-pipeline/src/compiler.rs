//! `Compiler` port (`compiler/compiler.hpp`): the interface the asset
//! pipeline uses for callbacks, hashing, task queueing and path resolution.
//!
//! Reference quirks preserved:
//! - the reference mutates compiler state through thread-local flags and
//!   mutex-guarded caches while handing `const Compiler&` to converters, so
//!   every method here is `&self` (interior mutability) — a converter can
//!   only call the `&self` surface, matching the reference's const-ness
//!   boundary;
//! - `ensureUpToDate` may trigger the conversion of a sub-dependency and
//!   reports the task it resolved (the reference's `ConversionTask*&`
//!   out-parameter);
//! - the on-task/on-convert callbacks are no-ops the compiler impl and the
//!   batch driver override;
//! - `getHash` combines a file's path hash with its content hash (`Hash64::
//!   combine`), which the dependency hashes and the `.deps` input hash rely
//!   on being stable across runs.

use crate::conversion_task::{ConversionTask, TaskHandle};
use crate::dependency::Dependency;
use crate::dependency_list::HashProvider;

/// `Compiler`: the asset pipeline's compiler seam (see the module docs).
pub trait Compiler {
    /// `getResourcePaths`: the resource search paths added to `BWResource`.
    fn get_resource_paths(&self) -> &[String];

    /// `getHash( dependency )`: the combined hash of a dependency.
    fn get_hash(&self, dependency: &Dependency) -> u64;

    /// `getFileHash( fileName, forceUpdate )`: the content hash of a file
    /// (0 when it does not exist).
    fn get_file_hash(&self, file_name: &str, force_update: bool) -> u64;

    /// `getDirectoryHash( directory, pattern, regex, recursive )`.
    fn get_directory_hash(
        &self,
        directory: &str,
        pattern: &str,
        regex: bool,
        recursive: bool,
    ) -> u64;

    /// `checkFileHashUpToDate( fileName )`.
    fn check_file_hash_up_to_date(&self, file_name: &str) -> bool;

    /// `ensureUpToDate( dependency, &conversionTask )`: return whether the
    /// dependency is up to date, plus the task resolved for it (None for
    /// non-task dependencies).
    fn ensure_up_to_date(&self, dependency: &Dependency) -> (bool, Option<TaskHandle>);

    /// `getSourceFile( file )`: the source file for a compiled file.
    fn get_source_file(&self, file: &str) -> Option<String>;

    /// `queueTask( conversionTask )`: queue a task for processing.
    fn queue_task(&self, task: TaskHandle);

    /// `getNextTask()`: pop the next task off the queue.
    fn get_next_task(&self) -> Option<TaskHandle>;

    /// `hasTasks()`: whether unprocessed tasks remain.
    fn has_tasks(&self) -> bool;

    /// `setError` / `setWarning` / `resetErrorFlags` / `hasError` /
    /// `hasWarning`: the error/warning flags converters raise during a
    /// create-convert step.
    fn set_error(&self);
    fn set_warning(&self);
    fn reset_error_flags(&self);
    fn has_error(&self) -> bool;
    fn has_warning(&self) -> bool;

    /// `resolveRelativePath( path )`: convert an absolute path to relative.
    fn resolve_relative_path(&self, path: &mut String) -> bool;
    /// `resolveSourcePath( path )`: convert a path to an absolute source path.
    fn resolve_source_path(&self, path: &mut String) -> bool;
    /// `resolveIntermediatePath( path )`: convert a path to an absolute
    /// intermediate path.
    fn resolve_intermediate_path(&self, path: &mut String) -> bool;
    /// `resolveOutputPath( path )`: convert a path to an absolute output path.
    fn resolve_output_path(&self, path: &mut String) -> bool;

    /// `ContentAddressableCache::readFromCache( file, hash, compiler )`.
    fn cache_read(&self, file_name: &str, hash: u64) -> bool;
    /// `ContentAddressableCache::writeToCache( file, hash, compiler )`.
    fn cache_write(&self, file_name: &str, hash: u64) -> bool;

    /// `shouldIterateFile( file )`: whether discovery should descend into
    /// `file`.
    fn should_iterate_file(&self, file: &str) -> bool;
    /// `shouldIterateDirectory( directory )`: whether discovery should
    /// descend into `directory`.
    fn should_iterate_directory(&self, directory: &str) -> bool;

    // --- callbacks (the reference virtuals; the compiler impl and the
    // batch driver override these) ---

    fn on_task_started(&self, _task: &ConversionTask) {}
    fn on_task_resumed(&self, _task: &ConversionTask) {}
    fn on_task_suspended(&self, _task: &ConversionTask) {}
    fn on_task_completed(&self, _task: &ConversionTask) {}

    fn on_pre_create_dependencies(&self, _task: &ConversionTask) {}
    fn on_post_create_dependencies(&self, _task: &ConversionTask) {}

    fn on_pre_convert(&self, _task: &ConversionTask) {}
    fn on_post_convert(&self, _task: &ConversionTask) {}

    fn on_output_generated(&self, _filename: &str) {}

    fn on_cache_read(&self, _filename: &str) {}
    fn on_cache_read_miss(&self, _filename: &str) {}
    fn on_cache_write(&self, _filename: &str) {}
    fn on_cache_write_miss(&self, _filename: &str) {}
}

/// `&dyn Compiler` doubles as a `HashProvider` so a `DependencyList` can be
/// built directly against the compiler (the reference `DependencyList` holds
/// a `Compiler&`).
impl HashProvider for &dyn Compiler {
    fn get_hash(&self, dependency: &Dependency) -> u64 {
        Compiler::get_hash(*self, dependency)
    }
}

/// `CompilerHook`: the task-lifecycle seam a driver overrides. The reference
/// `BatchCompiler` subclasses `AssetCompiler` and overrides the virtual
/// callbacks; Rust has no inheritance, so the base `AssetCompiler` routes its
/// `Compiler` impl through an optional `CompilerHook` (the batch driver
/// installs one). Each hook method runs *in addition to* the base behaviour
/// (the reference's `BatchCompiler::onTaskStarted` calls the base after its
/// own bookkeeping).
///
/// The `should_iterate_*` hooks are consulted only after the base exclusion
/// check has passed (the reference `BatchCompiler::shouldIterateFile` calls
/// `AssetCompiler::shouldIterateFile` first and returns `false` early when it
/// vetoes).
pub trait CompilerHook {
    fn on_task_started(&self, _task: &ConversionTask) {}
    fn on_task_resumed(&self, _task: &ConversionTask) {}
    fn on_task_suspended(&self, _task: &ConversionTask) {}
    fn on_task_completed(&self, _task: &ConversionTask) {}
    fn on_pre_create_dependencies(&self, _task: &ConversionTask) {}
    fn on_pre_convert(&self, _task: &ConversionTask) {}
    fn on_output_generated(&self, _filename: &str) {}
    fn on_cache_read(&self, _filename: &str) {}
    fn on_cache_read_miss(&self, _filename: &str) {}
    fn on_cache_write(&self, _filename: &str) {}
    fn on_cache_write_miss(&self, _filename: &str) {}

    /// `shouldIterateFile`: return `false` to suppress iteration.
    fn should_iterate_file(&self, _file: &str) -> bool {
        true
    }
    /// `shouldIterateDirectory`: return `false` to suppress iteration.
    fn should_iterate_directory(&self, _directory: &str) -> bool {
        true
    }
}
