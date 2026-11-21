//! `ConversionTask` port (`conversion/conversion_task.hpp`): the record of an
//! asset being converted.
//!
//! Reference quirks preserved:
//! - `status_` order is load-bearing for the pipeline's logic and must not
//!   change (`NEW` → `QUEUED` → `PROCESSING` → the two dep states →
//!   `NEEDS_CONVERSION` → `DONE`/`FAILED`);
//! - `s_unknownId` is `0`, the converter id used when no rule matched;
//! - `subTasks_` is the `(task, critical)` dependency list (source path and
//!   critical flag stand in for the reference's `const ConversionTask*` and
//!   `bool`); it is populated by the task processor, not discovery;
//! - `fileHandle_` (the reference's `HANDLE` held to stop external edits
//!   during conversion) is a processor concern and is not modelled here.

/// The stage an asset has reached in the conversion process.
///
/// The reference enum's numeric order is essential to the asset pipeline's
/// logic and is preserved here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum ConversionStatus {
    /// `NEW`: created but not queued.
    New = 0,
    /// `QUEUED`: waiting in the queue.
    Queued = 1,
    /// `PROCESSING`: pulled from the queue and being processed.
    Processing = 2,
    /// `NEEDS_PRIMARY_DEPS`: needs its primary dependencies evaluated.
    NeedsPrimaryDeps = 3,
    /// `NEEDS_SECONDARY_DEPS`: needs its secondary dependencies evaluated.
    NeedsSecondaryDeps = 4,
    /// `NEEDS_CONVERSION`: ready to be converted.
    NeedsConversion = 5,
    /// `DONE`: conversion finished.
    Done = 6,
    /// `FAILED`: an earlier state failed.
    Failed = 7,
}

/// A dependent task: `(source path, critical)`. The reference holds a
/// `(const ConversionTask*, bool)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubTask {
    /// `subTasks_[i].first`: the source path of the depended-on task.
    pub source: String,
    /// `subTasks_[i].second`: whether the dependency is critical.
    pub critical: bool,
}

/// `ConversionTask`: everything needed to know how to convert one asset and
/// where it is in the process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionTask {
    /// `source_`: filename of the source asset.
    pub source: String,
    /// `converterId_`: id of the converter that will process this asset.
    pub converter_id: u64,
    /// `converterVersion_`: the converter's version string.
    pub converter_version: String,
    /// `converterParams_`: parameters the converter is initialised with.
    pub converter_params: String,
    /// `status_`: how far the asset has progressed.
    pub status: ConversionStatus,
    /// `subTasks_`: tasks this task depends on.
    pub sub_tasks: Vec<SubTask>,
    /// `threadId_`: the id of the thread this task is waiting on.
    pub thread_id: u32,
}

/// `ConversionTask::s_unknownId`: the converter id of a task the pipeline
/// does not know how to convert.
pub const UNKNOWN_ID: u64 = 0;

impl Default for ConversionTask {
    fn default() -> Self {
        Self {
            source: String::new(),
            converter_id: UNKNOWN_ID,
            converter_version: String::new(),
            converter_params: String::new(),
            status: ConversionStatus::New,
            sub_tasks: Vec::new(),
            thread_id: 0,
        }
    }
}

impl ConversionTask {
    /// A freshly created task for `source` (the reference's `new ConversionTask`
    /// with the members filled in by the rule).
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            ..Self::default()
        }
    }

    /// The "no rule matched" task the reference builds inline (unknown
    /// converter, `FAILED` status, thread id 0).
    pub fn failed(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            converter_id: UNKNOWN_ID,
            converter_version: String::new(),
            converter_params: String::new(),
            status: ConversionStatus::Failed,
            sub_tasks: Vec::new(),
            thread_id: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status enum's numeric order matches the reference exactly (the
    /// pipeline's logic depends on it).
    #[test]
    fn status_order_matches_reference() {
        assert_eq!(ConversionStatus::New as u32, 0);
        assert_eq!(ConversionStatus::Queued as u32, 1);
        assert_eq!(ConversionStatus::Processing as u32, 2);
        assert_eq!(ConversionStatus::NeedsPrimaryDeps as u32, 3);
        assert_eq!(ConversionStatus::NeedsSecondaryDeps as u32, 4);
        assert_eq!(ConversionStatus::NeedsConversion as u32, 5);
        assert_eq!(ConversionStatus::Done as u32, 6);
        assert_eq!(ConversionStatus::Failed as u32, 7);
    }

    /// `s_unknownId` is 0.
    #[test]
    fn unknown_id_is_zero() {
        assert_eq!(UNKNOWN_ID, 0);
    }

    /// `new` builds a task with `NEW` status and an unknown converter until a
    /// rule fills it in; `failed` is the no-rule task.
    #[test]
    fn new_and_failed_tasks() {
        let task = ConversionTask::new("a.model");
        assert_eq!(task.source, "a.model");
        assert_eq!(task.status, ConversionStatus::New);
        assert_eq!(task.converter_id, UNKNOWN_ID);

        let failed = ConversionTask::failed("b.model");
        assert_eq!(failed.status, ConversionStatus::Failed);
        assert_eq!(failed.converter_id, UNKNOWN_ID);
    }
}
