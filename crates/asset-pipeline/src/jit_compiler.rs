//! `JITCompiler` port (`tools/jit_compiler/jit_compiler.{hpp,cpp}` plus
//! `jit_compiler_options.{hpp,cpp}` and the `AssetServer` base): the
//! compiler that rebuilds assets *on demand* — it indexes which tasks
//! depend on which files (the reverse-dependency maps), re-queues exactly
//! those tasks when a file changes, and lets a consumer request an asset so
//! its task jumps the queue.
//!
//! Reference quirks preserved:
//! - `onAssetRequested` is the on-demand entry: it maps the requested asset
//!   back to its source, pulls the source's task to the *front* of the
//!   queue (a queued task is pulled out and re-queued; only `NEW`/`QUEUED`
//!   tasks move), records the request so the asset is broadcast when the
//!   task completes, and signals the managing thread;
//! - a requested asset nobody can build (no rule maps it, unknown
//!   converter) is broadcast immediately as done;
//! - a `DONE` task whose source has no pending modification is broadcast
//!   without a rebuild (the port has no modification monitor, so this
//!   branch always reads "no pending modification");
//! - `onTaskCompleted` reads the task's saved `.deps` list and registers a
//!   reverse dependency for every input and (when the task did not fail)
//!   every output; a task whose list has no primary inputs keeps at least
//!   its own source as the reverse dependency;
//! - `collectReverseDependencies` removes a collected task from *all* the
//!   reverse lists it appears in (through the forward map) before it is
//!   re-queued, so the re-run repopulates the maps cleanly; a directory
//!   dependency releases its dependents once (the entry is erased on the
//!   first matching file);
//! - `onResourceModified` purges the caches, creates a root task when a
//!   file was *added*, collects the dependents both by path and by
//!   directory entry, resets each dependent (status, sub tasks, store
//!   record) and re-queues it — unless its source was deleted by this
//!   modification, or the dependent's own source is already gone;
//! - `stop` terminates the compiler (letting in-flight work finish) and
//!   wakes the managing thread;
//! - `lock`/`unlock` (the `AssetServer` face a consumer holds while reading
//!   outputs) pause and resume the compiler;
//! - `JitCompilerOptions` extends `AssetCompilerOptions` with
//!   `enableBalloonNotifications` (default on).
//!
//! Port deviations (structural, behaviour preserved):
//! - the reference runs a scanning thread, a managing thread and a Windows
//!   named-pipe `AssetServer`. The port is single-threaded: `scanning_main`
//!   and `managing_step` are the same work expressed as driver-invoked
//!   steps (a driver loop plays the thread bodies), the `SimpleEvent`
//!   becomes a flag `managing_step` drains (the semaphore/monitor-flush
//!   dance has no monitor to flush), and `broadcastAsset` appends to a log
//!   plus an optional callback instead of writing to connected pipes;
//! - the resource modification monitor (`BWResource::
//!   enableModificationMonitor`) is not ported; the driver feeds
//!   `on_resource_modified` directly (after a managing step, mirroring the
//!   reference's flush-after-process ordering);
//! - the reference keys its maps by `ConversionTask*`; here the maps key by
//!   the task's *source path* (the finder's source-path cache is the task
//!   identity table — [`AssetCompiler::get_cached_task`] resolves a handle
//!   back), and the current task is tracked by its inner address;
//! - `TaskStore`/`TaskInfo` live in [`crate::task_store`] (the four task
//!   lists and the per-task log/outputs survive; the UI signals do not);
//! - the [`CompilerHook`] seam exposes only the `onPre*` half of the
//!   create/convert phase pair, so the `CHECKING`-after states the
//!   reference records in `onPostCreateDependencies`/`onPostConvert` are
//!   not observable.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use resources::bwresource::{self, BwResource, path_is_relative};
use resources::datasection::DataSection;
use resources::zip::FileType;

use crate::asset_compiler::AssetCompiler;
use crate::asset_compiler_options::AssetCompilerOptions;
use crate::compiler::{Compiler, CompilerHook};
use crate::conversion_task::{ConversionStatus, ConversionTask, UNKNOWN_ID};
use crate::dependency::Dependency;
use crate::dependency_list::{DependencyList, HashProvider};
use crate::task_store::{SharedTaskStore, TaskInfoState, TaskStore, task_key};

/// A read-only [`DependencyList`] still needs a [`HashProvider`] (never
/// invoked on the read path — the hook only walks the deserialised
/// inputs/outputs).
struct StoredHashes;

impl HashProvider for StoredHashes {
    fn get_hash(&self, _dependency: &Dependency) -> u64 {
        0
    }
}

/// `JitCompilerOptions` (`jit_compiler_options.{hpp,cpp}`): the batch
/// options plus the balloon-notification flag.
pub struct JitCompilerOptions {
    /// The inherited `AssetCompilerOptions`.
    pub base: AssetCompilerOptions,
    /// `enableBalloonNotifications_` (default on).
    enable_balloon_notifications: bool,
}

impl Default for JitCompilerOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl JitCompilerOptions {
    /// `JitCompilerOptions()`.
    pub fn new() -> Self {
        Self {
            base: AssetCompilerOptions::new(),
            enable_balloon_notifications: true,
        }
    }

    /// `JitCompilerOptions( compiler )`: inherit the batch settings.
    pub fn from_compiler(compiler: &AssetCompiler) -> Self {
        Self {
            base: AssetCompilerOptions::from_compiler(compiler),
            enable_balloon_notifications: true,
        }
    }

    /// `enableBalloonNotifications`.
    pub fn enable_balloon_notifications(&self) -> bool {
        self.enable_balloon_notifications
    }

    /// `enableBalloonNotifications( enable )`.
    pub fn set_enable_balloon_notifications(&mut self, enable: bool) {
        self.enable_balloon_notifications = enable;
    }

    /// Apply the batch options onto `compiler` (the base class's `apply`).
    pub fn apply(&mut self, compiler: &mut AssetCompiler) {
        self.base.apply(compiler);
    }
}

/// The broadcast consumer (the reference's connected pipe clients).
pub type BroadcastCallback = Rc<dyn Fn(&str)>;

/// The modification kinds `onResourceModified` distinguishes (the
/// reference's `Action` from the modification listener).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceAction {
    /// `ACTION_ADDED` — a new file appeared (a root task may be created).
    Added,
    /// `ACTION_REMOVED` — a file disappeared.
    Removed,
    /// `ACTION_MODIFIED` — a file's content changed.
    Modified,
}

/// The JIT's shared state: the reverse/forward dependency maps, the pending
/// requests, the wake flag and the broadcast log. [`JitCompiler`] and its
/// installed [`JitHook`] share one `Rc`.
struct JitShared {
    /// `directoryDependencies_`: the `dir>pattern>regex>recursive` keys.
    directory_dependencies: RefCell<Vec<String>>,
    /// `reverseDependencyMap_`: path → `[(task source, isOutput)]`.
    reverse_dependencies: RefCell<HashMap<String, Vec<(String, bool)>>>,
    /// `forwardDependencyMap_`: task source → the paths it is registered
    /// under (the cleanup index `collectReverseDependencies` walks).
    forward_dependencies: RefCell<HashMap<String, Vec<String>>>,
    /// `requests_`: `(task source, asset)` pairs awaiting their completion
    /// broadcast (deduplicated on insert).
    requests: RefCell<Vec<(String, String)>>,
    /// `event_`: the managing-thread wake flag (`managing_step` drains it).
    event: Cell<bool>,
    /// `st_currentTask_`: the address of the task currently being
    /// processed (0 when none).
    current_task_addr: Cell<usize>,
    /// `intermediatePath_` / `outputPath_` mirrors (the hook resolves the
    /// `.deps` and output paths without reaching the compiler seam).
    intermediate_path: RefCell<String>,
    output_path: RefCell<String>,
    /// The resource paths (the empty-base fallback of `resolveCustomPath`).
    resource_paths: RefCell<Vec<String>>,
    /// The `AssetServer::broadcastAsset` log.
    broadcasts: RefCell<Vec<String>>,
    /// The optional broadcast consumer (the connected editor, in the
    /// reference's pipe protocol).
    broadcast_callback: RefCell<Option<BroadcastCallback>>,
}

impl JitShared {
    fn new() -> Self {
        Self {
            directory_dependencies: RefCell::new(Vec::new()),
            reverse_dependencies: RefCell::new(HashMap::new()),
            forward_dependencies: RefCell::new(HashMap::new()),
            requests: RefCell::new(Vec::new()),
            event: Cell::new(false),
            current_task_addr: Cell::new(0),
            intermediate_path: RefCell::new(String::new()),
            output_path: RefCell::new(String::new()),
            resource_paths: RefCell::new(Vec::new()),
            broadcasts: RefCell::new(Vec::new()),
            broadcast_callback: RefCell::new(None),
        }
    }

    /// `resolveCustomPath` for the hook: resolve `path` against `base`,
    /// falling back to `source`'s resource path component when the base is
    /// empty (the [`AssetCompiler`] copy; the hook cannot reach the
    /// compiler seam, so the resource does the relative strip).
    fn resolve_custom_path(
        &self,
        base_path: &str,
        path: &mut String,
        source: &str,
        resource: &BwResource,
    ) {
        let mut actual_base_path = base_path.to_string();

        if base_path.is_empty() {
            for resource_path in self.resource_paths.borrow().iter() {
                if source.starts_with(resource_path) && resource_path.len() > actual_base_path.len()
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
            return;
        }

        // The relative check of `resolveRelativePath`: a relative path
        // stays relative and the base is simply prepended.
        if !path_is_relative(path) && !resource.resolve_to_relative_path(path) {
            return;
        }
        *path = format!("{actual_base_path}{path}");
    }

    /// `resolveIntermediatePath( path )`.
    fn resolve_intermediate_path(&self, path: &mut String, source: &str, resource: &BwResource) {
        let base = self.intermediate_path.borrow().clone();
        self.resolve_custom_path(&base, path, source, resource);
    }

    /// `resolveOutputPath( path )`.
    fn resolve_output_path(&self, path: &mut String, source: &str, resource: &BwResource) {
        let base = self.output_path.borrow().clone();
        self.resolve_custom_path(&base, path, source, resource);
    }

    /// `broadcastAsset`: record the broadcast and hand it to the consumer.
    fn broadcast(&self, asset: &str) {
        self.broadcasts.borrow_mut().push(asset.to_string());
        if let Some(callback) = self.broadcast_callback.borrow().as_ref() {
            callback(asset);
        }
    }
}

/// `JitHook`: the [`CompilerHook`] the JIT compiler installs — the derived
/// overrides of the reference's `JITCompiler` (task states into the store,
/// request broadcasts on completion, and the `.deps`-driven
/// reverse-dependency registration).
struct JitHook {
    shared: Rc<JitShared>,
    store: SharedTaskStore,
    resource: Rc<BwResource>,
}

impl JitHook {
    /// `addReverseDependency( path, isOutput, conversionTask )`
    /// (jit_compiler.cpp:94-112).
    fn add_reverse_dependency(&self, path: &str, is_output: bool, source: &str) {
        self.shared
            .reverse_dependencies
            .borrow_mut()
            .entry(path.to_string())
            .or_default()
            .push((source.to_string(), is_output));
        self.shared
            .forward_dependencies
            .borrow_mut()
            .entry(source.to_string())
            .or_default()
            .push(path.to_string());
    }

    /// `addReverseDependency( directory, pattern, regex, recursive, task )`
    /// (jit_compiler.cpp:114-131): key the directory entry and index it.
    /// The key stores the directory *without* its trailing slash — the
    /// consumer re-appends it (the reference's `tokens[0] + "/"`).
    fn add_reverse_dependency_directory(
        &self,
        directory: &str,
        pattern: &str,
        regex: bool,
        recursive: bool,
        source: &str,
    ) {
        let directory = directory.trim_end_matches('/');
        let path = format!(
            "{directory}>{pattern}>{}>{}",
            u8::from(regex),
            u8::from(recursive)
        );
        let mut entries = self.shared.directory_dependencies.borrow_mut();
        if !entries.contains(&path) {
            entries.push(path.clone());
        }
        drop(entries);
        self.add_reverse_dependency(&path, false, source);
    }

    /// `addReverseDependency( dependency, conversionTask )`
    /// (jit_compiler.cpp:133-197): route the dependency's kind to the right
    /// path form (file dependencies resolve first; a directory dependency
    /// with an empty directory expands over every resource path).
    fn add_reverse_dependency_from(&self, dependency: &Dependency, source: &str) {
        match dependency {
            Dependency::SourceFile(d) => {
                let mut filename = d.file_name().to_string();
                // resolveSourcePath( filename ): relative-strip (a relative
                // path stays) then resource-path anchoring.
                if !path_is_relative(&filename) {
                    let _ = self.resource.resolve_to_relative_path(&mut filename);
                }
                filename = self.resource.resolve_filename(&filename);
                self.add_reverse_dependency(&filename, false, source);
            }
            Dependency::IntermediateFile(d) => {
                let mut filename = d.file_name().to_string();
                self.shared
                    .resolve_intermediate_path(&mut filename, source, &self.resource);
                self.add_reverse_dependency(&filename, false, source);
            }
            Dependency::OutputFile(d) => {
                let mut filename = d.file_name().to_string();
                self.shared
                    .resolve_output_path(&mut filename, source, &self.resource);
                self.add_reverse_dependency(&filename, false, source);
            }
            Dependency::Directory(d) => {
                let directory = d.directory();
                if directory.is_empty() {
                    for index in (0..self.resource.get_path_num()).rev() {
                        self.add_reverse_dependency_directory(
                            self.resource.get_path(index),
                            d.pattern(),
                            d.is_regex(),
                            d.is_recursive(),
                            source,
                        );
                    }
                } else {
                    let mut resolved = directory.to_string();
                    if !path_is_relative(&resolved) {
                        let _ = self.resource.resolve_to_relative_path(&mut resolved);
                    }
                    resolved = self.resource.resolve_filename(&resolved);
                    self.add_reverse_dependency_directory(
                        resolved.trim_end_matches('/'),
                        d.pattern(),
                        d.is_regex(),
                        d.is_recursive(),
                        source,
                    );
                }
            }
            // The reference's switch has no case for converter/params
            // dependencies: they register no reverse dependency.
            Dependency::Converter(_) | Dependency::ConverterParams(_) => {}
        }
    }

    /// `collectReverseDependencies( path, includeOutputs, conversionTasks )`
    /// (jit_compiler.cpp:244-286): every task registered under `path` (and,
    /// when `include_outputs`, the output-kind registrations too) is
    /// collected and cleaned out of all its reverse lists via the forward
    /// map. Returns the collected task sources.
    fn collect_reverse_dependencies_path(
        &self,
        path: &str,
        include_outputs: bool,
        conversion_tasks: &mut Vec<String>,
    ) {
        loop {
            let mut reverse = self.shared.reverse_dependencies.borrow_mut();
            let Some(entries) = reverse.get_mut(path) else {
                return;
            };

            // The next collectible entry (the reference's loop skips
            // output-kind entries when `includeOutputs` is false).
            let mut index = 0;
            while index < entries.len() && !include_outputs && entries[index].1 {
                index += 1;
            }
            if index >= entries.len() {
                return;
            }

            let (source, _) = entries[index].clone();

            // Use the forward dependency map to clear this task out of all
            // the reverse dependency lists: it is about to be re-queued and
            // its completion will repopulate them.
            {
                let forward = self.shared.forward_dependencies.borrow();
                if let Some(paths) = forward.get(&source) {
                    for forward_path in paths {
                        if let Some(list) = reverse.get_mut(forward_path) {
                            list.retain(|(s, _)| s != &source);
                        }
                    }
                }
            }
            // The task's own forward list is spent.
            drop(reverse);
            if let Some(paths) = self
                .shared
                .forward_dependencies
                .borrow_mut()
                .get_mut(&source)
            {
                paths.clear();
            }

            conversion_tasks.push(source);
        }
    }

    /// `collectReverseDependencies( path, file, conversionTasks )`
    /// (jit_compiler.cpp:199-242): the directory-entry branch — every
    /// registered directory dependency whose directory covers `path` and
    /// whose pattern matches `file` releases its dependents (once; the
    /// entry is erased after a match).
    fn collect_reverse_dependencies_directory(
        &self,
        path: &str,
        file: &str,
        conversion_tasks: &mut Vec<String>,
    ) {
        let mut index = 0;
        while index < self.shared.directory_dependencies.borrow().len() {
            let entry = self.shared.directory_dependencies.borrow()[index].clone();
            let tokens: Vec<&str> = entry.split('>').collect();
            debug_assert!(tokens.len() == 4, "malformed directory dependency key");

            let directory = format!("{}/", tokens[0]);
            let pattern = tokens[1];
            let regex = tokens[2] == "1";
            let recursive = tokens[3] == "1";

            let covered =
                (!recursive && path == directory) || (recursive && path.starts_with(&directory));
            if covered {
                let hit = if regex {
                    regex::Regex::new(&format!("^(?:{pattern})$"))
                        .map(|re| re.is_match(file))
                        .unwrap_or(false)
                } else {
                    file == pattern
                };
                if hit {
                    self.collect_reverse_dependencies_path(&entry, false, conversion_tasks);
                    self.shared
                        .directory_dependencies
                        .borrow_mut()
                        .remove(index);
                    continue;
                }
            }
            index += 1;
        }
    }

    /// The dep-list registration of `onTaskCompleted`
    /// (jit_compiler.cpp:533-588): read the saved `.deps` and index every
    /// input and output under the resolved paths.
    fn register_dependency_list(&self, source: &str, status: ConversionStatus) {
        // DependencyList depList( *this ); <source>.deps; serialiseIn.
        let mut dep_list_file_name = format!("{source}.deps");
        self.shared
            .resolve_intermediate_path(&mut dep_list_file_name, source, &self.resource);
        self.resource
            .ensure_absolute_path_exists(&dep_list_file_name);
        let root = match self.resource.open_section(&dep_list_file_name, true) {
            Some(section) => (*section).clone(),
            None => DataSection::new("deps"),
        };
        let mut dep_list = DependencyList::new(StoredHashes);
        // A malformed or empty list is tolerated (the reference's lenient
        // `serialiseIn`): at least the source stays registered.
        let _ = dep_list.serialise_in(&root);

        if dep_list.primary_inputs().is_empty() {
            // Add a reverse dependency to the task's source file.
            self.add_reverse_dependency(source, false, source);
        }
        for (dependency, _hash) in dep_list.primary_inputs() {
            self.add_reverse_dependency_from(dependency, source);
        }
        for (dependency, _hash) in dep_list.secondary_inputs() {
            self.add_reverse_dependency_from(dependency, source);
        }

        if status != ConversionStatus::Failed {
            for (filename, _hash) in dep_list.intermediate_outputs() {
                let mut resolved = filename.clone();
                self.shared
                    .resolve_intermediate_path(&mut resolved, source, &self.resource);
                self.add_reverse_dependency(&resolved, true, source);
            }
            for (filename, _hash) in dep_list.outputs() {
                let mut resolved = filename.clone();
                self.shared
                    .resolve_output_path(&mut resolved, source, &self.resource);
                self.add_reverse_dependency(&resolved, true, source);
            }
        }
    }

    /// The request broadcast of `onTaskCompleted` (jit_compiler.cpp:510-526):
    /// every request recorded for this task is broadcast as done and
    /// dropped.
    fn broadcast_requests_for(&self, source: &str) {
        loop {
            let next = {
                let mut requests = self.shared.requests.borrow_mut();
                match requests.iter().position(|(task, _)| task == source) {
                    Some(index) => requests.remove(index).1,
                    None => break,
                }
            };
            self.shared.broadcast(&next);
        }
    }
}

impl CompilerHook for JitHook {
    /// `onTaskStarted` (jit_compiler.cpp:478-488): the store moves the task
    /// to the current list and records the `CHECKING` state.
    fn on_task_started(&self, task: &ConversionTask) {
        self.store.set_task_current(task);
        self.store
            .set_current_task_state(task, TaskInfoState::Checking);
        // MF_ASSERT( st_currentTask_ == nullptr ); st_currentTask_ = task.
        self.shared.current_task_addr.set(task_key(task));
    }

    /// `onTaskResumed` (jit_compiler.cpp:490-496).
    fn on_task_resumed(&self, task: &ConversionTask) {
        self.shared.current_task_addr.set(task_key(task));
    }

    /// `onTaskSuspended` (jit_compiler.cpp:498-504).
    fn on_task_suspended(&self, _task: &ConversionTask) {
        self.shared.current_task_addr.set(0);
    }

    /// `onTaskCompleted` (jit_compiler.cpp:506-591): state, request
    /// broadcasts, store move, then the `.deps`-driven reverse-dependency
    /// registration.
    fn on_task_completed(&self, task: &ConversionTask) {
        self.store.set_current_task_state(task, TaskInfoState::None);

        self.broadcast_requests_for(&task.source);

        self.store.set_task_complete(task);

        // MF_ASSERT( &conversionTask == st_currentTask_ );
        // st_currentTask_ = nullptr.
        self.shared.current_task_addr.set(0);

        self.register_dependency_list(&task.source, task.status);
    }

    /// `onPreCreateDependencies` (jit_compiler.cpp:593-597).
    fn on_pre_create_dependencies(&self, task: &ConversionTask) {
        self.store
            .set_current_task_state(task, TaskInfoState::GeneratingDependencies);
    }

    /// `onPreConvert` (jit_compiler.cpp:605-609).
    fn on_pre_convert(&self, task: &ConversionTask) {
        self.store
            .set_current_task_state(task, TaskInfoState::Converting);
    }

    /// `onOutputGenerated` (jit_compiler.cpp:617-625): attribute the output
    /// to the current task (the reference reads `st_currentTask_`).
    fn on_output_generated(&self, filename: &str) {
        let addr = self.shared.current_task_addr.get();
        if addr != 0 {
            self.store.add_output_to_task_key(addr, filename);
        }
    }
}

/// `JITCompiler`: the on-demand compiler (see the module docs).
pub struct JitCompiler {
    /// The wrapped `AssetCompiler` (the reference's base sub-object).
    compiler: AssetCompiler,
    /// `store_`.
    store: SharedTaskStore,
    /// The shared maps/requests/event/broadcast state.
    shared: Rc<JitShared>,
    /// The installed hook (the compiler holds a clone too; the JIT reaches
    /// the reverse-dependency collectors through it).
    hook: Rc<JitHook>,
    /// `JitCompilerOptions`.
    options: JitCompilerOptions,
}

impl JitCompiler {
    /// `JITCompiler( store )`: compose the [`AssetCompiler`], capture the
    /// tools' resource paths and install the hook (the reference's
    /// constructor also enables the modification monitor — not ported; the
    /// driver feeds `on_resource_modified` directly).
    pub fn new(resource: Rc<BwResource>) -> Self {
        let store = Rc::new(TaskStore::new());
        let shared = Rc::new(JitShared::new());
        let hook = Rc::new(JitHook {
            shared: shared.clone(),
            store: store.clone(),
            resource: resource.clone(),
        });
        let mut compiler = AssetCompiler::new(resource);
        compiler.add_tools_resource_paths();
        *shared.resource_paths.borrow_mut() = compiler.get_resource_paths().to_vec();
        compiler.set_hook(hook.clone());
        Self {
            compiler,
            store,
            shared,
            hook,
            options: JitCompilerOptions::new(),
        }
    }

    /// The wrapped compiler (rule/converter registration, path setters and
    /// the task-queue surface the driver may reach directly).
    pub fn compiler(&self) -> &AssetCompiler {
        &self.compiler
    }

    /// The wrapped compiler mutably (the plugin loader's registrar face and
    /// `initCompiler` — the reference reaches both through inheritance).
    pub fn compiler_mut(&mut self) -> &mut AssetCompiler {
        &mut self.compiler
    }

    /// `store_`.
    pub fn store(&self) -> &SharedTaskStore {
        &self.store
    }

    /// `options`.
    pub fn options(&self) -> &JitCompilerOptions {
        &self.options
    }

    /// The mutable options (the reference's config dialog writes these).
    pub fn options_mut(&mut self) -> &mut JitCompilerOptions {
        &mut self.options
    }

    /// `AssetServer`'s broadcast consumer: a callback invoked for every
    /// `broadcastAsset` (the reference's connected pipe clients).
    pub fn set_broadcast_callback(&self, callback: Option<BroadcastCallback>) {
        *self.shared.broadcast_callback.borrow_mut() = callback;
    }

    /// The broadcast log (the reference's `broadcast\n<path>` messages).
    pub fn broadcasts(&self) -> Vec<String> {
        self.shared.broadcasts.borrow().clone()
    }

    /// The pending `on_asset_requested` wake flag (the `SimpleEvent`).
    pub fn event_pending(&self) -> bool {
        self.shared.event.get()
    }

    /// `setIntermediatePath`: forward to the compiler and mirror it (the
    /// hook resolves the `.deps` paths against the mirror).
    pub fn set_intermediate_path(&mut self, path: &str) {
        self.compiler.set_intermediate_path(path);
        *self.shared.intermediate_path.borrow_mut() = path.to_string();
    }

    /// `setOutputPath`: forward to the compiler and mirror it.
    pub fn set_output_path(&mut self, path: &str) {
        self.compiler.set_output_path(path);
        *self.shared.output_path.borrow_mut() = path.to_string();
    }

    /// `scanningThreadMain` (jit_compiler.cpp:49-62): discover tasks in
    /// every resource path, newest path first, stopping when terminated.
    pub fn scanning_main(&self) {
        self.store.scanning_started();

        let num_paths = self.compiler.resource().get_path_num();
        for index in (0..num_paths).rev() {
            if self.compiler.terminating() {
                break;
            }
            let path = self.compiler.resource().get_path(index).to_string();
            self.compiler.find_tasks(&path);
        }

        self.store.scanning_stopped();
    }

    /// `managingThreadMain`'s body (jit_compiler.cpp:64-77) as one step:
    /// drain the wake flag, then drive the task queue. Returns whether the
    /// flag had been set (something was requested). The reference's
    /// `event_.wait( 1000 )` and the modification-monitor flush have no
    /// counterpart — the driver loop plays the wait and feeds
    /// `on_resource_modified` between steps.
    pub fn managing_step(&self) -> bool {
        let fired = self.shared.event.replace(false);
        self.compiler.process_tasks();
        fired
    }

    /// `stop` (jit_compiler.cpp:79-92): terminate the compiler (letting
    /// in-flight tasks finish) and wake the managing thread.
    pub fn stop(&self) {
        if self.compiler.terminating() {
            return;
        }
        self.compiler.terminate();
        self.shared.event.set(true);
    }

    /// `lock`/`unlock` (the `AssetServer` face): pause the compiler while a
    /// consumer reads outputs, resume afterwards.
    pub fn lock(&self) -> bool {
        // this will block until the asset compiler has been paused
        self.compiler.pause()
    }

    /// `unlock`.
    pub fn unlock(&self) -> bool {
        self.compiler.resume()
    }

    /// `broadcastAsset` (the `AssetServer` face): log the asset and hand it
    /// to the consumer.
    pub fn broadcast_asset(&self, asset: &str) {
        self.shared.broadcast(asset);
    }

    /// `onAssetRequested` (jit_compiler.cpp:298-373): the on-demand entry.
    pub fn on_asset_requested(&self, asset: &str) {
        if !self.compiler.executing() {
            return;
        }

        let Some(source_file) = self.compiler.get_source_file(asset) else {
            // Cannot build this asset - just broadcast it as done.
            self.broadcast_asset(asset);
            return;
        };

        // Get the task for this source file.
        let task = self.compiler.get_task(&source_file);
        if task.borrow().converter_id == UNKNOWN_ID {
            self.broadcast_asset(asset);
            return;
        }

        // Try to push the task to the start of the queue.
        self.compiler.pull_task_to_front(&task);

        if task.borrow().status >= ConversionStatus::Done {
            // The port has no modification monitor: no pending
            // modification — the task is up to date, broadcast it as done.
            self.broadcast_asset(asset);
            return;
        }

        {
            // need to store this request so we can broadcast when it is done
            let task_source = task.borrow().source.clone();
            let mut requests = self.shared.requests.borrow_mut();
            let known = requests
                .iter()
                .any(|(source, request)| source == &task_source && request == asset);
            if !known {
                requests.push((task_source, asset.to_string()));
            }
        }

        self.store.add_requested_task(&task.borrow());

        // notify the processing thread that there is something to build
        self.shared.event.set(true);
    }

    /// `purgeResource` (jit_compiler.cpp:288-296): drop the resource and
    /// its cached content hash so a modification is re-read.
    pub fn purge_resource(&self, resource_id: &str) {
        self.compiler.resource().purge(resource_id, false);
        self.compiler.purge_file_hash(resource_id);
    }

    /// `onResourceModified` (jit_compiler.cpp:386-476).
    pub fn on_resource_modified(&self, base_path: &str, resource_id: &str, action: ResourceAction) {
        if self.compiler.terminating() {
            return;
        }

        let full_path = format!("{base_path}{resource_id}");
        if path_is_relative(&full_path) {
            return;
        }

        // Purge the data census & cache.
        self.purge_resource(resource_id);
        self.purge_resource(&full_path);

        if self.compiler.resource().get_file_type(&full_path, None) == FileType::Directory {
            return;
        }

        let mut tasks: Vec<String> = Vec::new();

        // If the file was added, we may need to create a new root task.
        if action == ResourceAction::Added
            && let Some(root_task) = self.compiler.get_root_task(&full_path)
        {
            tasks.push(root_task.borrow().source.clone());
        }

        // Collect all the tasks that depend on this file. The directory
        // path carries its trailing slash (the reference's `getFilePath`
        // form; the port's `get_file_path` is a bare `dirname`).
        let path = format!("{}/", bwresource::get_file_path(&full_path));
        let filename = bwresource::get_filename(&full_path);
        let include_outputs = !self.compiler.check_file_hash_up_to_date(&full_path);
        self.hook
            .collect_reverse_dependencies_path(&full_path, include_outputs, &mut tasks);
        self.hook
            .collect_reverse_dependencies_directory(&path, filename, &mut tasks);

        for source in tasks {
            let task = self.compiler.get_cached_task(&source);
            if task.borrow().converter_id == UNKNOWN_ID {
                continue;
            }
            if task.borrow().status == ConversionStatus::Queued {
                continue;
            }
            debug_assert!(
                matches!(
                    task.borrow().status,
                    ConversionStatus::New | ConversionStatus::Done | ConversionStatus::Failed
                ),
                "a collected task is not NEW, DONE or FAILED"
            );

            let modified_is_source = task.borrow().source == full_path;
            let source_exists = self.compiler.resource().file_exists(&task.borrow().source);

            // Don't touch this task if its source has been deleted,
            // but not by this modification.
            if !modified_is_source && !source_exists {
                continue;
            }

            // Reset the task.
            task.borrow_mut().status = ConversionStatus::New;
            task.borrow_mut().sub_tasks.clear();
            self.store.reset_task(&task.borrow());

            // Don't re-queue the task if we deleted the source.
            if modified_is_source && !source_exists {
                continue;
            }

            // Queue the task.
            Compiler::queue_task(&self.compiler, task.clone());

            // notify the processing thread that there is something to build
            self.shared.event.set(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Compiler;
    use crate::conversion_rule::ConversionRule;
    use crate::converter::Converter;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The fixture converter's registry id (unique per converter type).
    const TEST_CONVERTER_ID: u64 = 0x6A17_0001;

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A fresh temp directory (the crate-wide test convention).
    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("bw_jit_{}_{}_{}", tag, std::process::id(), id));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    /// A `BwResource` rooted at a fresh temp directory (with the
    /// editor-enabled empty-base root so absolute paths resolve).
    fn make_resource(tag: &str) -> (String, Rc<BwResource>) {
        let root = temp_root(tag);
        let mut resource = BwResource::init_paths(&[root.as_str()]);
        resource.enable_editor_default_root();
        (root, Rc::new(resource))
    }

    /// The fixture workspace: a JIT compiler over a fresh root with the
    /// given files, the generic rule loaded (`initCompiler`, which also
    /// enters `EXECUTING`) and the fixture rule on top.
    fn make_jit(tag: &str, files: &[(&str, &str)]) -> (String, JitCompiler) {
        let (root, resource) = make_resource(tag);
        for (name, content) in files {
            let path = format!("{root}/{name}");
            std::fs::write(&path, content).unwrap();
        }
        let rule = TxtRule {
            resource: resource.clone(),
        };
        let mut jit = JitCompiler::new(resource);
        jit.compiler_mut().init_compiler();
        jit.compiler_mut().register_conversion_rule(Box::new(rule));
        jit.compiler_mut()
            .register_converter(crate::converter_info::ConverterInfo::new(
                "test",
                TEST_CONVERTER_ID,
                "1.0",
                crate::converter_info::ConverterFlags::DEFAULT_FLAGS,
                |_| Box::new(TestConverter),
            ));
        (root, jit)
    }

    /// Scan every resource path (the driver's initial pass) and run one
    /// managing step (drain the queue).
    fn scan_and_build(jit: &JitCompiler) {
        jit.scanning_main();
        jit.managing_step();
    }

    /// The fixture rule: `.txt` sources are roots built by the test
    /// converter; the produced `.out` maps back to its `.txt` source
    /// (resolved against the resource, like the concrete rule's
    /// `getSourceFile`).
    struct TxtRule {
        resource: Rc<BwResource>,
    }

    impl ConversionRule for TxtRule {
        fn create_root_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
            if source_file.ends_with(".txt") {
                task.converter_id = TEST_CONVERTER_ID;
                task.converter_version = "1.0".to_string();
                true
            } else {
                false
            }
        }

        fn get_source_file(&self, file: &str) -> Option<String> {
            let stem = file.strip_suffix(".out")?;
            let mut source = format!("{stem}.txt");
            // resolveSourcePath: a relative path stays relative, an
            // absolute one under a resource path is stripped, then
            // resolveFilename anchors it (the compiler's pair).
            if !path_is_relative(&source) {
                let _ = self.resource.resolve_to_relative_path(&mut source);
            }
            source = self.resource.resolve_filename(&source);
            Some(source)
        }
    }

    /// The fixture converter (the body of a converter plugin's `convert`):
    /// writes `<stem>.out` carrying the source's content; `b.txt` depends
    /// on `a.out` and copies it through; `d.txt` declares a directory
    /// dependency on `*.cfg` files.
    struct TestConverter;

    impl Converter for TestConverter {
        fn create_dependencies(
            &self,
            source_file: &str,
            _compiler: &dyn Compiler,
            dependencies: &mut crate::dependency_list::DependencyList<&dyn Compiler>,
        ) -> bool {
            let name = bwresource::get_filename(source_file);
            if name == "b.txt" {
                let dir = bwresource::get_file_path(source_file);
                dependencies.add_secondary_output_file_dependency(format!("{dir}/a.out"), true);
            }
            if name == "d.txt" {
                dependencies.add_secondary_directory_dependency("", "new.cfg", false, false, true);
            }
            true
        }

        fn convert(
            &self,
            source_file: &str,
            _compiler: &dyn Compiler,
            _intermediate_files: &mut Vec<String>,
            output_files: &mut Vec<String>,
        ) -> bool {
            let name = bwresource::get_filename(source_file);
            let stem = name.trim_end_matches(".txt").to_string();
            let dir = bwresource::get_file_path(source_file);
            let mut content = std::fs::read_to_string(source_file).unwrap_or_default();
            if name == "b.txt" {
                // Pass the upstream output through (the chain fixture).
                if let Ok(upstream) = std::fs::read_to_string(format!("{dir}/a.out")) {
                    content = upstream;
                }
            }
            let output = format!("{stem}.out");
            std::fs::write(format!("{dir}/{output}"), content).unwrap();
            output_files.push(output);
            true
        }
    }

    /// The fixture plugin's `PluginInit` (the `plugin_main.cpp` pattern):
    /// register the converter info and succeed.
    fn plugin_init(registrar: &mut dyn crate::plugin_loader::ConverterRegistrar) -> bool {
        registrar.register_converter(crate::converter_info::ConverterInfo::new(
            "test",
            TEST_CONVERTER_ID,
            "1.0",
            crate::converter_info::ConverterFlags::DEFAULT_FLAGS,
            |_| Box::new(TestConverter),
        ));
        true
    }

    /// The registered converter is queryable from the compiler (the
    /// registry lookup the rules and processor share).
    #[test]
    fn the_plugin_registers_a_queryable_converter() {
        let (root, resource) = make_resource("plugin");
        let mut compiler = AssetCompiler::new(resource);
        compiler.add_tools_resource_paths();

        assert!(compiler.converter_info(TEST_CONVERTER_ID).is_none());

        // initPlugins: the list file names the plugin; the loader runs its
        // init against the compiler (the reference's PluginLoader&-is-a-
        // compiler face).
        let mut loader = crate::plugin_loader::PluginLoader::new();
        loader.register_plugin("test_converter", plugin_init, None);
        let config = format!("{root}/jit_compiler_plugins.txt");
        std::fs::write(&config, "test_converter\n").unwrap();
        loader.init_plugins(&config, &mut compiler);

        let info = compiler
            .converter_info(TEST_CONVERTER_ID)
            .expect("the plugin's converter is registered");
        assert_eq!(info.name, "test");
        assert_eq!(info.type_id, TEST_CONVERTER_ID);
        assert_eq!(info.version, "1.0");
        assert!(info.is_thread_safe());
        assert!(info.cache_dependencies());
        assert!(loader.errors().is_empty());
    }

    /// The same registration through the JIT compiler (the reference's
    /// `JITCompiler : PluginLoader` inheritance, flattened to
    /// `compiler_mut()`).
    #[test]
    fn the_jit_compiler_hosts_the_plugin_registration() {
        let (root, resource) = make_resource("jit_plugin");
        let mut jit = JitCompiler::new(resource);
        let mut loader = crate::plugin_loader::PluginLoader::new();
        loader.register_plugin("test_converter", plugin_init, None);
        let config = format!("{root}/jit_compiler_plugins.txt");
        std::fs::write(&config, "test_converter\n").unwrap();
        loader.init_plugins(&config, jit.compiler_mut());

        let info = jit
            .compiler()
            .converter_info(TEST_CONVERTER_ID)
            .expect("registered through the JIT's compiler face");
        assert_eq!(info.version, "1.0");
    }

    /// A requested asset jumps the queue (the `onAssetRequested`
    /// pull-to-front block): `z.txt`'s task moves in front of `m.txt`'s.
    #[test]
    fn a_requested_asset_jumps_the_queue() {
        let (_root, jit) = make_jit("jump", &[("m.txt", "mmm"), ("z.txt", "zzz")]);
        jit.scanning_main();

        // The discovery walk queues both roots (in its own order).
        let mut queue = Vec::new();
        while let Some(task) = jit.compiler().get_next_task() {
            queue.push(task.borrow().source.clone());
        }
        assert_eq!(queue.len(), 2, "both roots queued");

        // Rebuild the queue in the discovered order, then request the
        // asset of whichever task is NOT at the front: its task jumps.
        let front_is_z = queue[0].ends_with("z.txt");
        let requested = if front_is_z { "m.out" } else { "z.out" };
        for source in &queue {
            let task = jit.compiler().get_cached_task(source);
            task.borrow_mut().status = ConversionStatus::New;
            Compiler::queue_task(jit.compiler(), task);
        }
        jit.on_asset_requested(requested);

        let first = jit
            .compiler()
            .get_next_task()
            .expect("the requested task is queued");
        let expected_suffix = requested.trim_end_matches(".out");
        assert!(
            first
                .borrow()
                .source
                .ends_with(&format!("{expected_suffix}.txt")),
            "{requested} jumped the queue, got {}",
            first.borrow().source
        );

        // The request is recorded (a completion broadcast is pending) and
        // the managing thread has been signalled.
        assert!(jit.event_pending());
    }

    /// The on-demand pipeline: a request drives the build and the asset is
    /// broadcast as done when the task completes.
    #[test]
    fn a_request_drives_the_build_and_broadcasts_on_completion() {
        let (root, jit) = make_jit("ondemand", &[("z.txt", "zzz")]);

        let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = seen.clone();
        jit.set_broadcast_callback(Some(Rc::new(move |asset: &str| {
            sink.borrow_mut().push(asset.to_string());
        })));

        jit.on_asset_requested("z.out");
        assert!(jit.event_pending(), "the request wakes the managing step");
        jit.managing_step();

        // The output was generated on demand and broadcast.
        let output = std::fs::read_to_string(format!("{root}/z.out")).unwrap();
        assert_eq!(output, "zzz");
        assert_eq!(jit.broadcasts(), ["z.out".to_string()]);
        assert_eq!(seen.borrow().len(), 1, "the callback saw the broadcast");
        assert!(!jit.event_pending(), "the wake flag drained");

        // The store followed the task: requested → completed.
        let task = jit.compiler().get_cached_task(&format!("{root}/z.txt"));
        let borrowed = task.borrow();
        assert_eq!(borrowed.status, ConversionStatus::Done);
        assert!(jit.store().is_completed(&borrowed));
        assert!(!jit.store().is_requested(&borrowed));
    }

    /// An up-to-date asset broadcasts without a rebuild: the second request
    /// on a `DONE` task never re-queues.
    #[test]
    fn an_up_to_date_asset_broadcasts_without_rebuilding() {
        let (_root, jit) = make_jit("uptodate", &[("z.txt", "zzz")]);
        jit.on_asset_requested("z.out");
        jit.managing_step();
        assert_eq!(jit.broadcasts().len(), 1);

        // Second request: the task is DONE with no pending modification.
        jit.on_asset_requested("z.out");
        assert_eq!(
            jit.broadcasts().len(),
            2,
            "broadcast straight from the DONE branch"
        );
        assert!(!jit.event_pending(), "no rebuild was signalled");
        assert!(jit.compiler().task_queue_empty(), "nothing re-queued");
    }

    /// An asset nobody can build broadcasts immediately (no rule maps it).
    #[test]
    fn an_unknown_asset_broadcasts_immediately() {
        let (_root, jit) = make_jit("unknown", &[]);
        jit.on_asset_requested("no-rule.xyz");
        assert_eq!(jit.broadcasts(), ["no-rule.xyz".to_string()]);
        assert!(!jit.event_pending());
        assert!(jit.compiler().task_queue_empty());
    }

    /// A modified source is re-queued and rebuilt by the next managing
    /// step (the modification-driven path).
    #[test]
    fn a_modified_source_requeues_and_rebuilds() {
        let (root, jit) = make_jit("modified", &[("c.txt", "v1")]);
        scan_and_build(&jit);
        assert_eq!(
            std::fs::read_to_string(format!("{root}/c.out")).unwrap(),
            "v1"
        );

        std::fs::write(format!("{root}/c.txt"), "v2").unwrap();
        jit.on_resource_modified(&format!("{root}/"), "c.txt", ResourceAction::Modified);
        assert!(
            jit.event_pending(),
            "the modification wakes the managing step"
        );
        jit.managing_step();

        let output = std::fs::read_to_string(format!("{root}/c.out")).unwrap();
        assert_eq!(output, "v2", "the rebuild saw the new content");
    }

    /// An upstream output change propagates down the chain: `b.txt` stays
    /// registered under `a.out` (its output dependency), so once `a` is
    /// rebuilt, the monitor's modification of `a.out` re-queues `b` and the
    /// new content flows through.
    #[test]
    fn an_output_change_propagates_down_the_chain() {
        let (root, jit) = make_jit("chain", &[("a.txt", "alpha"), ("b.txt", "beta")]);
        scan_and_build(&jit);
        assert_eq!(
            std::fs::read_to_string(format!("{root}/b.out")).unwrap(),
            "alpha",
            "b copied a's output through"
        );

        // Modify the upstream source and rebuild it.
        std::fs::write(format!("{root}/a.txt"), "ALPHA2").unwrap();
        jit.on_resource_modified(&format!("{root}/"), "a.txt", ResourceAction::Modified);
        jit.managing_step();

        // The regenerated output then arrives as its own modification.
        jit.on_resource_modified(&format!("{root}/"), "a.out", ResourceAction::Modified);
        jit.managing_step();

        let output = std::fs::read_to_string(format!("{root}/b.out")).unwrap();
        assert_eq!(output, "ALPHA2", "the change flowed through the chain");
    }

    /// A deleted source resets its task but does not re-queue it.
    #[test]
    fn a_deleted_source_resets_but_does_not_requeue() {
        let (root, jit) = make_jit("deleted", &[("c.txt", "v1")]);
        scan_and_build(&jit);

        std::fs::remove_file(format!("{root}/c.txt")).unwrap();
        jit.on_resource_modified(&format!("{root}/"), "c.txt", ResourceAction::Removed);

        let task = jit.compiler().get_cached_task(&format!("{root}/c.txt"));
        assert_eq!(task.borrow().status, ConversionStatus::New, "reset");
        assert!(
            jit.compiler().task_queue_empty(),
            "no re-queue for a deleted source"
        );
        assert!(jit.store().find_task_info(&task.borrow()).is_some());
    }

    /// A directory dependency re-queues its task when a matching file is
    /// added (the directory-entry branch of `collectReverseDependencies`).
    #[test]
    fn a_directory_dependency_requeues_on_an_added_file() {
        let (root, jit) = make_jit("dirdep", &[("d.txt", "delta")]);
        scan_and_build(&jit);

        // Add a file the directory dependency watches.
        std::fs::write(format!("{root}/new.cfg"), "cfg").unwrap();
        jit.on_resource_modified(&format!("{root}/"), "new.cfg", ResourceAction::Added);
        assert!(jit.event_pending(), "the dependent was re-queued");

        let task = jit.compiler().get_cached_task(&format!("{root}/d.txt"));
        assert_eq!(task.borrow().status, ConversionStatus::Queued);
        jit.managing_step();
        let borrowed = task.borrow();
        assert_eq!(borrowed.status, ConversionStatus::Done);
        assert!(jit.store().is_completed(&borrowed));
    }

    /// `stop` terminates the compiler and wakes the managing thread; `lock`
    /// pauses and `unlock` resumes (the `AssetServer` faces).
    #[test]
    fn stop_and_lock_drive_the_state_machine() {
        let (_root, jit) = make_jit("stop", &[]);

        assert!(jit.compiler().executing());
        assert!(!jit.event_pending());
        jit.stop();
        assert!(jit.compiler().terminating());
        assert!(jit.event_pending(), "the managing thread is woken");
        // A second stop is a no-op (the reference's early return).
        jit.stop();
        assert!(jit.compiler().terminating());

        // lock/unlock on an executing compiler (a fresh one: the first is
        // terminating and pause() refuses off TERMINATING).
        let (_root2, jit2) = make_jit("lock", &[]);
        assert!(jit2.lock(), "lock pauses the executing compiler");
        assert!(jit2.compiler().paused());
        assert!(jit2.unlock(), "unlock resumes");
        assert!(jit2.compiler().executing());
    }

    /// `JitCompilerOptions`: the balloon flag defaults on and the batch
    /// settings are inherited from the compiler.
    #[test]
    fn options_default_and_inherit() {
        let (_root, mut jit) = make_jit("options", &[]);
        jit.compiler_mut().set_force_rebuild(true);
        jit.compiler_mut().set_thread_count(4);

        assert!(jit.options().enable_balloon_notifications());

        let mut options = JitCompilerOptions::from_compiler(jit.compiler());
        assert!(options.enable_balloon_notifications());
        assert!(options.base.force_rebuild());
        assert_eq!(options.base.thread_count(), 4);

        options.set_enable_balloon_notifications(false);
        assert!(!options.enable_balloon_notifications());

        options
            .base
            .intermediate_path_set("/tmp/jit_inter", jit.compiler().resource());
        options.apply(jit.compiler_mut());
        // sanitise_path_param adds the trailing slash for a fresh directory.
        assert!(
            jit.compiler()
                .get_intermediate_path()
                .starts_with("/tmp/jit_inter")
        );
    }

    /// The forward/reverse map cleanup: a collected task is removed from
    /// every list it appears in (the reference's forward-map walk).
    #[test]
    fn collected_tasks_are_cleaned_from_every_reverse_list() {
        let (root, resource) = make_resource("maps");
        let store = Rc::new(TaskStore::new());
        let shared = Rc::new(JitShared::new());
        let hook = JitHook {
            shared: shared.clone(),
            store: store.clone(),
            resource,
        };

        // One source registered under two paths.
        let source = format!("{root}/x.txt");
        hook.add_reverse_dependency(&format!("{root}/x.out"), true, &source);
        hook.add_reverse_dependency(&source, false, &source);

        // Collecting by the source path releases it from both lists.
        let mut collected = Vec::new();
        hook.collect_reverse_dependencies_path(&source, false, &mut collected);
        assert_eq!(collected, vec![source.clone()]);
        collected.clear();

        // The lists are empty: a second collect finds nothing.
        hook.collect_reverse_dependencies_path(&format!("{root}/x.out"), true, &mut collected);
        assert!(
            collected.is_empty(),
            "the forward cleanup emptied the lists"
        );
        assert!(
            shared
                .forward_dependencies
                .borrow()
                .get(&source)
                .unwrap()
                .is_empty()
        );

        // Directory keys round-trip: register, match, erase once (a
        // non-regex pattern matches the literal filename).
        hook.add_reverse_dependency_directory(&root, "keep.cfg", false, false, &source);
        let mut tasks = Vec::new();
        hook.collect_reverse_dependencies_directory(&format!("{root}/"), "keep.cfg", &mut tasks);
        assert_eq!(tasks, vec![source.clone()]);
        assert!(
            shared.directory_dependencies.borrow().is_empty(),
            "the entry is erased after the match"
        );
    }
}
