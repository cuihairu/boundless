//! BW-12-01: the Watcher monitoring interface — the path tree and value
//! types (BW-12-01-01), then get/set (BW-12-01-02), callable watchers
//! (BW-12-01-03), remote queries and forwarding (BW-12-01-04) and the
//! statistics fields (BW-12-01-05). The source is
//! `programming/bigworld/lib/cstdmf/watcher.{hpp,cpp}`.

pub mod path_tree;

pub use path_tree::{
    partition_path, tail, Mode, WatcherDataType, WatcherNode, WatcherTree, WatcherValue,
    DIRECTORY_STRING, WATCHER_SEPARATOR,
};
