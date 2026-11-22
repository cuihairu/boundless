//! `ConversionRule` port (`discovery/conversion_rule.hpp`): the abstract rule
//! that turns a source filename into a conversion task.
//!
//! Reference quirks preserved:
//! - `createTask` defaults to calling `createRootTask` (the base class is a
//!   no-op for every method, so a rule that only overrides the root variant
//!   still answers both);
//! - `getSourceFile` answers `false` by default (no mapping), the concrete
//!   rule maps a destination back to its source via its `sourcePattern`.

use crate::conversion_task::ConversionTask;

/// A rule that can populate a conversion task from a source filename.
pub trait ConversionRule {
    /// `createRootTask`: populate `task` when the rule can match `source_file`
    /// as a *root* asset (one discovered by walking the tree). Returns `false`
    /// by default (no match).
    fn create_root_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
        let _ = (source_file, task);
        false
    }

    /// `createTask`: populate `task` when the rule can match `source_file`.
    /// The base class delegates to `create_root_task`.
    fn create_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
        self.create_root_task(source_file, task)
    }

    /// `getSourceFile`: map a destination `file` back to its source filename.
    /// Returns `None` by default (no mapping).
    fn get_source_file(&self, file: &str) -> Option<String> {
        let _ = file;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion_task::ConversionTask;

    /// A rule that only overrides the root variant still answers `create_task`
    /// (the base class delegates).
    #[derive(Clone, Copy)]
    struct RootOnlyRule;

    impl ConversionRule for RootOnlyRule {
        fn create_root_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
            if source_file.ends_with(".model") {
                task.converter_id = 0xABC12345;
                true
            } else {
                false
            }
        }
    }

    #[test]
    fn create_task_defaults_to_root_task() {
        let rule = RootOnlyRule;
        let mut task = ConversionTask::new("a.model");
        assert!(rule.create_task("a.model", &mut task));
        assert_eq!(task.converter_id, 0xABC12345);

        let mut other = ConversionTask::new("b.txt");
        assert!(!rule.create_task("b.txt", &mut other));
        assert_eq!(other.converter_id, crate::conversion_task::UNKNOWN_ID);
    }

    /// The no-op base returns `false`/`None` for every method.
    #[derive(Clone, Copy)]
    struct NoopRule;

    impl ConversionRule for NoopRule {}

    #[test]
    fn noop_rule_matches_nothing() {
        let rule = NoopRule;
        let mut task = ConversionTask::new("a.model");
        assert!(!rule.create_root_task("a.model", &mut task));
        assert!(!rule.create_task("a.model", &mut task));
        assert!(rule.get_source_file("a.model").is_none());
    }
}
