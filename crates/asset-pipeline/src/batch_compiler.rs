//! `BatchCompiler` port (`tools/batch_compiler/batch_compiler.cpp`): the
//! command-line batch driver over [`AssetCompiler`], plus its options.
//!
//! Reference quirks preserved:
//! - `BatchCompiler` composes an [`AssetCompiler`] and installs itself (via a
//!   [`CompilerHook`]) as the callback sink, standing in for the reference's
//!   `BatchCompiler : AssetCompiler` subclassing — the hook methods run *in
//!   addition to* the base `AssetCompiler` behaviour, exactly like the
//!   reference's derived overrides calling the base;
//! - task records are keyed by the task's address (the reference's
//!   `const ConversionTask*`), so a task resumed after being suspended reuses
//!   its record;
//! - a task is *up to date* when its dependencies are generated but the
//!   conversion is skipped, and *skipped* when both dependencies and
//!   conversion are skipped (the `onPreCreateDependencies`/`onPreConvert`
//!   hooks clear the respective flag);
//! - `build` asserts the queue is empty, queues a single file's task (an
//!   unknown converter id is an error, not a queue entry) or discovers a
//!   directory's root tasks, then drains the queue and records the elapsed
//!   time;
//! - the driver order is `parseCommandLine` → `initCompiler` → `build` →
//!   `outputReport` → `finiCompiler`, and the process exit code is
//!   `getFailedCount()`.

use std::cell::{Cell, RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use resources::bwresource::BwResource;
use resources::zip::FileType;

use crate::asset_compiler::AssetCompiler;
use crate::asset_compiler_options::AssetCompilerOptions;
use crate::compiler::{Compiler, CompilerHook};
use crate::conversion_task::{ConversionStatus, ConversionTask, UNKNOWN_ID};

/// `BatchCompiler::TaskRecord`: the per-task statistics the report reads.
#[derive(Clone)]
pub struct TaskRecord {
    /// `source_`: the task's source file (the report links to it).
    pub source: String,
    /// `id_`: the unique task id (1-based, incremented per started task).
    pub id: u64,
    /// `upToDate_`: dependencies generated but the conversion was skipped.
    pub up_to_date: bool,
    /// `skipped_`: both dependencies and conversion were skipped.
    pub skipped: bool,
    /// `hasError_`: an error was reported during create/convert.
    pub has_error: bool,
    /// `hasWarning_`: a warning was reported during create/convert.
    pub has_warning: bool,
    /// `duration_`: the wall-clock seconds this task ran.
    pub duration: f64,
    /// `outputs_`: the files `onOutputGenerated` reported.
    pub outputs: Vec<String>,
}

impl Default for TaskRecord {
    fn default() -> Self {
        Self {
            source: String::new(),
            id: 0,
            up_to_date: true,
            skipped: true,
            has_error: false,
            has_warning: false,
            duration: 0.0,
            outputs: Vec::new(),
        }
    }
}

/// `BatchStats`: the shared counters and task records the [`BatchHook`]
/// mutates and the [`BatchCompiler`] reads for the report.
struct BatchStats {
    /// `taskRecords_`: per-task records keyed by the task's address.
    task_records: RefCell<HashMap<usize, TaskRecord>>,
    /// `totalDuration_`: the whole `build` wall-clock seconds.
    total_duration: Cell<f64>,
    /// `cacheReadCount_` & co.
    cache_read_count: Cell<u64>,
    cache_read_miss_count: Cell<u64>,
    cache_write_count: Cell<u64>,
    cache_write_miss_count: Cell<u64>,
    /// Discovery stats.
    files_iterated: Cell<u64>,
    directories_iterated: Cell<u64>,
    /// Conversion stats.
    task_count: Cell<u64>,
    task_failed_count: Cell<u64>,
    task_up_to_date_count: Cell<u64>,
    task_skipped_count: Cell<u64>,
    /// `s_currentTaskRecord`: the record key of the task being processed.
    current_task_record: RefCell<Option<usize>>,
    /// `s_currentTaskStartTime`: when the current task (re)started.
    current_task_start: RefCell<Option<Instant>>,
}

impl Default for BatchStats {
    fn default() -> Self {
        Self {
            task_records: RefCell::new(HashMap::new()),
            total_duration: Cell::new(0.0),
            cache_read_count: Cell::new(0),
            cache_read_miss_count: Cell::new(0),
            cache_write_count: Cell::new(0),
            cache_write_miss_count: Cell::new(0),
            files_iterated: Cell::new(0),
            directories_iterated: Cell::new(0),
            task_count: Cell::new(0),
            task_failed_count: Cell::new(0),
            task_up_to_date_count: Cell::new(0),
            task_skipped_count: Cell::new(0),
            current_task_record: RefCell::new(None),
            current_task_start: RefCell::new(None),
        }
    }
}

/// `BatchHook`: the [`CompilerHook`] the batch compiler installs. It only
/// mutates shared [`BatchStats`], so no reference cycle is formed.
struct BatchHook {
    stats: Rc<BatchStats>,
}

impl BatchHook {
    /// The current task's record (created on first touch).
    fn current_record(&self) -> Option<RefMut<'_, TaskRecord>> {
        let key = self.stats.current_task_record.borrow().as_ref().copied()?;
        Some(RefMut::map(
            self.stats.task_records.borrow_mut(),
            |records| records.entry(key).or_default(),
        ))
    }
}

impl CompilerHook for BatchHook {
    fn should_iterate_file(&self, _file: &str) -> bool {
        self.stats
            .files_iterated
            .set(self.stats.files_iterated.get() + 1);
        true
    }

    fn should_iterate_directory(&self, _directory: &str) -> bool {
        self.stats
            .directories_iterated
            .set(self.stats.directories_iterated.get() + 1);
        true
    }

    fn on_task_started(&self, task: &ConversionTask) {
        let key = task as *const ConversionTask as usize;
        self.stats.current_task_record.borrow_mut().replace(key);
        self.stats
            .current_task_start
            .borrow_mut()
            .replace(Instant::now());
        let id = self.stats.task_count.get() + 1;
        self.stats.task_count.set(id);
        let mut record = RefMut::map(self.stats.task_records.borrow_mut(), |records| {
            records.entry(key).or_default()
        });
        record.id = id;
        record.source = task.source.clone();
        eprintln!("------ Task started: {} ------", task.source);
    }

    fn on_task_resumed(&self, task: &ConversionTask) {
        let key = task as *const ConversionTask as usize;
        self.stats.current_task_record.borrow_mut().replace(key);
        self.stats
            .current_task_start
            .borrow_mut()
            .replace(Instant::now());
    }

    fn on_task_suspended(&self, task: &ConversionTask) {
        let key = task as *const ConversionTask as usize;
        if self.stats.current_task_record.borrow().as_ref() == Some(&key) {
            let duration = self
                .stats
                .current_task_start
                .borrow()
                .map(|start| start.elapsed().as_secs_f64())
                .unwrap_or(0.0);
            if let Some(record) = self.stats.task_records.borrow_mut().get_mut(&key) {
                record.duration += duration;
            }
        }
        self.stats.current_task_record.borrow_mut().take();
        self.stats.current_task_start.borrow_mut().take();
    }

    fn on_task_completed(&self, task: &ConversionTask) {
        let key = task as *const ConversionTask as usize;
        let duration = self
            .stats
            .current_task_start
            .borrow()
            .map(|start| start.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let status = task.status;

        let mut record = RefMut::map(self.stats.task_records.borrow_mut(), |records| {
            records.entry(key).or_default()
        });
        record.duration += duration;
        if status == ConversionStatus::Failed {
            self.stats
                .task_failed_count
                .set(self.stats.task_failed_count.get() + 1);
        } else if record.skipped {
            self.stats
                .task_skipped_count
                .set(self.stats.task_skipped_count.get() + 1);
        } else if record.up_to_date {
            self.stats
                .task_up_to_date_count
                .set(self.stats.task_up_to_date_count.get() + 1);
        }
        drop(record);

        self.stats.current_task_record.borrow_mut().take();
        self.stats.current_task_start.borrow_mut().take();
    }

    fn on_pre_create_dependencies(&self, _task: &ConversionTask) {
        if let Some(mut record) = self.current_record() {
            record.skipped = false;
        }
    }

    fn on_pre_convert(&self, _task: &ConversionTask) {
        if let Some(mut record) = self.current_record() {
            record.up_to_date = false;
            record.skipped = false;
        }
    }

    fn on_output_generated(&self, filename: &str) {
        if let Some(mut record) = self.current_record() {
            record.outputs.push(filename.to_string());
        }
    }

    fn on_cache_read(&self, _filename: &str) {
        self.stats
            .cache_read_count
            .set(self.stats.cache_read_count.get() + 1);
    }
    fn on_cache_read_miss(&self, _filename: &str) {
        self.stats
            .cache_read_miss_count
            .set(self.stats.cache_read_miss_count.get() + 1);
    }
    fn on_cache_write(&self, _filename: &str) {
        self.stats
            .cache_write_count
            .set(self.stats.cache_write_count.get() + 1);
    }
    fn on_cache_write_miss(&self, _filename: &str) {
        self.stats
            .cache_write_miss_count
            .set(self.stats.cache_write_miss_count.get() + 1);
    }
}

/// `BatchCompiler`: the batch driver (see the module docs).
pub struct BatchCompiler {
    /// The wrapped `AssetCompiler` (the reference's base sub-object).
    compiler: AssetCompiler,
    /// The shared counters (read by `get_failed_count`/`output_report`).
    stats: Rc<BatchStats>,
    /// Keeps the hook alive (the compiler holds a clone).
    _hook: Rc<BatchHook>,
}

impl BatchCompiler {
    /// `BatchCompiler()`: compose an [`AssetCompiler`] over `resource`,
    /// install the counting hook, and capture the tools' resource paths.
    pub fn new(resource: Rc<BwResource>) -> Self {
        let stats = Rc::new(BatchStats::default());
        let hook = Rc::new(BatchHook {
            stats: stats.clone(),
        });
        let mut compiler = AssetCompiler::new(resource);
        compiler.set_hook(hook.clone());
        compiler.add_tools_resource_paths();
        Self {
            compiler,
            stats,
            _hook: hook,
        }
    }

    /// `build( paths )`: queue a file's task or a directory's root tasks, then
    /// drain the queue, recording the elapsed time (see the module docs).
    pub fn build(&self, paths: &[String]) {
        assert!(self.compiler.task_queue_empty(), "task queue must be empty");

        let start = Instant::now();
        for path in paths {
            let ft = self.compiler.file_type(path);
            if ft == FileType::File {
                eprintln!("========== Processing File: {} ==========", path);
                let task = self.compiler.get_task(path);
                if task.borrow().converter_id == UNKNOWN_ID {
                    eprintln!("Could not find task for file {}", path);
                } else {
                    Compiler::queue_task(&self.compiler, task);
                }
            } else if ft == FileType::Directory {
                eprintln!("========== Processing Directory: {} ==========", path);
                self.compiler.find_tasks(path);
            }
        }

        eprintln!(
            "========== Found: {} tasks, Searched: {} files, {} directories ==========",
            self.compiler.task_queue_size(),
            self.stats.files_iterated.get(),
            self.stats.directories_iterated.get(),
        );

        self.compiler.process_tasks();
        eprintln!(
            "========== Conversion: {} succeeded, {} failed, {} converted, {} up-to-date, {} skipped ==========",
            self.stats.task_count.get() - self.stats.task_failed_count.get(),
            self.stats.task_failed_count.get(),
            self.stats.task_count.get()
                - (self.stats.task_failed_count.get()
                    + self.stats.task_up_to_date_count.get()
                    + self.stats.task_skipped_count.get()),
            self.stats.task_up_to_date_count.get(),
            self.stats.task_skipped_count.get(),
        );

        self.stats.total_duration.set(start.elapsed().as_secs_f64());
    }

    /// `getFailedCount()`.
    pub fn get_failed_count(&self) -> u64 {
        self.stats.task_failed_count.get()
    }

    /// `outputReport( filename )`: write a readable HTML report.
    pub fn output_report(&self, filename: &str) {
        let records: Vec<TaskRecord> = self.stats.task_records.borrow().values().cloned().collect();
        let mut html = String::from(
            "<!DOCTYPE html><html><head><title>Batch Compiler Report</title></head><body>\n",
        );
        html.push_str(&format!(
            "<h1>Batch Compiler</h1><p>Duration: {:.6} seconds</p>\n",
            self.stats.total_duration.get()
        ));
        html.push_str(&format!(
            "<p>Tasks: {} total, {} failed, {} up-to-date, {} skipped</p>\n",
            self.stats.task_count.get(),
            self.stats.task_failed_count.get(),
            self.stats.task_up_to_date_count.get(),
            self.stats.task_skipped_count.get(),
        ));
        for record in &records {
            html.push_str(&format!(
                "<h2>Task {}: {} ({})</h2>\n",
                record.id,
                record_status(record),
                record.source,
            ));
            if !record.outputs.is_empty() {
                html.push_str("<ul>\n");
                for output in &record.outputs {
                    html.push_str(&format!("<li>{}</li>\n", output));
                }
                html.push_str("</ul>\n");
            }
        }
        html.push_str("</body></html>\n");
        let _ = std::fs::write(filename, html);
    }

    /// `initCompiler`.
    pub fn init_compiler(&mut self) {
        self.compiler.init_compiler();
    }

    /// `finiCompiler`.
    pub fn fini_compiler(&self) {
        self.compiler.fini_compiler();
    }

    /// The wrapped compiler (the driver's `apply` targets it).
    pub fn compiler(&mut self) -> &mut AssetCompiler {
        &mut self.compiler
    }
}

fn record_status(record: &TaskRecord) -> &'static str {
    if record.has_error {
        "FAILED"
    } else if record.has_warning {
        "SUCCEEDED - WARNINGS"
    } else if record.skipped {
        "SUCCEEDED - SKIPPED"
    } else if record.up_to_date {
        "SUCCEEDED - UP TO DATE"
    } else {
        "SUCCEEDED"
    }
}

/// `BatchCompilerOptions`: the batch driver's command line, extending
/// [`AssetCompilerOptions`] with the input paths and the report path.
pub struct BatchCompilerOptions {
    base: AssetCompilerOptions,
    /// `parsedPaths_`: the validated inputs (files and directories).
    input_paths: Vec<String>,
    /// `reportPath_`: the report to write (empty = no report).
    report_path: String,
}

impl BatchCompilerOptions {
    /// `BatchCompilerOptions()`.
    pub fn new() -> Self {
        Self {
            base: AssetCompilerOptions::new(),
            input_paths: Vec::new(),
            report_path: String::new(),
        }
    }

    /// `parseCommandLine( commandLine )`: parse the positional input paths and
    /// `-report`, then the base options. Returns `false` when no valid input
    /// was found.
    pub fn parse_command_line(&mut self, args: &[String], resource: &BwResource) -> bool {
        if !self.parse_input_paths(args, resource) {
            return false;
        }

        // `-report <path>`.
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            if (arg == "-report" || arg == "/report")
                && let Some(value) = args.get(index + 1)
            {
                let dir = resources::bwresource::get_file_path(value);
                let name = resources::bwresource::get_filename(value);
                let clean = AssetCompiler::sanitise_path_param(&dir);
                self.report_path = format!("{}{}", clean.trim_end_matches('/'), name);
                resource.ensure_absolute_path_exists(&self.report_path);
                index += 1;
            }
            index += 1;
        }

        self.base.parse_command_line(args, resource);
        true
    }

    /// `parseInputPaths`: the tokens that are not switches are input
    /// candidates; each is resolved to an absolute sanitised path and kept
    /// only when it is a file or a directory.
    fn parse_input_paths(&mut self, args: &[String], resource: &BwResource) -> bool {
        let mut parsed: Vec<String> = Vec::new();
        let mut index = 0;
        while index < args.len() {
            let input = &args[index];
            if input.is_empty() {
                index += 1;
                continue;
            }
            if Self::has_switch(input) {
                // Skip the switch and its value.
                index += 2;
                continue;
            }

            let mut input_path = input.clone();
            resource.resolve_to_absolute_path(&mut input_path);
            let input_path = AssetCompiler::sanitise_path_param(&input_path);
            let ft = resource.get_file_type(&input_path, None);
            if ft != FileType::File && ft != FileType::Directory {
                eprintln!("Invalid input: {}", input);
                index += 1;
                continue;
            }
            parsed.push(input_path);
            index += 1;
        }

        let has_input = !parsed.is_empty();
        self.input_paths = parsed;
        has_input
    }

    /// `hasSwitch( arg )`: the reference's `CommandLine::hasSwitch` (a switch
    /// starts with `-` or `/`).
    fn has_switch(arg: &str) -> bool {
        arg.starts_with('-') || arg.starts_with('/')
    }

    /// `resolveInputPaths`: when no input was given, fall back to the
    /// resource search paths, then prune sub-paths of the set.
    pub fn resolve_input_paths(&mut self, resource: &BwResource) {
        if self.input_paths.is_empty() {
            let mut parsed = Vec::new();
            for i in 0..resource.get_path_num() {
                let path = AssetCompiler::sanitise_path_param(resource.get_path(i));
                parsed.push(path);
            }
            self.input_paths = parsed;
        }

        // Prune sub-paths (a directory contained in another input is dropped).
        let mut pruned: Vec<String> = Vec::new();
        for input_path in &self.input_paths {
            let is_file = resource.get_file_type(input_path, None) == FileType::File;
            if is_file {
                pruned.push(input_path.clone());
                continue;
            }
            let is_sub_path = self
                .input_paths
                .iter()
                .any(|path| path.starts_with(input_path) && path.len() != input_path.len());
            if !is_sub_path {
                pruned.push(input_path.clone());
            }
        }
        self.input_paths = pruned;
    }

    /// `apply( compiler )`: push the base options onto the batch compiler.
    pub fn apply(&mut self, compiler: &mut AssetCompiler) {
        self.base.apply(compiler);
    }

    /// `getInputPaths`.
    pub fn input_paths(&self) -> &[String] {
        &self.input_paths
    }

    /// `getReportPath`.
    pub fn report_path(&self) -> &str {
        &self.report_path
    }

    /// `generateReport`.
    pub fn generate_report(&self) -> bool {
        !self.report_path.is_empty()
    }
}

impl Default for BatchCompilerOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion_rule::ConversionRule;
    use crate::converter::Converter;
    use crate::converter_info::{ConverterCreator, ConverterFlags, ConverterInfo};
    use crate::dependency_list::DependencyList;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "bw_batch_compiler_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    /// A rule that claims `.txt` source files as root tasks.
    #[derive(Clone, Copy)]
    struct TextRule;

    impl ConversionRule for TextRule {
        fn create_root_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
            if source_file.ends_with(".txt") {
                task.converter_id = 7;
                task.converter_version = "1.0".to_string();
                true
            } else {
                false
            }
        }

        fn create_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
            self.create_root_task(source_file, task)
        }
    }

    /// A converter that writes a copy of the source to the output path.
    struct TextConverter;

    impl Converter for TextConverter {
        fn create_dependencies(
            &self,
            _source_file: &str,
            _compiler: &dyn Compiler,
            _dependencies: &mut DependencyList<&dyn Compiler>,
        ) -> bool {
            true
        }

        fn convert(
            &self,
            source_file: &str,
            compiler: &dyn Compiler,
            _intermediate_files: &mut Vec<String>,
            output_files: &mut Vec<String>,
        ) -> bool {
            let source = source_file.trim_end_matches('/');
            let mut out = source.to_string();
            if !compiler.resolve_output_path(&mut out) {
                return false;
            }
            let content = std::fs::read_to_string(source).unwrap_or_default();
            std::fs::write(&out, &content).unwrap();
            output_files.push(out);
            true
        }
    }

    fn text_converter(_params: &str) -> Box<dyn Converter> {
        Box::new(TextConverter)
    }

    #[test]
    fn build_converts_a_single_file_to_output() {
        let root = temp_root("e2e");
        let input = format!("{root}/in");
        let output = format!("{root}/out/");
        let intermediate = format!("{root}/intermediate/");
        std::fs::create_dir_all(&input).unwrap();
        std::fs::create_dir_all(&output).unwrap();
        std::fs::create_dir_all(&intermediate).unwrap();
        std::fs::write(format!("{input}/a.txt"), "hello").unwrap();

        let mut resource = BwResource::init_paths(&[input.as_str()]);
        resource.enable_editor_default_root();
        let resource = Rc::new(resource);

        let mut bc = BatchCompiler::new(resource.clone());
        // Set through the options (which sanitise to absolute directories),
        // matching the reference driver.
        let mut options = BatchCompilerOptions::new();
        options.base.intermediate_path_set(&intermediate, &resource);
        options.base.output_path_set(&output, &resource);
        options.apply(bc.compiler());
        bc.compiler().register_conversion_rule(Box::new(TextRule));
        bc.compiler().register_converter(ConverterInfo {
            name: "text".to_string(),
            type_id: 7,
            version: "1.0".to_string(),
            flags: ConverterFlags::DEFAULT_FLAGS,
            creator: text_converter as ConverterCreator,
        });
        bc.init_compiler();

        let path = format!("{input}/a.txt");
        bc.build(&[path]);
        bc.fini_compiler();

        assert_eq!(bc.get_failed_count(), 0);
        let produced = std::fs::read_to_string(format!("{output}/a.txt")).unwrap();
        assert_eq!(produced, "hello");
    }
}
