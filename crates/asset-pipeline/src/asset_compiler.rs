//! `AssetCompiler` port (`compiler/asset_compiler.cpp`): the concrete
//! compiler the pipeline drives, owning the task queue, the conversion-rule
//! list and the per-file hash cache.
//!
//! Reference quirks preserved:
//! - the `Compiler` seam is implemented with interior mutability
//!   (`Cell`/`RefCell`) so a converter handed a `&dyn Compiler` can still
//!   mutate the queue and error flags, matching the reference's
//!   mutex-guarded state behind `const Compiler&`;
//! - the *recursive* `ensureCompiled` branch is always taken (sub-dependencies
//!   run inline), because the single-threaded port has no other worker to
//!   hand a queued sub-task to — the reference's `recursive_` flag is kept as
//!   an observable API but does not change this;
//! - a *cyclic* dependency (a sub-task that is still mid-flight when
//!   `ensureCompiled` is asked for it) fails the sub-task instead of the
//!   reference's `numThreads_ == 1` spin, so the batch driver terminates;
//! - `getHash` combines the path hash with the file content hash (the
//!   reference's `Hash64::combine`), which the `.deps` input hash relies on
//!   being stable across runs;
//! - the `s_currentTask` source used by `resolveCustomPath` (the
//!   intermediate/output path when it is empty) is tracked per-task through
//!   the `onTaskStarted`/`onTaskCompleted` callbacks, and is swapped while a
//!   sub-task runs inline (the reference's `onTaskSuspended`/`onTaskResumed`).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use resources::bwresource::{BwResource, format_path, path_is_relative};
use resources::zip::FileType;

use crate::compiler::{Compiler, CompilerHook};
use crate::content_addressable_cache::{ContentAddressableCache, NoCacheEvents};
use crate::conversion_rule::ConversionRule;
use crate::conversion_task::{ConversionStatus, ConversionTask, TaskHandle};
use crate::converter_info::ConverterInfo;
use crate::converter_map::ConverterMap;
use crate::dependency::Dependency;
use crate::generic_conversion_rule::GenericConversionRule;
use crate::hash64;
use crate::task_finder::{TaskCompiler, TaskFinder};
use crate::task_processor::TaskProcessor;

/// `AssetCompiler::CompilerState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompilerState {
    Invalid,
    Executing,
    Paused,
    Terminating,
}

/// `AssetCompiler`: the concrete compiler (see the module docs).
pub struct AssetCompiler {
    state: Cell<CompilerState>,
    recursive: Cell<bool>,
    force_rebuild: Cell<bool>,
    num_threads: Cell<u32>,
    intermediate_path: String,
    output_path: String,
    /// `resourcePaths_`: the res paths captured by `addToolsResourcePaths`.
    resource_paths: Vec<String>,
    task_finder: TaskFinder,
    task_processor: TaskProcessor,
    /// `conversionRules_`: the shared rule list (the finder walks the same
    /// vector, so rules registered after construction are visible to it).
    conversion_rules: Rc<RefCell<Vec<Box<dyn ConversionRule>>>>,
    converter_map: Rc<RefCell<ConverterMap>>,
    resource: Rc<BwResource>,
    /// `taskQueue_`: the pending tasks (FIFO, like the reference's list).
    task_queue: RefCell<VecDeque<TaskHandle>>,
    /// `s_error` / `s_warning`: the flags converters raise during a
    /// create-convert step (thread-local in the reference, single here).
    error: Cell<bool>,
    warning: Cell<bool>,
    /// `s_currentTask->source_`: the source of the task currently being
    /// processed, used by `resolveCustomPath` when the base path is empty.
    current_task_source: RefCell<Option<String>>,
    /// `fileHashes_`: the content-hash cache keyed by absolute file name.
    file_hashes: RefCell<HashMap<String, u64>>,
    /// `ContentAddressableCache`: the hash-addressed file cache.
    content_cache: ContentAddressableCache<NoCacheEvents>,
    /// `CompilerHook`: the optional driver overrides (the reference's virtual
    /// callback overrides on `BatchCompiler`; see [`CompilerHook`]).
    hook: RefCell<Option<Rc<dyn CompilerHook>>>,
}

impl AssetCompiler {
    /// `AssetCompiler()`: a compiler over `resource` with the default state
    /// (`INVALID`, one thread, no paths, empty queue).
    pub fn new(resource: Rc<BwResource>) -> Self {
        let converter_map = Rc::new(RefCell::new(ConverterMap::new()));
        let conversion_rules = Rc::new(RefCell::new(Vec::new()));
        let task_finder = TaskFinder::new(conversion_rules.clone(), resource.clone());
        let task_processor = TaskProcessor::new(converter_map.clone(), false);
        Self {
            state: Cell::new(CompilerState::Invalid),
            recursive: Cell::new(false),
            force_rebuild: Cell::new(false),
            num_threads: Cell::new(1),
            intermediate_path: String::new(),
            output_path: String::new(),
            resource_paths: Vec::new(),
            task_finder,
            task_processor,
            conversion_rules,
            converter_map,
            resource,
            task_queue: RefCell::new(VecDeque::new()),
            error: Cell::new(false),
            warning: Cell::new(false),
            current_task_source: RefCell::new(None),
            file_hashes: RefCell::new(HashMap::new()),
            content_cache: ContentAddressableCache::new(NoCacheEvents),
            hook: RefCell::new(None),
        }
    }

    /// `setCallback`: install the driver's [`CompilerHook`] (the reference's
    /// subclass overrides).
    pub fn set_hook(&self, hook: Rc<dyn CompilerHook>) {
        *self.hook.borrow_mut() = Some(hook);
    }

    /// `addToolsResourcePaths`: capture the res paths already loaded in
    /// `BWResource` (called by the batch driver at construction).
    pub fn add_tools_resource_paths(&mut self) {
        self.resource_paths.clear();
        for index in 0..self.resource.get_path_num() {
            self.resource_paths
                .push(self.resource.get_path(index).to_string());
        }
    }

    /// `initCompiler`: load the generic conversion rule, register it, apply
    /// the force-rebuild flag to the processor, and enter `EXECUTING`.
    pub fn init_compiler(&mut self) {
        let mut generic_rule =
            GenericConversionRule::new(self.converter_map.clone(), self.resource.clone());
        generic_rule.load("asset_rules.xml");
        self.conversion_rules
            .borrow_mut()
            .push(Box::new(generic_rule));

        if self.force_rebuild.get() {
            self.task_processor.set_force_rebuild(true);
        }

        self.state.set(CompilerState::Executing);
    }

    /// `finiCompiler`: terminate and reset to `INVALID`.
    pub fn fini_compiler(&self) {
        self.terminate();
        self.state.set(CompilerState::Invalid);
    }

    /// `paused`.
    pub fn paused(&self) -> bool {
        self.state.get() == CompilerState::Paused
    }

    /// `executing`.
    pub fn executing(&self) -> bool {
        self.state.get() == CompilerState::Executing
    }

    /// `terminating`.
    pub fn terminating(&self) -> bool {
        self.state.get() == CompilerState::Terminating
    }

    /// `operating`.
    pub fn operating(&self) -> bool {
        self.paused() || self.executing()
    }

    /// `pause`.
    pub fn pause(&self) -> bool {
        if !self.executing() {
            return false;
        }
        self.state.set(CompilerState::Paused);
        true
    }

    /// `resume`.
    pub fn resume(&self) -> bool {
        if !self.paused() {
            return false;
        }
        self.state.set(CompilerState::Executing);
        true
    }

    /// `terminate`.
    pub fn terminate(&self) -> bool {
        if self.paused() {
            self.resume();
        }
        if !self.executing() {
            return false;
        }
        self.state.set(CompilerState::Terminating);
        true
    }

    /// `getThreadCount`.
    pub fn get_thread_count(&self) -> u32 {
        self.num_threads.get()
    }

    /// `setThreadCount` (the `BgTaskManager` resize is a dropped concern).
    pub fn set_thread_count(&self, num_threads: u32) {
        self.num_threads.set(num_threads);
    }

    /// `getIntermediatePath`.
    pub fn get_intermediate_path(&self) -> &str {
        &self.intermediate_path
    }

    /// `setIntermediatePath`.
    pub fn set_intermediate_path(&mut self, path: &str) {
        self.intermediate_path = path.to_string();
    }

    /// `getOutputPath`.
    pub fn get_output_path(&self) -> &str {
        &self.output_path
    }

    /// `setOutputPath`.
    pub fn set_output_path(&mut self, path: &str) {
        self.output_path = path.to_string();
    }

    /// `getForceRebuild`.
    pub fn get_force_rebuild(&self) -> bool {
        self.force_rebuild.get()
    }

    /// `setForceRebuild`: store the flag and forward it to the processor.
    pub fn set_force_rebuild(&mut self, force: bool) {
        self.force_rebuild.set(force);
        self.task_processor.set_force_rebuild(force);
    }

    /// `getRecursive`.
    pub fn get_recursive(&self) -> bool {
        self.recursive.get()
    }

    /// `setRecursive`. The port always processes sub-dependencies inline (the
    /// reference's recursive branch), so this flag is informational only.
    pub fn set_recursive(&self, recursive: bool) {
        self.recursive.set(recursive);
    }

    /// `registerConversionRule`: append a rule to the shared list (the finder
    /// sees it immediately).
    pub fn register_conversion_rule(&self, rule: Box<dyn ConversionRule>) {
        self.conversion_rules.borrow_mut().push(rule);
    }

    /// `registerConverter`: insert into the id-keyed registry (a duplicate id
    /// is a programmer error, like the reference's uniqueness assert).
    pub fn register_converter(&self, info: ConverterInfo) {
        let previous = self.converter_map.borrow_mut().insert(info.type_id, info);
        debug_assert!(previous.is_none(), "duplicate converter id registered");
    }

    /// Cache configuration for the `AssetCompilerOptions` (`setCachePath`,
    /// `setReadFromCache`, `setWriteToCache`).
    pub fn set_cache_path(&mut self, cache_path: impl Into<String>) {
        self.content_cache.set_cache_path(cache_path);
    }
    pub fn set_cache_read(&mut self, read: bool) {
        self.content_cache.set_read_enabled(read);
    }
    pub fn set_cache_write(&mut self, write: bool) {
        self.content_cache.set_write_enabled(write);
    }

    /// `ContentAddressableCache` state (read by `AssetCompilerOptions`).
    pub fn get_cache_path(&self) -> &str {
        self.content_cache.cache_path()
    }
    pub fn get_cache_read(&self) -> bool {
        self.content_cache.read_enabled()
    }
    pub fn get_cache_write(&self) -> bool {
        self.content_cache.write_enabled()
    }

    /// `processTasks`: drive the task queue to completion (the batch driver
    /// calls this after `build` populates the queue).
    pub fn process_tasks(&self) {
        self.task_processor.process_tasks(self, &self.resource);
    }

    /// `taskFinder.getTask( file )`: the task for `file` (creating it).
    /// The batch driver's `build` uses this for a single-file input.
    pub fn get_task(&self, file: &str) -> TaskHandle {
        self.task_finder.get_task(self, file)
    }

    /// `taskFinder.findTasks( directory )`: discover and queue root tasks.
    pub fn find_tasks(&self, directory: &str) {
        self.task_finder.find_tasks(self, directory);
    }

    /// `taskQueue_.empty()`.
    pub fn task_queue_empty(&self) -> bool {
        self.task_queue.borrow().is_empty()
    }

    /// `taskQueue_.size()`.
    pub fn task_queue_size(&self) -> usize {
        self.task_queue.borrow().len()
    }

    /// `BWResource::instance().fileSystem()->getFileType( path )`.
    pub fn file_type(&self, path: &str) -> FileType {
        self.resource.get_file_type(path, None)
    }

    /// The wrapped `BwResource` (the batch driver reports its paths).
    pub fn resource(&self) -> &Rc<BwResource> {
        &self.resource
    }

    /// `ensureCompiled`: make sure `compiledFileName` is up to date. `true`
    /// when it is, plus the task resolved for it (the reference's
    /// `ConversionTask*&` out-parameter). Sub-dependencies run inline; a
    /// cyclic dependency fails the sub-task.
    fn ensure_compiled(
        &self,
        compiled_file_name: &str,
        output: bool,
    ) -> (bool, Option<TaskHandle>) {
        if let Some(source_file) = self.get_source_file(compiled_file_name) {
            let task = self.task_finder.get_task(self, &source_file);

            // The recursive branch: take the task over and process it inline.
            let mut should_process = false;
            {
                let mut t = task.borrow_mut();
                match t.status {
                    ConversionStatus::New => {
                        t.status = ConversionStatus::Processing;
                        should_process = true;
                    }
                    ConversionStatus::Queued => {
                        // Remove it from the queue (it is being pulled forward).
                        let mut queue = self.task_queue.borrow_mut();
                        if let Some(pos) = queue.iter().position(|q| Rc::ptr_eq(q, &task)) {
                            queue.remove(pos);
                        }
                        t.status = ConversionStatus::Processing;
                        should_process = true;
                    }
                    _ => {}
                }
            }

            if should_process {
                // Suspend the parent (its source is restored after).
                let saved = self
                    .current_task_source
                    .replace(Some(task.borrow().source.clone()));
                self.on_task_started(&task.borrow());
                self.task_processor
                    .process_task(self, &self.resource, &task);
                self.on_task_completed(&task.borrow());
                *self.current_task_source.borrow_mut() = saved;
            }

            let status = task.borrow().status;
            if status != ConversionStatus::Done && status != ConversionStatus::Failed {
                // Still mid-flight after processing: it is an ancestor of the
                // current task, i.e. a cyclic dependency. Fail it so the
                // parent (and everything above) fails instead of spinning.
                eprintln!("Cyclic dependency found {}", task.borrow().source);
                task.borrow_mut().status = ConversionStatus::Failed;
            }

            let done = task.borrow().status == ConversionStatus::Done;
            return (done, Some(task));
        }

        if output {
            // No rule maps this file: copy it from a source if one exists.
            let mut source_path = compiled_file_name.to_string();
            self.resolve_source_path(&mut source_path);
            if self.resource.file_absolutely_exists(&source_path) {
                if self.output_path.is_empty() {
                    // The source is used in place; no output path configured.
                    return (true, None);
                }

                let mut output_path = compiled_file_name.to_string();
                self.resolve_output_path(&mut output_path);
                if self.resource.file_absolutely_exists(&output_path)
                    && self.get_file_hash(&source_path, false)
                        == self.get_file_hash(&output_path, false)
                {
                    // The output already matches the source.
                    return (true, None);
                }

                if copy_file_or_directory(&source_path, &output_path) {
                    self.get_file_hash(&output_path, true);
                    return (true, None);
                }
            }
        }

        // No way to compile this file (the reference's `s_invalidTask`).
        (false, None)
    }

    /// `sanitisePathParam`: make `pathParam` absolute, canonicalised and
    /// slash-normalised, with a trailing `/` for a directory or a path that
    /// does not exist yet.
    pub fn sanitise_path_param(path_param: &str) -> String {
        let mut path = path_param.to_string();
        if path.is_empty() || path_is_relative(&path) {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            path = format!("{cwd}{path}");
        }
        path = path.replace('\\', "/");
        // Canonicalise the parent chain (the reference's PathCanonicalizeW);
        // a non-existent leaf is appended back.
        let leaf = resources::bwresource::get_filename(&path).to_string();
        let parent = resources::bwresource::get_file_path(&path);
        let canonical = std::fs::canonicalize(&parent)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or(parent);
        let mut canonical = format!("{}/{}", canonical.trim_end_matches('/'), leaf);
        if canonical == "/" {
            canonical = canonical.trim_end_matches('/').to_string();
        }

        // A directory (or a path not on disk) gains a trailing slash.
        let ft = std::fs::metadata(&canonical)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if (ft || !std::path::Path::new(&canonical).exists()) && !canonical.ends_with('/') {
            canonical.push('/');
        }
        canonical
    }
}

impl Compiler for AssetCompiler {
    fn get_resource_paths(&self) -> &[String] {
        &self.resource_paths
    }

    fn get_hash(&self, dependency: &Dependency) -> u64 {
        match dependency {
            Dependency::SourceFile(d) => {
                // The hash of a file dependency is a hash of its file on disk.
                let mut filename = d.file_name().to_string();
                let mut hash = hash64::compute_str(&filename);
                self.resolve_source_path(&mut filename);
                hash64::combine(
                    &mut hash,
                    hash64::compute_u64(self.get_file_hash(&filename, false)),
                );
                hash
            }
            Dependency::IntermediateFile(d) => {
                let mut filename = d.file_name().to_string();
                let mut hash = hash64::compute_str(&filename);
                self.resolve_intermediate_path(&mut filename);
                hash64::combine(
                    &mut hash,
                    hash64::compute_u64(self.get_file_hash(&filename, false)),
                );
                hash
            }
            Dependency::OutputFile(d) => {
                let mut filename = d.file_name().to_string();
                let mut hash = hash64::compute_str(&filename);
                self.resolve_output_path(&mut filename);
                hash64::combine(
                    &mut hash,
                    hash64::compute_u64(self.get_file_hash(&filename, false)),
                );
                hash
            }
            Dependency::Converter(d) => {
                let mut hash = d.converter_id();
                hash64::combine(&mut hash, hash64::compute_str(d.converter_version()));
                hash
            }
            Dependency::ConverterParams(d) => hash64::compute_str(d.converter_params()),
            Dependency::Directory(d) => {
                let mut directory = d.directory().to_string();
                let mut hash = hash64::compute_str(&directory);
                if directory.is_empty() {
                    let num_paths = self.resource.get_path_num();
                    for index in (0..num_paths).rev() {
                        let path = self.resource.get_path(index).to_string();
                        let sub_hash = self.get_directory_hash(
                            &path,
                            d.pattern(),
                            d.is_regex(),
                            d.is_recursive(),
                        );
                        hash64::combine(&mut hash, hash64::compute_u64(sub_hash));
                    }
                } else {
                    self.resolve_source_path(&mut directory);
                    let sub_hash = self.get_directory_hash(
                        &directory,
                        d.pattern(),
                        d.is_regex(),
                        d.is_recursive(),
                    );
                    hash64::combine(&mut hash, hash64::compute_u64(sub_hash));
                }
                hash
            }
        }
    }

    fn get_file_hash(&self, file_name: &str, force_update: bool) -> u64 {
        debug_assert!(
            !path_is_relative(file_name),
            "getFileHash on a relative path"
        );

        if !force_update && let Some(&hash) = self.file_hashes.borrow().get(file_name) {
            return hash;
        }

        // A missing file hashes to 0 (the reference's `hash = 0` default).
        let mut hash = 0;
        if let Some(data) = self.resource.file_system().read_file(file_name) {
            hash = hash64::compute_bytes(&data);
        }

        self.file_hashes
            .borrow_mut()
            .insert(file_name.to_string(), hash);
        hash
    }

    fn check_file_hash_up_to_date(&self, file_name: &str) -> bool {
        let old_hash = match self.file_hashes.borrow().get(file_name) {
            Some(&hash) => hash,
            None => return false,
        };
        let new_hash = self.get_file_hash(file_name, true);
        old_hash == new_hash
    }

    fn get_directory_hash(
        &self,
        directory: &str,
        pattern: &str,
        regex: bool,
        recursive: bool,
    ) -> u64 {
        let mut hash = 0;
        let path = format_path(directory);

        let mut dir = self
            .resource
            .file_system()
            .read_directory(directory)
            .unwrap_or_default();
        dir.sort();

        let compiled = if regex {
            Some(regex::Regex::new(&format!("^(?:{pattern})$")).ok())
        } else {
            None
        };

        for entry in dir {
            let sub_path = format!("{path}{entry}");
            let ft = self.resource.file_system().get_file_type(&sub_path, None);
            if ft == FileType::File {
                let matches = match &compiled {
                    Some(Some(re)) => re.is_match(&entry),
                    Some(None) => false,
                    None => entry == pattern,
                };
                if matches {
                    hash64::combine(&mut hash, hash64::compute_str(&entry));
                    hash64::combine(
                        &mut hash,
                        hash64::compute_u64(self.get_file_hash(&sub_path, false)),
                    );
                }
            } else if ft == FileType::Directory && recursive {
                let sub_hash = self.get_directory_hash(&sub_path, pattern, regex, recursive);
                if sub_hash != 0 {
                    hash64::combine(&mut hash, hash64::compute_u64(sub_hash));
                }
            }
        }

        hash
    }

    fn ensure_up_to_date(&self, dependency: &Dependency) -> (bool, Option<TaskHandle>) {
        match dependency {
            Dependency::SourceFile(d) => (self.resource.file_exists(d.file_name()), None),
            Dependency::IntermediateFile(d) => self.ensure_compiled(d.file_name(), false),
            Dependency::OutputFile(d) => self.ensure_compiled(d.file_name(), true),
            _ => (true, None),
        }
    }

    fn get_source_file(&self, file: &str) -> Option<String> {
        for rule in self.conversion_rules.borrow().iter().rev() {
            if let Some(source) = rule.get_source_file(file) {
                return Some(source);
            }
        }
        None
    }

    fn queue_task(&self, task: TaskHandle) {
        let mut t = task.borrow_mut();
        if t.status == ConversionStatus::New {
            self.task_queue.borrow_mut().push_back(task.clone());
            t.status = ConversionStatus::Queued;
        }
    }

    fn get_next_task(&self) -> Option<TaskHandle> {
        if self.terminating() {
            return None;
        }
        let task = self.task_queue.borrow_mut().pop_front();
        if let Some(t) = &task {
            let mut t = t.borrow_mut();
            if t.status == ConversionStatus::Queued {
                t.status = ConversionStatus::Processing;
            }
        }
        task
    }

    fn has_tasks(&self) -> bool {
        if self.terminating() {
            return false;
        }
        !self.task_queue.borrow().is_empty()
    }

    fn set_error(&self) {
        self.error.set(true);
    }
    fn set_warning(&self) {
        self.warning.set(true);
    }
    fn reset_error_flags(&self) {
        self.error.set(false);
        self.warning.set(false);
    }
    fn has_error(&self) -> bool {
        self.error.get()
    }
    fn has_warning(&self) -> bool {
        self.warning.get()
    }

    fn resolve_relative_path(&self, path: &mut String) -> bool {
        if path_is_relative(path) {
            return true;
        }
        if !self.intermediate_path.is_empty() && path.starts_with(&self.intermediate_path) {
            *path = path[self.intermediate_path.len()..].to_string();
            return true;
        }
        if !self.output_path.is_empty() && path.starts_with(&self.output_path) {
            *path = path[self.output_path.len()..].to_string();
            return true;
        }
        self.resource.resolve_to_relative_path(path)
    }

    fn resolve_source_path(&self, path: &mut String) -> bool {
        if !self.resolve_relative_path(path) {
            return false;
        }
        *path = self.resource.resolve_filename(path);
        true
    }

    fn resolve_intermediate_path(&self, path: &mut String) -> bool {
        self.resolve_custom_path(&self.intermediate_path, path)
    }

    fn resolve_output_path(&self, path: &mut String) -> bool {
        self.resolve_custom_path(&self.output_path, path)
    }

    fn cache_read(&self, file_name: &str, hash: u64) -> bool {
        let ok = self.content_cache.read_from_cache(file_name, hash);
        if ok {
            self.on_cache_read(file_name);
        } else {
            self.on_cache_read_miss(file_name);
        }
        ok
    }

    fn cache_write(&self, file_name: &str, hash: u64) -> bool {
        let ok = self.content_cache.write_to_cache(file_name, hash);
        if ok {
            self.on_cache_write(file_name);
        } else {
            self.on_cache_write_miss(file_name);
        }
        ok
    }

    fn should_iterate_file(&self, file: &str) -> bool {
        if !self.should_iterate_file_base(file) {
            return false;
        }
        // The hook observes the pass (the reference's derived override
        // increments its counter here) and may veto.
        self.hook
            .borrow()
            .as_ref()
            .is_none_or(|hook| hook.should_iterate_file(file))
    }

    fn should_iterate_directory(&self, directory: &str) -> bool {
        if !self.should_iterate_directory_base(directory) {
            return false;
        }
        self.hook
            .borrow()
            .as_ref()
            .is_none_or(|hook| hook.should_iterate_directory(directory))
    }

    fn on_task_started(&self, task: &ConversionTask) {
        // The hook first (the reference's derived override), then the base.
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_task_started(task);
        }
        *self.current_task_source.borrow_mut() = Some(task.source.clone());
        // Reset the source file's hash so a change is caught (the reference
        // opens a read handle to the source here too).
        self.get_file_hash(&task.source, true);
    }

    fn on_task_resumed(&self, task: &ConversionTask) {
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_task_resumed(task);
        }
        *self.current_task_source.borrow_mut() = Some(task.source.clone());
    }

    fn on_task_suspended(&self, task: &ConversionTask) {
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_task_suspended(task);
        }
        // The single-threaded recursive model never suspends; clear the
        // current task like the reference's `s_currentTask = NULL`.
        *self.current_task_source.borrow_mut() = None;
    }

    fn on_task_completed(&self, task: &ConversionTask) {
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_task_completed(task);
        }
        self.reset_error_flags();
        *self.current_task_source.borrow_mut() = None;
    }

    fn on_pre_create_dependencies(&self, task: &ConversionTask) {
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_pre_create_dependencies(task);
        }
    }

    fn on_pre_convert(&self, task: &ConversionTask) {
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_pre_convert(task);
        }
    }

    fn on_output_generated(&self, filename: &str) {
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_output_generated(filename);
        }
    }

    fn on_cache_read(&self, filename: &str) {
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_cache_read(filename);
        }
    }

    fn on_cache_read_miss(&self, filename: &str) {
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_cache_read_miss(filename);
        }
    }

    fn on_cache_write(&self, filename: &str) {
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_cache_write(filename);
        }
    }

    fn on_cache_write_miss(&self, filename: &str) {
        if let Some(hook) = self.hook.borrow().as_ref() {
            hook.on_cache_write_miss(filename);
        }
    }
}

impl TaskCompiler for AssetCompiler {
    fn should_iterate_file(&self, file: &str) -> bool {
        Compiler::should_iterate_file(self, file)
    }
    fn should_iterate_directory(&self, directory: &str) -> bool {
        Compiler::should_iterate_directory(self, directory)
    }
    fn queue_task(&self, task: TaskHandle) {
        Compiler::queue_task(self, task)
    }
}

impl AssetCompiler {
    /// `shouldIterateFile` base: the terminating exclusion (the reference's
    /// `AssetCompiler::shouldIterateFile`).
    fn should_iterate_file_base(&self, _file: &str) -> bool {
        !self.terminating()
    }

    /// `shouldIterateDirectory` base: the terminating / intermediate /
    /// output / svn / testfiles exclusions (the reference's
    /// `AssetCompiler::shouldIterateDirectory`).
    fn should_iterate_directory_base(&self, directory: &str) -> bool {
        if self.terminating() {
            return false;
        }
        // Don't iterate into the intermediate or output paths.
        if !self.intermediate_path.is_empty() && directory.starts_with(&self.intermediate_path) {
            return false;
        }
        if !self.output_path.is_empty() && directory.starts_with(&self.output_path) {
            return false;
        }
        // Don't iterate into svn folders or the unit-test testfiles.
        if directory.contains("/.svn") {
            return false;
        }
        if directory.contains("/tools/asset_pipeline/testfiles") {
            return false;
        }
        true
    }

    /// `resolveCustomPath`: resolve `path` against `basePath`, choosing the
    /// source path component when the base is empty (see the module docs).
    fn resolve_custom_path(&self, base_path: &str, path: &mut String) -> bool {
        let mut actual_base_path = base_path.to_string();

        if base_path.is_empty() {
            // Resolve the base to the resource path component of the current
            // task's source (the reference asserts `s_currentTask != NULL`).
            let source_path = self
                .current_task_source
                .borrow()
                .clone()
                .unwrap_or_default();
            for resource_path in &self.resource_paths {
                if source_path.starts_with(resource_path)
                    && resource_path.len() > actual_base_path.len()
                {
                    actual_base_path = resource_path.clone();
                    if !actual_base_path.ends_with('/') {
                        actual_base_path.push('/');
                    }
                }
            }
        }

        if path.starts_with(&actual_base_path) {
            // Already an absolute custom path.
            return true;
        }

        if !self.resolve_relative_path(path) {
            return false;
        }

        *path = format!("{actual_base_path}{path}");
        true
    }
}

/// `MultiFileSystem::copyFileOrDirectory`: copy a file or directory tree on
/// the native file system (the pipeline only copies absolute source/output
/// paths, which the native base system handles).
fn copy_file_or_directory(source: &str, destination: &str) -> bool {
    let meta = match std::fs::metadata(source) {
        Ok(meta) => meta,
        Err(_) => return false,
    };
    if meta.is_dir() {
        if std::fs::create_dir_all(destination).is_err() {
            return false;
        }
        let entries = match std::fs::read_dir(source) {
            Ok(entries) => entries,
            Err(_) => return false,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let src = format!("{}/{}", source.trim_end_matches('/'), name);
            let dst = format!("{}/{}", destination.trim_end_matches('/'), name);
            if !copy_file_or_directory(&src, &dst) {
                return false;
            }
        }
        true
    } else {
        std::fs::copy(source, destination).is_ok()
    }
}
