//! `TaskProcessor` port (`conversion/task_processor.cpp`): drives one
//! conversion task through its dependency and conversion states.
//!
//! Reference quirks preserved:
//! - the state ladder (`PROCESSING` → `NEEDS_PRIMARY_DEPS` →
//!   `NEEDS_SECONDARY_DEPS` → `NEEDS_CONVERSION` → `DONE`/`FAILED`) is driven
//!   by [`TaskProcessor::process_task`], with the `.deps` dependency list
//!   loaded from `{source}.deps` in the intermediate path and saved back when
//!   the primary or secondary dependencies change;
//! - the primary dependencies are (source, converter, converter params),
//!   hashed by the compiler; a cached dependency list is only trusted when
//!   every primary hash still matches, otherwise the converter regenerates it;
//! - the secondary dependencies are resolved through
//!   `Compiler::ensure_up_to_date` (which may itself convert a sub-dependency);
//!   a *critical* failed dependency fails the task, a non-critical one is
//!   tolerated;
//! - conversion is skipped when the stored output hashes still match the
//!   files on disk (or the outputs are served from the content cache), and
//!   the outputs/intermediates are re-hashed and re-cached after a convert;
//! - the `.deps` and output caches are only written when the converter did
//!   not raise a warning, matching the reference's `hasWarning` gates.
//!
//! The single-threaded port processes sub-dependencies *inline* (the
//! reference's `recursive_` branch of `ensureCompiled`): it does not model
//! `s_currentTaskQueue`, `onTaskSuspended`'s queue reordering, thread ids,
//! file handles, semaphores or the `BgTaskManager`, so a task runs to
//! completion in one `process_task` call. The result is identical (every
//! sub-dependency is up to date before the parent converts); only the
//! interleaving differs. Cyclic dependencies are detected in
//! `AssetCompiler::ensure_compiled`.

use std::cell::RefCell;
use std::rc::Rc;

use resources::bwresource::BwResource;
use resources::datasection::DataSection;

use crate::compiler::Compiler;
use crate::conversion_task::{ConversionStatus, ConversionTask, SubTask, TaskHandle};
use crate::converter::Converter;
use crate::converter_info::ConverterInfo;
use crate::converter_map::ConverterMap;
use crate::dependency::Dependency;
use crate::dependency_list::DependencyList;

/// `TaskProcessor( Compiler &, ConverterMap &, bool )`: drives tasks to
/// completion. The compiler is passed per call (`&dyn Compiler`) so the
/// processor never owns it (the owning `AssetCompiler` re-enters through
/// `ensure_up_to_date`).
pub struct TaskProcessor {
    converter_map: Rc<RefCell<ConverterMap>>,
    force_rebuild: bool,
}

impl TaskProcessor {
    pub fn new(converter_map: Rc<RefCell<ConverterMap>>, force_rebuild: bool) -> Self {
        Self {
            converter_map,
            force_rebuild,
        }
    }

    /// `forceRebuild`.
    pub fn set_force_rebuild(&mut self, force: bool) {
        self.force_rebuild = force;
    }

    /// `processTasks`: run the single-threaded loop (the reference's
    /// `processTasksOnSingleThread`; the `BgTaskManager`-driven multi-threaded
    /// variant is a dropped Windows concern — see the module docs).
    pub fn process_tasks(&self, compiler: &dyn Compiler, resource: &BwResource) {
        while let Some(task) = compiler.get_next_task() {
            let status = task.borrow().status;
            if status == ConversionStatus::Processing {
                compiler.on_task_started(&task.borrow());
            } else {
                compiler.on_task_resumed(&task.borrow());
            }

            self.process_task(compiler, resource, &task);

            let status = task.borrow().status;
            if status >= ConversionStatus::Done {
                compiler.on_task_completed(&task.borrow());
            } else {
                compiler.on_task_suspended(&task.borrow());
            }
        }
    }

    /// `processTask`: walk the state ladder, short-circuiting at the first
    /// failed step (the reference's `MF_ASSERT(status >= DONE)` guard becomes
    /// an early return when a step leaves the task failed or suspended).
    ///
    /// This is also the entry point for a sub-dependency driven inline by
    /// `AssetCompiler::ensure_compiled` (the reference's `recursive_` branch),
    /// which wraps the call with `onTaskStarted`/`onTaskCompleted` itself.
    pub fn process_task(&self, compiler: &dyn Compiler, resource: &BwResource, task: &TaskHandle) {
        // TaskContext: the source resolved to a relative path.
        let relative_source = {
            let mut rel = task.borrow().source.clone();
            if !compiler.resolve_relative_path(&mut rel) {
                String::new()
            } else {
                rel
            }
        };

        if task.borrow().status == ConversionStatus::Processing
            && !self.process_new_task(compiler, task, &relative_source)
        {
            return;
        }

        // The converter map is indexed by id; an unknown id fails the task.
        // (The id is read first: a match scrutinee temporary would hold the
        // task borrow through the arms and poison the `None` arm's
        // `borrow_mut`.)
        let converter_id = task.borrow().converter_id;
        let converter_info = match self.converter_map.borrow().get(&converter_id).cloned() {
            Some(info) => info,
            None => {
                eprintln!(
                    "FAILED: No converter registered for id {} ({})",
                    converter_id,
                    task.borrow().source
                );
                task.borrow_mut().status = ConversionStatus::Failed;
                return;
            }
        };
        let converter_params = task.borrow().converter_params.clone();

        let mut dependency_context = DependencyContext::new(compiler, resource, &task.borrow());
        let mut conversion_context = ConversionContext::new(converter_info, converter_params);

        if task.borrow().status == ConversionStatus::NeedsPrimaryDeps
            && !self.process_primary_dependencies(
                compiler,
                task,
                &relative_source,
                &mut dependency_context,
                &mut conversion_context,
            )
        {
            return;
        }

        if task.borrow().status == ConversionStatus::NeedsSecondaryDeps
            && !self.process_secondary_dependencies(
                compiler,
                task,
                &mut dependency_context,
                &mut conversion_context,
            )
        {
            // Either failed (status FAILED) or blocked on a dependency
            // (status still NEEDS_CONVERSION → suspended). The loop
            // inspects the status to choose the callback.
            return;
        }

        if task.borrow().status == ConversionStatus::NeedsConversion
            && !self.process_conversion(
                compiler,
                task,
                &mut dependency_context,
                &mut conversion_context,
            )
        {}
    }

    /// `processNewTask`: sanity-check the resolved source path.
    fn process_new_task(
        &self,
        compiler: &dyn Compiler,
        task: &TaskHandle,
        relative_source: &str,
    ) -> bool {
        if relative_source.is_empty() {
            eprintln!(
                "FAILED: Could not resolve relative path for {}.",
                task.borrow().source
            );
            task.borrow_mut().status = ConversionStatus::Failed;
            return false;
        }

        let mut resolved = relative_source.to_string();
        if !compiler.resolve_source_path(&mut resolved) || task.borrow().source != resolved {
            eprintln!(
                "FAILED: Found {} while trying to compile {}. This asset will be ignored.",
                resolved,
                task.borrow().source
            );
            task.borrow_mut().status = ConversionStatus::Failed;
            return false;
        }

        task.borrow_mut().status = ConversionStatus::NeedsPrimaryDeps;
        true
    }

    /// `processPrimaryDependencies`.
    fn process_primary_dependencies(
        &self,
        compiler: &dyn Compiler,
        task: &TaskHandle,
        relative_source: &str,
        dependency_context: &mut DependencyContext,
        conversion_context: &mut ConversionContext,
    ) -> bool {
        let (converter_id, converter_version, converter_params, source) = {
            let t = task.borrow();
            (
                t.converter_id,
                t.converter_version.clone(),
                t.converter_params.clone(),
                t.source.clone(),
            )
        };

        // Check if the primary dependencies are up to date.
        let mut needs_deps = self.force_rebuild;
        if !needs_deps {
            needs_deps = !self.check_primary_dependencies_up_to_date(
                compiler,
                relative_source,
                converter_id,
                &converter_version,
                &converter_params,
                dependency_context,
            );
        }

        if needs_deps {
            // Setup the primary dependencies for this task (the same for all
            // assets, so the converter is not consulted).
            dependency_context.dep_list.initialise(
                relative_source,
                converter_id,
                &converter_version,
                &converter_params,
            );

            // Don't regenerate if we can retrieve them from the cache.
            needs_deps = !self.retrieve_primary_dependency_list_from_cache(
                compiler,
                dependency_context,
                conversion_context,
            );
        }

        if needs_deps {
            if !conversion_context.init_converter() {
                eprintln!("FAILED: Could not create converter for {source}");
                task.borrow_mut().status = ConversionStatus::Failed;
                return false;
            }

            compiler.on_pre_create_dependencies(&task.borrow());
            let res = conversion_context
                .converter
                .as_ref()
                .expect("converter initialised")
                .create_dependencies(&source, compiler, &mut dependency_context.dep_list);
            compiler.on_post_create_dependencies(&task.borrow());

            if !res || compiler.has_error() {
                eprintln!("FAILED: Errors creating dependencies");
                task.borrow_mut().status = ConversionStatus::Failed;
                return false;
            }

            // Save the generated dependency list if there were no warnings.
            if !compiler.has_warning() {
                dependency_context
                    .dep_list
                    .serialise_out(&mut dependency_context.dep_list_root)
                    .ok();
                dependency_context.resource.save_section(
                    &dependency_context.dep_list_file_name,
                    &mut dependency_context.dep_list_root,
                );
                if conversion_context.converter_info.cache_dependencies() {
                    compiler.cache_write(
                        &dependency_context.dep_list_file_name,
                        dependency_context.dep_list.get_input_hash(false),
                    );
                }
            }
        }

        task.borrow_mut().status = ConversionStatus::NeedsSecondaryDeps;
        true
    }

    /// `processSecondaryDependencies`: make every secondary dependency up to
    /// date before converting. Returns `false` when the task failed (critical
    /// dependency error) or when a dependency is still blocked (suspended).
    fn process_secondary_dependencies(
        &self,
        compiler: &dyn Compiler,
        task: &TaskHandle,
        dependency_context: &mut DependencyContext,
        _conversion_context: &mut ConversionContext,
    ) -> bool {
        let secondary = dependency_context.dep_list.secondary_inputs().to_vec();

        let mut blocked = false;
        let mut sub_task_error = false;
        let mut sub_tasks: Vec<SubTask> = Vec::new();

        for (dependency, _stored) in &secondary {
            let (mut up_to_date, sub_task) = compiler.ensure_up_to_date(dependency);
            let mut failed = !up_to_date;

            if let Some(sub) = &sub_task {
                let status = sub.borrow().status;
                if status == ConversionStatus::Failed {
                    // A failed sub-task cannot be helped; treat as up to date
                    // so the parent is not blocked forever (the reference
                    // also treats a task blocked by this thread as such).
                    up_to_date = true;
                } else {
                    // A sub-task still processing is not failed, just not
                    // up to date yet.
                    failed = false;
                }
                sub_tasks.push(SubTask {
                    handle: sub.clone(),
                    critical: dependency.is_critical(),
                });
            }

            if failed {
                if dependency.is_critical() {
                    if let Some(sub) = &sub_task {
                        eprintln!(
                            "Errors processing critical dependent task {}",
                            sub.borrow().source
                        );
                    }
                    sub_task_error = true;
                } else {
                    up_to_date = true;
                }
            }

            blocked |= !up_to_date;
        }

        if sub_task_error {
            eprintln!("FAILED: Errors processing dependencies");
            task.borrow_mut().status = ConversionStatus::Failed;
            return false;
        }

        task.borrow_mut().sub_tasks = sub_tasks;
        // If blocked the task stays NEEDS_CONVERSION and is suspended (the
        // loop re-queues it); inline processing never blocks, but the state
        // machine stays faithful.
        task.borrow_mut().status = ConversionStatus::NeedsConversion;
        !blocked
    }

    /// `processConversion`.
    fn process_conversion(
        &self,
        compiler: &dyn Compiler,
        task: &TaskHandle,
        dependency_context: &mut DependencyContext,
        conversion_context: &mut ConversionContext,
    ) -> bool {
        let source = task.borrow().source.clone();

        if has_sub_task_errors(task) {
            eprintln!("FAILED: Errors processing dependencies");
            task.borrow_mut().status = ConversionStatus::Failed;
            return false;
        }

        let mut needs_conversion = self.force_rebuild;
        if !needs_conversion {
            needs_conversion = !has_outputs(dependency_context);
        }
        if !needs_conversion {
            needs_conversion =
                !self.check_secondary_dependencies_up_to_date(compiler, dependency_context);
        }

        if needs_conversion {
            dependency_context.dep_list.update_secondary_input_hashes();
            needs_conversion = !self.retrieve_secondary_dependency_list_from_cache(
                compiler,
                dependency_context,
                conversion_context,
            );
        }

        if !needs_conversion {
            needs_conversion = !has_outputs(dependency_context);
        }
        if !needs_conversion {
            needs_conversion = !self.check_intermediate_outputs_up_to_date(
                compiler,
                dependency_context,
                conversion_context,
            );
        }
        if !needs_conversion {
            needs_conversion =
                !self.check_outputs_up_to_date(compiler, dependency_context, conversion_context);
        }

        if needs_conversion {
            let mut intermediate_files = Vec::new();
            let mut output_files = Vec::new();

            if !conversion_context.init_converter() {
                eprintln!("FAILED: Could not create converter for {source}");
                task.borrow_mut().status = ConversionStatus::Failed;
                return false;
            }

            compiler.on_pre_convert(&task.borrow());
            let res = conversion_context
                .converter
                .as_ref()
                .expect("converter initialised")
                .convert(
                    &source,
                    compiler,
                    &mut intermediate_files,
                    &mut output_files,
                );
            compiler.on_post_convert(&task.borrow());

            self.update_intermediate_outputs(
                compiler,
                dependency_context,
                conversion_context,
                &intermediate_files,
            );
            self.update_outputs(
                compiler,
                dependency_context,
                conversion_context,
                &output_files,
            );

            if !res || compiler.has_error() {
                eprintln!("FAILED: Errors converting task");
                task.borrow_mut().status = ConversionStatus::Failed;
                return false;
            }
            if intermediate_files.is_empty() && output_files.is_empty() {
                eprintln!("FAILED: No outputs were generated");
                task.borrow_mut().status = ConversionStatus::Failed;
                return false;
            }

            // Serialise out the updated dependency list if there were no
            // warnings.
            if !compiler.has_warning() {
                dependency_context
                    .dep_list
                    .serialise_out(&mut dependency_context.dep_list_root)
                    .ok();
                dependency_context.resource.save_section(
                    &dependency_context.dep_list_file_name,
                    &mut dependency_context.dep_list_root,
                );
                if conversion_context.converter_info.cache_conversion() {
                    compiler.cache_write(
                        &dependency_context.dep_list_file_name,
                        dependency_context.dep_list.get_input_hash(true),
                    );
                }
            }
        }

        task.borrow_mut().status = ConversionStatus::Done;
        true
    }

    /// `checkPrimaryDependenciesUpToDate`.
    fn check_primary_dependencies_up_to_date(
        &self,
        compiler: &dyn Compiler,
        relative_source: &str,
        converter_id: u64,
        converter_version: &str,
        converter_params: &str,
        dependency_context: &DependencyContext,
    ) -> bool {
        let primary = dependency_context.dep_list.primary_inputs().to_vec();
        if primary.len() < 3 {
            // Our dep list should always have at least 3 primary deps.
            return false;
        }

        for (i, (dependency, stored)) in primary.iter().enumerate() {
            if *stored != compiler.get_hash(dependency) {
                return false;
            }

            match i {
                0 => {
                    // The first should be a dependency on the source file.
                    if let Dependency::SourceFile(d) = dependency {
                        if d.file_name() != relative_source {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                1 => {
                    // The second should be a converter dependency.
                    if let Dependency::Converter(d) = dependency {
                        if d.converter_id() != converter_id
                            || d.converter_version() != converter_version
                        {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                2 => {
                    // The last should be a converter params dependency.
                    if let Dependency::ConverterParams(d) = dependency {
                        if d.converter_params() != converter_params {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                _ => {}
            }
        }

        true
    }

    /// `retrievePrimaryDependencyListFromCache`.
    fn retrieve_primary_dependency_list_from_cache(
        &self,
        compiler: &dyn Compiler,
        dependency_context: &mut DependencyContext,
        conversion_context: &ConversionContext,
    ) -> bool {
        !self.force_rebuild
            && conversion_context.converter_info.cache_dependencies()
            && dependency_context.load_from_cache(false, compiler)
    }

    /// `retrieveSecondaryDependencyListFromCache`.
    fn retrieve_secondary_dependency_list_from_cache(
        &self,
        compiler: &dyn Compiler,
        dependency_context: &mut DependencyContext,
        conversion_context: &ConversionContext,
    ) -> bool {
        !self.force_rebuild
            && conversion_context.converter_info.cache_conversion()
            && dependency_context.load_from_cache(true, compiler)
    }

    /// `checkSecondaryDependenciesUpToDate`.
    fn check_secondary_dependencies_up_to_date(
        &self,
        compiler: &dyn Compiler,
        dependency_context: &DependencyContext,
    ) -> bool {
        for (dependency, stored) in dependency_context.dep_list.secondary_inputs() {
            if *stored != compiler.get_hash(dependency) {
                return false;
            }
        }
        true
    }

    /// `checkIntermediateOutputsUpToDate`.
    fn check_intermediate_outputs_up_to_date(
        &self,
        compiler: &dyn Compiler,
        dependency_context: &DependencyContext,
        conversion_context: &ConversionContext,
    ) -> bool {
        let outputs = dependency_context.dep_list.intermediate_outputs().to_vec();
        for (file, stored) in &outputs {
            let mut output = file.clone();
            compiler.resolve_intermediate_path(&mut output);
            let mut hash = compiler.get_file_hash(&output, false);
            if *stored != hash {
                if conversion_context.converter_info.cache_conversion()
                    && compiler.cache_read(&output, *stored)
                {
                    hash = compiler.get_file_hash(&output, true);
                    if *stored != hash {
                        eprintln!("Corrupted intermediate output retrieved from the cache");
                        return false;
                    }
                    continue;
                }
                return false;
            }
        }
        true
    }

    /// `checkOutputsUpToDate`.
    fn check_outputs_up_to_date(
        &self,
        compiler: &dyn Compiler,
        dependency_context: &DependencyContext,
        conversion_context: &ConversionContext,
    ) -> bool {
        let outputs = dependency_context.dep_list.outputs().to_vec();
        for (file, stored) in &outputs {
            let mut output = file.clone();
            compiler.resolve_output_path(&mut output);
            let mut hash = compiler.get_file_hash(&output, false);
            if *stored != hash {
                if conversion_context.converter_info.cache_conversion()
                    && compiler.cache_read(&output, *stored)
                {
                    hash = compiler.get_file_hash(&output, true);
                    if *stored != hash {
                        eprintln!("Corrupted output retrieved from the cache");
                        return false;
                    }
                    continue;
                }
                return false;
            }
        }
        true
    }

    /// `updateIntermediateOutputs`: re-hash and re-cache the intermediates.
    fn update_intermediate_outputs(
        &self,
        compiler: &dyn Compiler,
        dependency_context: &mut DependencyContext,
        conversion_context: &ConversionContext,
        intermediate_files: &[String],
    ) {
        let mut outputs = Vec::new();
        for file in intermediate_files {
            let mut output = file.clone();
            let mut relative_output = file.clone();
            compiler.resolve_intermediate_path(&mut output);
            compiler.resolve_relative_path(&mut relative_output);

            let hash = compiler.get_file_hash(&output, true);
            outputs.push((relative_output, hash));

            compiler.on_output_generated(&output);
            if conversion_context.converter_info.cache_conversion() {
                compiler.cache_write(&output, hash);
            }
        }
        dependency_context
            .dep_list
            .set_intermediate_outputs(outputs);
    }

    /// `updateOutputs`: re-hash and re-cache the final outputs.
    fn update_outputs(
        &self,
        compiler: &dyn Compiler,
        dependency_context: &mut DependencyContext,
        conversion_context: &ConversionContext,
        output_files: &[String],
    ) {
        let mut outputs = Vec::new();
        for file in output_files {
            let mut output = file.clone();
            let mut relative_output = file.clone();
            compiler.resolve_output_path(&mut output);
            compiler.resolve_relative_path(&mut relative_output);

            let hash = compiler.get_file_hash(&output, true);
            outputs.push((relative_output, hash));

            compiler.on_output_generated(&output);
            if conversion_context.converter_info.cache_conversion() {
                compiler.cache_write(&output, hash);
            }
        }
        dependency_context.dep_list.set_outputs(outputs);
    }
}

/// `hasSubTaskErrors`: a sub-task not completed, or a critical one failed.
fn has_sub_task_errors(task: &TaskHandle) -> bool {
    let mut error = false;
    for sub in &task.borrow().sub_tasks {
        let status = sub.handle.borrow().status;
        if status < ConversionStatus::Done {
            // All sub tasks should have been completed by this point.
            error = true;
        }
        if sub.critical && status == ConversionStatus::Failed {
            eprintln!(
                "Errors processing critical dependent task {}",
                sub.handle.borrow().source
            );
            error = true;
        }
    }
    error
}

/// `hasOutputs`.
fn has_outputs(dependency_context: &DependencyContext) -> bool {
    !dependency_context
        .dep_list
        .intermediate_outputs()
        .is_empty()
        || !dependency_context.dep_list.outputs().is_empty()
}

/// `DependencyContext`: the `.deps` file and its parsed dependency list.
struct DependencyContext<'a> {
    dep_list: DependencyList<&'a dyn Compiler>,
    dep_list_file_name: String,
    dep_list_root: DataSection,
    resource: &'a BwResource,
}

impl<'a> DependencyContext<'a> {
    /// `DependencyContext( taskProcessor, conversionTask )`: load the
    /// `{source}.deps` list (creating an empty one if the file is absent).
    fn new(compiler: &'a dyn Compiler, resource: &'a BwResource, task: &ConversionTask) -> Self {
        let mut file_name = task.source.clone();
        file_name.push_str(".deps");
        compiler.resolve_intermediate_path(&mut file_name);
        resource.ensure_absolute_path_exists(&file_name);

        let root = load_dep_list(resource, &file_name);
        let mut dep_list = DependencyList::new(compiler);
        // A malformed or empty list is tolerated (regenerated later by the
        // converter), matching the reference's lenient `serialiseIn`.
        let _ = dep_list.serialise_in(&root);

        Self {
            dep_list,
            dep_list_file_name: file_name,
            dep_list_root: root,
            resource,
        }
    }

    /// `loadFromCache`: restore the dependency list from the content cache
    /// and validate every hash. `true` when the list was served from the
    /// cache; on a corrupt cache entry the on-disk list is restored.
    fn load_from_cache(&mut self, secondary: bool, compiler: &dyn Compiler) -> bool {
        let backup = self.dep_list.clone();
        let file = resources::bwresource::get_filename(&self.dep_list_file_name).to_string();
        let hash = self.dep_list.get_input_hash(secondary);

        if compiler.cache_read(&self.dep_list_file_name, hash) {
            self.resource.purge(&self.dep_list_file_name, false);
            self.dep_list_root = load_dep_list(self.resource, &self.dep_list_file_name);

            let mut valid = self.dep_list.serialise_in(&self.dep_list_root).is_ok();
            if valid {
                for (dependency, stored) in self.dep_list.primary_inputs().to_vec() {
                    if stored != compiler.get_hash(&dependency) {
                        valid = false;
                    }
                }
                for (dependency, stored) in self.dep_list.secondary_inputs().to_vec() {
                    let expected = if secondary {
                        compiler.get_hash(&dependency)
                    } else {
                        0
                    };
                    if stored != expected {
                        valid = false;
                    }
                }
            }

            if !valid {
                eprintln!("Corrupted dependency list retrieved from the cache: {file}.{hash:X}");
                // Restore the on-disk list (the reference's backup copy).
                self.dep_list = backup;
                let mut root = DataSection::new("root");
                self.dep_list.serialise_out(&mut root).ok();
                self.resource
                    .save_section(&self.dep_list_file_name, &mut root);
                self.dep_list_root = root;
                let _ = self.dep_list.serialise_in(&self.dep_list_root);
                return false;
            }
            return true;
        }

        false
    }
}

/// `ConversionContext`: the converter instance and its registry info.
struct ConversionContext {
    converter_info: ConverterInfo,
    converter_params: String,
    converter: Option<Box<dyn Converter>>,
}

impl ConversionContext {
    fn new(converter_info: ConverterInfo, converter_params: String) -> Self {
        Self {
            converter_info,
            converter_params,
            converter: None,
        }
    }

    /// `initConverter`: create the converter from the registry's creator.
    fn init_converter(&mut self) -> bool {
        if self.converter.is_none() {
            let creator = self.converter_info.creator;
            self.converter = Some(creator(&self.converter_params));
        }
        self.converter.is_some()
    }
}

/// `{source}.deps` root section (created empty when the file does not exist).
fn load_dep_list(resource: &BwResource, path: &str) -> DataSection {
    match resource.open_section(path, true) {
        Some(section) => (*section).clone(),
        None => DataSection::new("deps"),
    }
}
