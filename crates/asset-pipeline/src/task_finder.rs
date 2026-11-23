//! `TaskFinder` port (`discovery/task_finder.cpp`): recursively walks a
//! directory tree and creates a root conversion task for every file a rule
//! matches.
//!
//! Reference quirks preserved:
//! - rules are tried in reverse registration order (the last-registered rule
//!   wins, matching the reference's `crbegin`/`crend` walk);
//! - a `root` task is only created by rules that mark themselves root
//!   (`createRootTask`); a non-root task (`createTask`) is created for any
//!   matching rule;
//! - `iterateDirectory` only recurses into *root tasks* for files that live
//!   under one of the resource search paths (`dissolveFilename` shortened the
//!   path), while sub-directories always recurse;
//! - a root file with no matching rule yields no task (`NULL`); a non-root
//!   file with no matching rule yields a `FAILED` task with the unknown
//!   converter id;
//! - completed tasks are cached by source path so a file reached twice (once
//!   as a root, once as a dependency) is not created twice.
//!
//! The reference holds a `Compiler&` and `BWResource`; here the compiler's
//! `shouldIterate*`/`queueTask` seam is passed per call as `&dyn
//! TaskCompiler` (the reference's `Compiler&`), and the filesystem path
//! services come from a shared `&BwResource`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use resources::bwresource::{BwResource, format_path, get_file_path};
use resources::zip::FileType;

use crate::conversion_rule::ConversionRule;
use crate::conversion_task::{ConversionStatus, ConversionTask, TaskHandle};

/// `Compiler`'s discovery seam: the reference `AssetCompiler` answers
/// `shouldIterateFile`/`shouldIterateDirectory` (the exclusions) and
/// `queueTask`. The real compiler implements this; tests drive it directly.
pub trait TaskCompiler {
    /// `shouldIterateFile`: whether discovery should descend into `file`.
    fn should_iterate_file(&self, file: &str) -> bool;
    /// `shouldIterateDirectory`: whether discovery should descend into `directory`.
    fn should_iterate_directory(&self, directory: &str) -> bool;
    /// `queueTask`: hand a discovered root task to the conversion queue.
    fn queue_task(&self, task: TaskHandle);
}

/// `TaskFinder`: discovers root tasks under a directory tree.
pub struct TaskFinder {
    /// `conversionRules_`: the shared rule list (the reference holds a
    /// reference to the compiler's vector, so `registerConversionRule` after
    /// construction is visible here).
    rules: Rc<RefCell<Vec<Box<dyn ConversionRule>>>>,
    /// `tasks_`: the cache of tasks by absolute source path. Tasks are held in
    /// shared `Rc` cells so the queue and processor observe the same status
    /// mutations (the reference's shared `ConversionTask*`).
    tasks: RefCell<HashMap<String, TaskHandle>>,
    resource: Rc<BwResource>,
}

impl TaskFinder {
    /// `TaskFinder( compiler, rules )`: `rules` is the compiler's shared
    /// conversion-rule list, so rules registered after construction are
    /// visible here (the reference holds a reference to the same vector).
    pub fn new(rules: Rc<RefCell<Vec<Box<dyn ConversionRule>>>>, resource: Rc<BwResource>) -> Self {
        Self {
            rules,
            tasks: RefCell::new(HashMap::new()),
            resource,
        }
    }

    /// `findTasks`: walk `directory` (a file or a directory) and create root
    /// tasks for matching files.
    pub fn find_tasks(&self, compiler: &dyn TaskCompiler, directory: &str) {
        let ft = self.resource.get_file_type(directory, None);
        match ft {
            FileType::NotFound => eprintln!("Directory passed in ({directory}) not found"),
            FileType::File => self.iterate_file(compiler, directory),
            FileType::Directory => self.iterate_directory(compiler, directory),
            // The reference has no `FT_ARCHIVE` arm here; an archive root is
            // left alone.
            FileType::Archive => {}
        }
    }

    /// `iterateFile`: create and queue a root task when the compiler accepts
    /// the file and a rule matches it.
    fn iterate_file(&self, compiler: &dyn TaskCompiler, file: &str) {
        if !compiler.should_iterate_file(file) {
            return;
        }
        if let Some(task) = self.get_task_inner(file, true) {
            compiler.queue_task(task);
        }
    }

    /// `iterateDirectory`: recurse. Files are turned into root tasks only when
    /// they sit under a resource search path; sub-directories always recurse.
    fn iterate_directory(&self, compiler: &dyn TaskCompiler, directory: &str) {
        let path = format_path(directory);
        if !compiler.should_iterate_directory(&path) {
            return;
        }

        let relative = self.resource.dissolve_filename(&path);
        let is_bw_path = relative.len() < path.len();

        let Some(entries) = self.resource.file_system().read_directory(&path) else {
            return;
        };
        for entry in entries {
            let sub_path = format!("{path}{entry}");
            let ft = self.resource.get_file_type(&sub_path, None);
            match ft {
                FileType::File if is_bw_path => self.iterate_file(compiler, &sub_path),
                FileType::Directory => self.iterate_directory(compiler, &sub_path),
                _ => {}
            }
        }
    }

    /// `getTask( filename )`: the task for `filename` (creating it if needed).
    /// A file the compiler excludes, or one no rule matches, yields a
    /// `FAILED` task with the unknown converter id.
    pub fn get_task(&self, compiler: &dyn TaskCompiler, filename: &str) -> TaskHandle {
        let directory = get_file_path(filename);
        if !compiler.should_iterate_directory(&directory) || !compiler.should_iterate_file(filename)
        {
            let failed = ConversionTask::failed(filename).handle();
            self.tasks
                .borrow_mut()
                .insert(filename.to_string(), failed.clone());
            return failed;
        }
        self.get_task_inner(filename, false)
            .expect("a non-root file with no matching rule yields a FAILED task")
    }

    /// `getTask( filename, bRoot )`: the task for `filename` when it has a
    /// matching rule; `None` for a root file with no rule (the reference's
    /// `NULL`). Created tasks are cached by source path.
    fn get_task_inner(&self, filename: &str, root: bool) -> Option<TaskHandle> {
        let source = filename.to_string();

        // Cache hit: the task already exists.
        if let Some(task) = self.tasks.borrow().get(&source) {
            return Some(task.clone());
        }

        // Create one from the rules, tried in reverse registration order.
        let mut task = ConversionTask::new(&source);
        let mut matched = false;
        for rule in self.rules.borrow().iter().rev() {
            let ok = if root {
                rule.create_root_task(filename, &mut task)
            } else {
                rule.create_task(filename, &mut task)
            };
            if ok {
                matched = true;
                break;
            }
        }

        if !matched {
            if root {
                return None;
            }
            // Warning + FAILED unknown task (the reference's inline build).
            let failed = ConversionTask::failed(&source).handle();
            self.tasks
                .borrow_mut()
                .insert(source.clone(), failed.clone());
            return Some(failed);
        }

        task.status = ConversionStatus::New;
        let handle = task.handle();
        self.tasks
            .borrow_mut()
            .insert(source.clone(), handle.clone());
        Some(handle)
    }

    /// The number of tasks currently cached (diagnostics).
    pub fn task_count(&self) -> usize {
        self.tasks.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion_task::UNKNOWN_ID;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A compiler that accepts every file and records queued root tasks.
    #[derive(Default)]
    struct MockCompiler {
        queued: RefCell<Vec<TaskHandle>>,
        exclude_file: String,
        exclude_dir: String,
    }

    impl TaskCompiler for MockCompiler {
        fn should_iterate_file(&self, file: &str) -> bool {
            file != self.exclude_file
        }
        fn should_iterate_directory(&self, directory: &str) -> bool {
            directory != self.exclude_dir
        }
        fn queue_task(&self, task: TaskHandle) {
            self.queued.borrow_mut().push(task);
        }
    }

    /// A rule that claims `.model` files as root and `.anim` files as
    /// non-root dependencies (with distinct converter ids).
    #[derive(Clone, Copy)]
    struct ModelRule;

    impl ConversionRule for ModelRule {
        fn create_root_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
            if source_file.ends_with(".model") {
                task.converter_id = 1;
                task.converter_version = "1.0".to_string();
                true
            } else {
                false
            }
        }

        fn create_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
            if source_file.ends_with(".anim") {
                task.converter_id = 2;
                true
            } else {
                false
            }
        }
    }

    /// Create a fresh temp directory (the crate-wide test convention: a
    /// counter + pid + tag under the system temp dir).
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

    /// Build a `BwResource` rooted at a fresh temp directory, with the
    /// `EDITOR_ENABLED` empty-base root enabled so absolute paths resolve
    /// (the asset tools run with this; `TaskFinder` walks absolute paths).
    fn make_resource(tag: &str) -> (String, Rc<BwResource>) {
        let root = temp_root(tag);
        let mut resource = BwResource::init_paths(&[root.as_str()]);
        resource.enable_editor_default_root();
        (root, Rc::new(resource))
    }

    #[test]
    fn find_root_tasks_in_directory_tree() {
        let (root, resource) = make_resource("roots");
        // a.model (root), sub/b.anim (non-root, not queued as root), c.txt (no rule).
        std::fs::write(format!("{root}/a.model"), b"x").unwrap();
        std::fs::create_dir(format!("{root}/sub")).unwrap();
        std::fs::write(format!("{root}/sub/b.anim"), b"y").unwrap();
        std::fs::write(format!("{root}/c.txt"), b"z").unwrap();

        let finder = TaskFinder::new(Rc::new(RefCell::new(vec![Box::new(ModelRule)])), resource);
        let compiler = MockCompiler::default();
        finder.find_tasks(&compiler, &root);

        let queued = compiler.queued.borrow();
        assert_eq!(queued.len(), 1, "only the root .model is queued");
        let task = queued[0].borrow();
        assert_eq!(task.source, format!("{root}/a.model"));
        assert_eq!(task.converter_id, 1);
        assert_eq!(task.status, ConversionStatus::New);
    }

    #[test]
    fn excluded_file_is_not_queued() {
        let (root, resource) = make_resource("exclude");
        std::fs::write(format!("{root}/a.model"), b"x").unwrap();

        let path = format!("{root}/a.model");
        let finder = TaskFinder::new(Rc::new(RefCell::new(vec![Box::new(ModelRule)])), resource);
        let compiler = MockCompiler {
            exclude_file: path,
            ..Default::default()
        };
        finder.find_tasks(&compiler, &root);
        assert!(compiler.queued.borrow().is_empty());
    }

    #[test]
    fn non_root_file_without_rule_is_failed_task() {
        let (_root, resource) = make_resource("norule");
        let path = "/nonexistent/nope.xyz".to_string();

        let finder = TaskFinder::new(Rc::new(RefCell::new(vec![Box::new(ModelRule)])), resource);
        let task = finder.get_task(&MockCompiler::default(), &path);
        let task = task.borrow();
        assert_eq!(task.status, ConversionStatus::Failed);
        assert_eq!(task.converter_id, UNKNOWN_ID);
    }

    #[test]
    fn root_file_without_rule_yields_no_task() {
        let (_root, resource) = make_resource("rootnorule");
        let path = "/nonexistent/nope.xyz".to_string();

        let finder = TaskFinder::new(Rc::new(RefCell::new(vec![Box::new(ModelRule)])), resource);
        // A root file with no rule produces no task (the reference's NULL).
        let task = finder.get_task_inner(&path, true);
        assert!(task.is_none());
    }

    #[test]
    fn task_is_cached_by_source() {
        let (_root, resource) = make_resource("cache");
        let path = "/nonexistent/a.model".to_string();

        let finder = TaskFinder::new(Rc::new(RefCell::new(vec![Box::new(ModelRule)])), resource);
        let compiler = MockCompiler::default();
        let a = finder.get_task(&compiler, &path);
        let b = finder.get_task(&compiler, &path);
        // The same source resolves to the same shared handle (the reference's
        // pointer identity), and the tasks' contents are equal.
        assert!(
            std::rc::Rc::ptr_eq(&a, &b),
            "same source yields the same handle"
        );
        assert_eq!(*a.borrow(), *b.borrow());
        assert_eq!(finder.task_count(), 1);
    }
}
