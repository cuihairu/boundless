//! BW-12-01: the Watcher monitoring interface — the path tree and value
//! types (BW-12-01-01), get/set (BW-12-01-02), callable watchers
//! (BW-12-01-03), remote queries and forwarding (BW-12-01-04) and the
//! statistics fields (BW-12-01-05). The source is
//! `programming/bigworld/lib/cstdmf/watcher.{hpp,cpp}` and, for the
//! statistics, `programming/bigworld/lib/server/event_history_stats.{hpp,cpp}`.

pub mod callable;
pub mod event_stats;
pub mod forwarding;
pub mod path_tree;

pub use callable::{ArgDescription, CallResult, Callable, CallableWatcher, ExposeHint};
pub use event_stats::EventHistoryStats;
pub use forwarding::{
    Component, ComponentEntry, ComponentID, ComponentIDList, ComponentReply, ComponentType,
    DecodeError, ExposeHints, ForwardingCollector, ForwardingDest, ForwardingWatcher, ManagerKind,
    TARGET_BASE_APPS, TARGET_BASE_SERVICE_APPS, TARGET_CELL_APPS, TARGET_LEAST_LOADED,
    TARGET_SERVICE_APPS, decode_component_reply, get_component_id_list, is_doc_path, is_empty_path,
    resolve_targets, split_forwarding_path,
};
pub use path_tree::{
    DIRECTORY_STRING, Mode, WATCHER_SEPARATOR, WatcherDataType, WatcherNode, WatcherTree,
    WatcherValue, partition_path, tail,
};
