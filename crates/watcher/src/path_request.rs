//! The watcher path request machinery (`lib/cstdmf/watcher_path_request.*`):
//! the handler objects that turn one watcher path into a reply stream,
//! version 2 (typed protocol) and version 1 (NUL-terminated strings).
//!
//! The reference request carries a `WatcherPathRequestNotification*` parent
//! that the nub installs to collect the finished packet. The nub is out of
//! this port's scope, so [`WatcherPathRequestV2::notify_parent`] and its V1
//! counterpart record the completion instead; [`Self::completed`] and
//! [`Self::last_replies`] read it back.

use std::cell::RefCell;
use std::rc::Rc;

use crate::stream::{
    MODE_READ_ONLY, MemoryIStream, MemoryOStream, StreamValue, TYPE_UNKNOWN, WRITES_TOGGLE_PATH,
    mode_byte, set_writes_enabled, stream_to_value, value_to_stream, writes_enabled,
};
use crate::{DirectoryWatcher, Mode, Watcher, WatcherValue, add_watcher, root};

/// `WatcherPathRequest` for watcher protocol version 2
/// (`watcher_path_request.hpp:132-184`).
pub struct WatcherPathRequestV2 {
    request_path: String,
    /// Original path requested via the watcher nub, used when visiting
    /// directories and `request_path` may be altered.
    original_request_path: String,
    /// True when the sequence number for the path request has already been
    /// added to the result stream.
    has_seq_num: bool,
    /// True when the path request is visiting children and shouldn't notify
    /// parent of completion.
    visiting_directories: bool,
    /// Data obtained from the watcher nub to use in a set operation.
    stream_data: Option<Vec<u8>>,
    /// Output stream for the watcher path request's data.
    result: MemoryOStream,
    /// The recorded completion of [`Self::notify_parent`].
    completed: bool,
    /// The reply count of the last recorded completion.
    last_replies: i32,
}

impl WatcherPathRequestV2 {
    /// `WatcherPathRequestV2::WatcherPathRequestV2` (`watcher_path_request.cpp:95-101`).
    pub fn new(path: &str) -> Self {
        Self {
            request_path: path.to_string(),
            original_request_path: String::new(),
            has_seq_num: false,
            visiting_directories: false,
            stream_data: None,
            result: MemoryOStream::new(),
            completed: false,
            last_replies: 0,
        }
    }

    /// `WatcherPathRequest::getPath`.
    pub fn path(&self) -> &str {
        &self.request_path
    }

    /// `WatcherPathRequestV2::setResult` split for the value arm
    /// (`watcher_path_request.cpp:116-161`, `mode != WT_DIRECTORY`): a set
    /// request reaching this point has a full data stream, so push the
    /// status boolean on and then notify the parent. `desc` is kept for
    /// signature parity -- the reference body ignores it too.
    pub fn set_result_value(&mut self, desc: &str, mode: Mode) {
        let _ = desc;
        if mode == Mode::Directory {
            // A directory answer must go through `set_result_directory`;
            // the reference routes both shapes through one `setResult`.
            return;
        }
        if mode == Mode::Invalid {
            return;
        }

        // If we are a set request and have reached this point the operation
        // has succeeded and we have a full data stream, so now just push on
        // the status boolean at the end and then we are done.
        if self.stream_data.is_some() {
            self.result.write_bool(true);
        }

        // At this point we have type, mode, size, and data on the stream.
        // Ready to fill in the packet now -- tell our parent we have
        // collected all our data.
        if !self.visiting_directories {
            self.notify_parent(1);
        }
    }

    /// `WatcherPathRequestV2::setResult` split for the directory arm
    /// (`watcher_path_request.cpp:119-133`): the value has already been
    /// streamed on, so visit the children and then notify the parent. The
    /// `visitingDirectories_` guard keeps a nested directory answer from
    /// re-entering.
    pub fn set_result_directory(&mut self, desc: &str, directory: &dyn Watcher) {
        let _ = desc;
        if self.visiting_directories {
            return;
        }
        self.visiting_directories = true;
        self.original_request_path = self.request_path.clone();

        // `DirectoryWatcher::visitChildren` reports the child count before
        // the walk (`watcher.cpp:253`); counting first keeps the wire order
        // count-then-entries as the reference lays it out.
        let mut count: i32 = 0;
        let mut counter = |_: &str, _: &Rc<RefCell<dyn Watcher>>| {
            count += 1;
        };
        directory.visit_children("", &mut counter);
        self.result.write_i32(count);

        let mut visitor = |label: &str, child: &Rc<RefCell<dyn Watcher>>| {
            self.add_watcher_path(label, child);
        };
        directory.visit_children("", &mut visitor);

        self.request_path = self.original_request_path.clone();
        self.visiting_directories = false;

        // Ready to fill in the packet now.
        self.notify_parent(1);
    }

    /// `WatcherPathRequestV2::addWatcherPath` (`watcher_path_request.cpp:254-283`):
    /// stream one child of a visited directory -- its value, then its label.
    /// Always `true`, or the visiting stream won't be completed.
    pub fn add_watcher_path(&mut self, label: &str, watcher: &Rc<RefCell<dyn Watcher>>) -> bool {
        if self.original_request_path.is_empty() {
            self.request_path = label.to_string();
        } else {
            self.request_path = format!("{}/{}", self.original_request_path, label);
        }

        // We are using a reference to the correct watcher now, so no need
        // to pass in the path to search for.
        let status = watcher.borrow().get_as_stream("", self);
        if !status {
            // If the get operation failed, we still need to notify the
            // parent, so keep the response well-formed.
            self.result.write_u8(TYPE_UNKNOWN);
            self.result.write_u8(MODE_READ_ONLY);
            self.result.write_string_length(0);
        }

        // Directory entries require an appended label.
        self.result.write_string(label);

        true
    }

    /// `WatcherPathRequestV2::setWatcherValue` (`watcher_path_request.cpp:165-199`):
    /// apply the attached payload to the root tree. The reference always
    /// answers `true` -- a rejected set is completed through
    /// `complete_failed_set` instead.
    pub fn set_watcher_value(&mut self) -> bool {
        add_watchers();

        let blocked = !writes_enabled() && self.request_path != WRITES_TOGGLE_PATH;

        let path = self.request_path.clone();
        let status = !blocked && root().borrow_mut().set_from_stream(&path, self);

        // A write the permission toggle stopped is audited as "blocked";
        // the port drops the audit log with the rest of the messaging.

        if !status {
            self.complete_failed_set();
        }

        true
    }

    /// `WatcherPathRequestV2::completeFailedSet` (`watcher_path_request.cpp:207-229`):
    /// fill the reply with the default failed-set result and notify the
    /// parent. Shared by the permission-blocked and the set-failure paths.
    fn complete_failed_set(&mut self) {
        // The reference drains the input stream here; the port's payload is
        // an owned buffer nothing reads afterwards.

        // NB: the sequence number should already be on the stream here, so
        // only fill out a default result while that is all we carry.
        if self.result.size() <= 4 {
            self.result.write_u8(TYPE_UNKNOWN);
            self.result.write_u8(MODE_READ_ONLY);
            self.result.write_string_length(0);
            self.result.write_bool(false);
        }

        // We're pretty much done setting watcher value (failed). Ready to
        // fill in the packet now.
        self.notify_parent(1);
    }

    /// `WatcherPathRequestV2::fetchWatcherValue` (`watcher_path_request.cpp:233-250`):
    /// run the root get for this request's path, answering with a default
    /// UNKNOWN entry when the path does not resolve.
    pub fn fetch_watcher_value(&mut self) {
        let path = self.request_path.clone();
        if !root().borrow().get_as_stream(&path, self) {
            // If the get operation failed, we still need to notify the
            // parent so it can continue with its response.
            self.result.write_u8(TYPE_UNKNOWN);
            self.result.write_u8(MODE_READ_ONLY);
            self.result.write_string_length(0);

            // And notify the parent of our failure.
            self.notify_parent(1);
        }
    }

    /// `WatcherPathRequestV2::addWatcherCount` (`watcher_path_request.cpp:353-356`).
    pub fn add_watcher_count(&mut self, count: i32) {
        self.result.write_i32(count);
    }

    /// `WatcherPathRequestV2::setSequenceNumber` (`watcher_path_request.cpp:360-367`):
    /// the first call wins.
    pub fn set_sequence_number(&mut self, seq_num: u32) {
        if self.has_seq_num {
            return;
        }
        self.has_seq_num = true;
        self.result.write_u32(seq_num);
    }

    /// `WatcherPathRequestV2::setPacketData` (`watcher_path_request.cpp:297-349`,
    /// both overloads folded): attach the payload of a set operation. The
    /// reference refuses a second attachment; so does the port.
    pub fn set_packet_data(&mut self, data: Vec<u8>) -> bool {
        if self.stream_data.is_some() {
            return false;
        }
        self.stream_data = Some(data);
        true
    }

    /// Whether a set payload has been attached (`getValueStream() != NULL`).
    pub fn has_value_stream(&self) -> bool {
        self.stream_data.is_some()
    }

    /// `WatcherPathRequestV2::getValueStream` folded onto the owned bytes:
    /// the set payload to decode. The reference hands out a stream pointer;
    /// the port hands out the bytes so the caller builds its borrowing
    /// stream and is forced to drop it before answering.
    pub fn value_data(&self) -> Option<&[u8]> {
        self.stream_data.as_deref()
    }

    /// `WatcherPathRequestV2::getResultStream`.
    pub fn result_mut(&mut self) -> &mut MemoryOStream {
        &mut self.result
    }

    /// `WatcherPathRequestV2::getData`.
    pub fn data(&self) -> &[u8] {
        self.result.data()
    }

    /// `WatcherPathRequestV2::getDataSize`.
    pub fn data_size(&self) -> usize {
        self.result.size()
    }

    /// Whether the request completed (`notifyParent` reached a parent).
    pub fn completed(&self) -> bool {
        self.completed
    }

    /// The reply count of the last completion.
    pub fn last_replies(&self) -> i32 {
        self.last_replies
    }

    /// `WatcherPathRequest::notifyParent` (`watcher_path_request.cpp:21-27`):
    /// the reference invokes the creator's `notifyComplete`; the port
    /// records the completion.
    fn notify_parent(&mut self, replies: i32) {
        self.completed = true;
        self.last_replies = replies;
    }
}

/// `WatcherPathRequest` for watcher protocol version 1
/// (`watcher_path_request.hpp:192-224`).
pub struct WatcherPathRequestV1 {
    request_path: String,
    /// Output stream for the watcher path request's data.
    result_stream: MemoryOStream,
    /// String used in set operations for this path request.
    set_value: String,
    /// Number of watcher path replies contained in this path request.
    contained_replies: i32,
    /// True if the watcher request reply should contain a description.
    use_description: bool,
    /// The recorded completion of [`Self::notify_parent`].
    completed: bool,
    /// The reply count of the last recorded completion.
    last_replies: i32,
}

impl WatcherPathRequestV1 {
    /// `WatcherPathRequestV1::WatcherPathRequestV1` (`watcher_path_request.cpp:393-397`).
    pub fn new(path: &str) -> Self {
        Self {
            request_path: path.to_string(),
            result_stream: MemoryOStream::new(),
            set_value: String::new(),
            contained_replies: 1,
            use_description: false,
            completed: false,
            last_replies: 0,
        }
    }

    /// `WatcherPathRequest::getPath`.
    pub fn path(&self) -> &str {
        &self.request_path
    }

    /// `WatcherPathRequestV1::setValueData` (`watcher_path_request.cpp:403-406`).
    pub fn set_value_data(&mut self, value: &str) {
        self.set_value = value.to_string();
    }

    /// `WatcherPathRequestV1::useDescription` (`watcher_path_request.hpp:199`).
    pub fn set_use_description(&mut self, should_use: bool) {
        self.use_description = should_use;
    }

    /// `WatcherPathRequestV1::setWatcherValue` (`watcher_path_request.cpp:410-443`):
    /// apply the stored string, then read the path back for the reply.
    pub fn set_watcher_value(&mut self) -> bool {
        add_watchers();

        let blocked = !writes_enabled() && self.request_path != WRITES_TOGGLE_PATH;

        let path = self.request_path.clone();
        let value = self.set_value.clone();
        let status = !blocked && root().borrow_mut().set_from_string(&path, &value);

        if !status {
            // Error out + notify our parent we have nothing.
            self.notify_parent(0);
            return false;
        }

        self.fetch_watcher_value();

        true
    }

    /// `WatcherPathRequestV1::fetchWatcherValue` (`watcher_path_request.cpp:447-490`):
    /// a leaf appends `path\0value\0` (plus `desc\0` when asked for); a
    /// directory collects one entry per child through the visitor.
    pub fn fetch_watcher_value(&mut self) {
        // NB: the reference call passes (desc, result) where the signature
        // takes (result, desc) -- `watcher_path_request.cpp:449-454` -- so
        // its scalar reply carries the comment in the value slot and the
        // value in the description slot. The local names mirror the
        // reference's to keep the swap readable; wire fidelity is kept.
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        let mut result = String::new();

        let path = self.request_path.clone();
        if !root()
            .borrow()
            .get_as_string(&path, &mut desc, &mut result, &mut mode)
        {
            self.notify_parent(0);
            return;
        }

        if mode == Mode::Directory {
            self.contained_replies = 0;

            // Handle a directory path request.
            let base = self.request_path.clone();
            let mut visitor = |label: &str, child: &Rc<RefCell<dyn Watcher>>| {
                self.visit_child(&base, label, child);
            };
            root().borrow().visit_children(&base, &mut visitor);

            // At this point we've collected results from the directory's
            // children watchers; the count is the amount of children.
            let replies = self.contained_replies;
            self.notify_parent(replies);
        } else if mode != Mode::Invalid {
            // Add the result onto the final result stream.
            self.append_c_string(&path);
            self.append_c_string(&result);
            if self.use_description {
                self.append_c_string(&desc);
            }

            // Tell our parent we have collected all our data.
            self.notify_parent(1);
        }
    }

    /// `WatcherPathRequestV1::visit` (`watcher_path_request.cpp:508-531`)
    /// with the per-child string read folded in: the reference reaches this
    /// through the `WatcherPathRequestWatcherVisitor` adapter, which runs
    /// `getAsString` on each visited child (`watcher_path_request.hpp:241-251`).
    fn visit_child(&mut self, base: &str, label: &str, child: &Rc<RefCell<dyn Watcher>>) -> bool {
        let mut value_str = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        child
            .borrow()
            .get_as_string("", &mut value_str, &mut desc, &mut mode);

        let path = if base.is_empty() {
            label.to_string()
        } else {
            format!("{base}/{label}")
        };
        self.append_c_string(&path);
        self.append_c_string(&value_str);
        if self.use_description {
            self.append_c_string(&desc);
        }
        self.contained_replies += 1;
        true
    }

    /// The reference's `addBlob( str.c_str(), str.size() + 1 )` shape: the
    /// text plus its NUL terminator.
    fn append_c_string(&mut self, value: &str) {
        self.result_stream.add_blob(value.as_bytes());
        self.result_stream.add_blob(&[0]);
    }

    /// `WatcherPathRequestV1::getData`.
    pub fn data(&self) -> &[u8] {
        self.result_stream.data()
    }

    /// `WatcherPathRequestV1::getDataSize`.
    pub fn data_size(&self) -> usize {
        self.result_stream.size()
    }

    /// Whether the request completed (`notifyParent` reached a parent).
    pub fn completed(&self) -> bool {
        self.completed
    }

    /// The reply count of the last completion.
    pub fn last_replies(&self) -> i32 {
        self.last_replies
    }

    /// `WatcherPathRequest::notifyParent`: the reference invokes the
    /// creator's `notifyComplete`; the port records the completion.
    fn notify_parent(&mut self, replies: i32) {
        self.completed = true;
        self.last_replies = replies;
    }
}

// The lazily-mounted flag of `WatcherPathRequest::addWatchers`.
thread_local! {
    static ADDED_WATCHERS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `WatcherPathRequest::addWatchers` (`watcher_path_request.cpp:62-83`):
/// mount the watcher write permission controls ("watcherWrites") on the
/// root watcher, once, on the first watcher write -- so apps do not need to
/// wire the toggle up themselves.
pub fn add_watchers() {
    ADDED_WATCHERS.with(|added| {
        if added.get() {
            return;
        }
        added.set(true);
    });

    let mut leaf = WritesToggleWatcher {
        comment: String::new(),
    };
    leaf.set_comment(
        "When false, watcher writes that arrive over the network are rejected. \
         This toggle itself always stays writable"
            .to_string(),
    );

    let directory: Rc<RefCell<DirectoryWatcher>> = Rc::new(RefCell::new(DirectoryWatcher::new()));
    let leaf: Rc<RefCell<dyn Watcher>> = Rc::new(RefCell::new(leaf));
    if !directory.borrow_mut().add_child("allowed", leaf) {
        return;
    }
    let directory: Rc<RefCell<dyn Watcher>> = directory;
    let _ = add_watcher("watcherWrites", directory);
}

/// The `watcherWrites/allowed` leaf (`watcher_path_request.cpp:72-82`). The
/// reference mounts a `DataWatcher<bool>` pointing straight at the
/// process-global `s_writesEnabled_`; the port cannot alias a thread-local
/// through `Rc<RefCell<T>>`, so this leaf reads and writes
/// [`writes_enabled`] instead.
struct WritesToggleWatcher {
    comment: String,
}

impl Watcher for WritesToggleWatcher {
    fn get_as_string(
        &self,
        path: &str,
        result: &mut String,
        desc: &mut String,
        mode: &mut Mode,
    ) -> bool {
        if path.is_empty() {
            *result = writes_enabled().to_cpp_string();
            *desc = self.comment.clone();
            *mode = Mode::ReadWrite;
            true
        } else {
            false
        }
    }

    fn set_from_string(&mut self, path: &str, value: &str) -> bool {
        if path.is_empty()
            && let Some(parsed) = bool::from_cpp_string(value)
        {
            set_writes_enabled(parsed);
            return true;
        }
        false
    }

    fn get_as_stream(&self, path: &str, request: &mut WatcherPathRequestV2) -> bool {
        if path.is_empty() {
            let mode = Mode::ReadWrite;
            value_to_stream(
                request.result_mut(),
                &StreamValue::Bool(writes_enabled()),
                mode_byte(mode),
            );
            request.set_result_value(&self.comment, mode);
            true
        } else {
            false
        }
    }

    /// The `DataWatcher<bool>::setFromStream` shape (`watcher.hpp:2657-2681`):
    /// decode, toggle, then always stream the flag back in READ_WRITE.
    fn set_from_stream(&mut self, path: &str, request: &mut WatcherPathRequestV2) -> bool {
        let mut status = false;
        if path.is_empty() {
            if let Some(data) = request.value_data() {
                let mut input = MemoryIStream::new(data);
                if let Some(parsed) = stream_to_value::<bool>(&mut input) {
                    set_writes_enabled(parsed);
                    status = true;
                }
            }

            let mode = Mode::ReadWrite;
            value_to_stream(
                request.result_mut(),
                &StreamValue::Bool(writes_enabled()),
                mode_byte(mode),
            );
            request.set_result_value(&self.comment, mode);
        }
        status
    }

    fn comment(&self) -> &str {
        &self.comment
    }

    fn set_comment(&mut self, comment: String) {
        self.comment = comment;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::add_watcher_value;
    use crate::fini;
    use crate::stream::{
        MODE_CALLABLE, MODE_DIRECTORY, MODE_READ_WRITE, TYPE_BOOL, TYPE_INT,
        calculate_packed_int_size,
    };
    use crate::{
        CallableWatcher, DataType, ExposeHint, NoArgFuncCallableWatcher, add_watcher, get_watcher,
        get_watcher_string, set_watcher_string,
    };

    /// A leaf get answers seqNum + type + mode + payload.
    #[test]
    fn v2_get_streams_a_typed_leaf() {
        fini();
        let value = Rc::new(RefCell::new(7i32));
        add_watcher_value("a/num", &value, Mode::ReadWrite, "a number");

        let mut request = WatcherPathRequestV2::new("a/num");
        request.set_sequence_number(9);
        request.set_sequence_number(11); // only the first call lands
        request.fetch_watcher_value();
        assert!(request.completed());
        assert_eq!(request.last_replies(), 1);
        assert_eq!(
            request.data(),
            &[9, 0, 0, 0, TYPE_INT, MODE_READ_WRITE, 4, 7, 0, 0, 0]
        );
    }

    /// A directory get answers the "<DIR>" marker, the child count, and one
    /// value stream + label per child, in label order.
    #[test]
    fn v2_get_walks_a_directory() {
        fini();
        let x = Rc::new(RefCell::new(1i32));
        let s = Rc::new(RefCell::new("two".to_string()));
        add_watcher_value("d/x", &x, Mode::ReadOnly, "");
        add_watcher_value("d/s", &s, Mode::ReadOnly, "");

        let mut request = WatcherPathRequestV2::new("d");
        request.set_sequence_number(1);
        request.fetch_watcher_value();
        assert!(request.completed());

        let mut expected = MemoryOStream::new();
        expected.write_u32(1);
        value_to_stream(
            &mut expected,
            &StreamValue::Str("<DIR>".to_string()),
            MODE_DIRECTORY,
        );
        expected.write_i32(2);
        value_to_stream(
            &mut expected,
            &StreamValue::Str("two".to_string()),
            MODE_READ_ONLY,
        );
        expected.write_string("s");
        value_to_stream(&mut expected, &StreamValue::Int(1), MODE_READ_ONLY);
        expected.write_string("x");
        assert_eq!(request.data(), expected.data());
    }

    /// A nested directory child answers just its "<DIR>" marker inside the
    /// parent's walk -- the visiting guard stops the re-entry.
    #[test]
    fn nested_directories_answer_only_their_marker() {
        fini();
        let leaf = Rc::new(RefCell::new(1i32));
        add_watcher_value("d/inner/leaf", &leaf, Mode::ReadOnly, "");

        let mut request = WatcherPathRequestV2::new("d");
        request.set_sequence_number(2);
        request.fetch_watcher_value();

        let mut expected = MemoryOStream::new();
        expected.write_u32(2);
        value_to_stream(
            &mut expected,
            &StreamValue::Str("<DIR>".to_string()),
            MODE_DIRECTORY,
        );
        expected.write_i32(1);
        value_to_stream(
            &mut expected,
            &StreamValue::Str("<DIR>".to_string()),
            MODE_DIRECTORY,
        );
        expected.write_string("inner");
        assert_eq!(request.data(), expected.data());
    }

    /// An unresolvable get still completes, with the UNKNOWN default entry.
    #[test]
    fn v2_get_of_a_missing_path_completes_with_unknown() {
        fini();
        let mut request = WatcherPathRequestV2::new("nope");
        request.set_sequence_number(3);
        request.fetch_watcher_value();
        assert!(request.completed());
        assert_eq!(
            request.data(),
            &[3, 0, 0, 0, TYPE_UNKNOWN, MODE_READ_ONLY, 0]
        );
    }

    /// A set applies the payload and answers the new value plus the status
    /// boolean.
    #[test]
    fn v2_set_applies_the_payload_and_confirms() {
        fini();
        let value = Rc::new(RefCell::new(1i32));
        add_watcher_value("a/num", &value, Mode::ReadWrite, "");

        let mut payload = MemoryOStream::new();
        value_to_stream(&mut payload, &StreamValue::Int(9), MODE_READ_WRITE);

        let mut request = WatcherPathRequestV2::new("a/num");
        request.set_sequence_number(3);
        assert!(request.set_packet_data(payload.data().to_vec()));
        assert!(!request.set_packet_data(vec![0])); // one payload only
        assert!(request.set_watcher_value());
        assert_eq!(*value.borrow(), 9);

        let mut expected = MemoryOStream::new();
        expected.write_u32(3);
        value_to_stream(&mut expected, &StreamValue::Int(9), MODE_READ_WRITE);
        expected.write_bool(true);
        assert_eq!(request.data(), expected.data());
    }

    /// A set on a missing path answers the default failed-set result.
    #[test]
    fn v2_set_failure_answers_the_default_result() {
        fini();
        let mut request = WatcherPathRequestV2::new("missing/leaf");
        request.set_sequence_number(4);
        request.set_packet_data(vec![TYPE_INT, MODE_READ_WRITE, 4, 1, 0, 0, 0]);
        assert!(request.set_watcher_value());
        assert!(request.completed());
        assert_eq!(
            request.data(),
            &[4, 0, 0, 0, TYPE_UNKNOWN, MODE_READ_ONLY, 0, 0]
        );
    }

    /// A read-only leaf refuses the stream set.
    #[test]
    fn read_only_leaves_refuse_stream_sets() {
        fini();
        let value = Rc::new(RefCell::new(1i32));
        add_watcher_value("ro", &value, Mode::ReadOnly, "");

        let mut request = WatcherPathRequestV2::new("ro");
        request.set_sequence_number(7);
        request.set_packet_data(vec![TYPE_INT, MODE_READ_WRITE, 4, 9, 0, 0, 0]);
        assert!(request.set_watcher_value());
        assert_eq!(*value.borrow(), 1);
        assert_eq!(
            request.data(),
            &[7, 0, 0, 0, TYPE_UNKNOWN, MODE_READ_ONLY, 0, 0]
        );
    }

    /// A failed decode echoes the old value back in READ_WRITE with the
    /// status boolean still on (`watcher.hpp:2673-2677`).
    #[test]
    fn a_failed_decode_echoes_the_old_value() {
        fini();
        let value = Rc::new(RefCell::new(5i32));
        add_watcher_value("a/num", &value, Mode::ReadWrite, "");

        let mut payload = MemoryOStream::new();
        value_to_stream(
            &mut payload,
            &StreamValue::Str("not a number".to_string()),
            MODE_READ_WRITE,
        );

        let mut request = WatcherPathRequestV2::new("a/num");
        request.set_sequence_number(2);
        request.set_packet_data(payload.data().to_vec());
        assert!(request.set_watcher_value());
        assert_eq!(*value.borrow(), 5);

        let mut expected = MemoryOStream::new();
        expected.write_u32(2);
        value_to_stream(&mut expected, &StreamValue::Int(5), MODE_READ_WRITE);
        expected.write_bool(true);
        assert_eq!(request.data(), expected.data());
    }

    /// With writes blocked, sets are rejected before any watcher sees
    /// them, but the toggle itself stays writable so it can be flipped
    /// back over the wire (`watcher_path_request.cpp:62-83`).
    #[test]
    fn blocked_writes_reject_but_the_toggle_stays_writable() {
        fini();
        let value = Rc::new(RefCell::new(1i32));
        add_watcher_value("a/num", &value, Mode::ReadWrite, "");
        set_writes_enabled(false);

        let mut request = WatcherPathRequestV2::new("a/num");
        request.set_sequence_number(1);
        request.set_packet_data(vec![TYPE_INT, MODE_READ_WRITE, 4, 9, 0, 0, 0]);
        assert!(request.set_watcher_value());
        assert_eq!(*value.borrow(), 1);
        assert_eq!(
            request.data(),
            &[1, 0, 0, 0, TYPE_UNKNOWN, MODE_READ_ONLY, 0, 0]
        );

        let mut request = WatcherPathRequestV2::new(WRITES_TOGGLE_PATH);
        request.set_sequence_number(2);
        request.set_packet_data(vec![TYPE_BOOL, MODE_READ_WRITE, 1, 1]);
        assert!(request.set_watcher_value());
        assert!(request.completed());
        assert!(writes_enabled());
    }

    /// The `__doc__` pseudo path streams the node's comment read-only.
    #[test]
    fn doc_paths_stream_the_comment() {
        fini();
        let value = Rc::new(RefCell::new(1i32));
        add_watcher_value("a/num", &value, Mode::ReadOnly, "the answer");

        let mut request = WatcherPathRequestV2::new("a/num/__doc__");
        request.set_sequence_number(6);
        request.fetch_watcher_value();

        let mut expected = MemoryOStream::new();
        expected.write_u32(6);
        value_to_stream(
            &mut expected,
            &StreamValue::Str("the answer".to_string()),
            MODE_READ_ONLY,
        );
        assert_eq!(request.data(), expected.data());
    }

    /// A callable streams its expose hint as an INT and its argument
    /// descriptors as the nested tuple blob
    /// (`watcher.cpp:776-834`).
    #[test]
    fn callables_stream_args_and_expose() {
        fini();
        let mut cb = CallableWatcher::new(ExposeHint::WithSpace);
        cb.add_arg(DataType::Int, "count of things");
        cb.add_arg(DataType::String, "a label");
        add_watcher("cb", Rc::new(RefCell::new(cb)));

        let mut request = WatcherPathRequestV2::new("cb/__expose__");
        request.set_sequence_number(1);
        request.fetch_watcher_value();
        let mut expected = MemoryOStream::new();
        expected.write_u32(1);
        value_to_stream(&mut expected, &StreamValue::Int(2), MODE_READ_ONLY);
        assert_eq!(request.data(), expected.data());

        let mut request = WatcherPathRequestV2::new("cb/__args__");
        request.set_sequence_number(2);
        request.fetch_watcher_value();

        let mut expected = MemoryOStream::new();
        expected.write_u32(2);
        expected.write_u8(crate::stream::TYPE_TUPLE);
        expected.write_u8(MODE_CALLABLE);
        let mut tmp = MemoryOStream::new();
        tmp.write_string_length(2);
        for (data_type, description) in [
            (DataType::Int, "count of things"),
            (DataType::String, "a label"),
        ] {
            tmp.write_u8(crate::stream::TYPE_TUPLE);
            tmp.write_u8(MODE_READ_ONLY);
            let mut arg_stream = MemoryOStream::new();
            arg_stream.write_string_length(2);
            value_to_stream(
                &mut arg_stream,
                &StreamValue::Str(description.to_string()),
                MODE_READ_ONLY,
            );
            value_to_stream(
                &mut arg_stream,
                &StreamValue::DataType(data_type),
                MODE_READ_ONLY,
            );
            tmp.write_string_length(arg_stream.size() as i32);
            tmp.add_blob(arg_stream.data());
        }
        expected.write_string_length(tmp.size() as i32);
        expected.add_blob(tmp.data());
        assert_eq!(request.data(), expected.data());
    }

    /// The wire argument of a no-arg callable: an empty tuple.
    fn empty_tuple_payload() -> Vec<u8> {
        let mut payload = MemoryOStream::new();
        payload.write_u8(crate::stream::TYPE_TUPLE);
        payload.write_u8(MODE_READ_ONLY);
        let mut inner = MemoryOStream::new();
        inner.write_packed_int(0);
        payload.write_packed_int(inner.size() as i32);
        payload.add_blob(inner.data());
        payload.data().to_vec()
    }

    /// Calling a no-arg function answers the `(stdio output, return value)`
    /// result tuple plus the set status boolean
    /// (`watcher.cpp:857-943`).
    #[test]
    fn calling_a_no_arg_function_answers_the_result_tuple() {
        fini();
        let cb = NoArgFuncCallableWatcher::new(
            Box::new(|output: &mut String, value: &mut String| {
                output.push_str("stdout line");
                value.push_str("42");
                true
            }),
            ExposeHint::LocalOnly,
        );
        add_watcher("cb", Rc::new(RefCell::new(cb)));

        let mut request = WatcherPathRequestV2::new("cb");
        request.set_sequence_number(5);
        assert!(request.set_packet_data(empty_tuple_payload()));
        assert!(request.set_watcher_value());
        assert!(request.completed());

        let mut expected = MemoryOStream::new();
        expected.write_u32(5);
        expected.write_u8(crate::stream::TYPE_TUPLE);
        expected.write_u8(MODE_READ_ONLY);
        // `CallableWatcher::startResultStream`: packed size of the whole
        // tuple, then the count, then both elements.
        const TUPLE_ITEM_COUNT: i32 = 2;
        let output = "stdout line";
        let value = "42";
        let tuple_element = |len: usize| 2 + calculate_packed_int_size(len as i32) + len;
        let result_size = calculate_packed_int_size(TUPLE_ITEM_COUNT)
            + tuple_element(output.len())
            + tuple_element(value.len());
        expected.write_packed_int(result_size as i32);
        expected.write_packed_int(TUPLE_ITEM_COUNT);
        value_to_stream(
            &mut expected,
            &StreamValue::Str(output.to_string()),
            MODE_READ_ONLY,
        );
        value_to_stream(
            &mut expected,
            &StreamValue::Str(value.to_string()),
            MODE_READ_ONLY,
        );
        expected.write_bool(true);
        assert_eq!(request.data(), expected.data());
    }

    /// A function that reports failure answers the default failed-set
    /// result.
    #[test]
    fn a_failing_function_call_answers_the_default_result() {
        fini();
        let cb = NoArgFuncCallableWatcher::new(
            Box::new(|_: &mut String, _: &mut String| false),
            ExposeHint::LocalOnly,
        );
        add_watcher("cb", Rc::new(RefCell::new(cb)));

        let mut request = WatcherPathRequestV2::new("cb");
        request.set_sequence_number(5);
        request.set_packet_data(empty_tuple_payload());
        assert!(request.set_watcher_value());
        assert_eq!(
            request.data(),
            &[5, 0, 0, 0, TYPE_UNKNOWN, MODE_READ_ONLY, 0, 0]
        );
    }

    /// PyCommon before 2.6 sent `""` instead of an empty tuple; the
    /// reference still calls the function (`watcher.cpp:896-914`).
    #[test]
    fn legacy_empty_string_arguments_still_call() {
        fini();
        let cb = NoArgFuncCallableWatcher::new(
            Box::new(|_: &mut String, value: &mut String| {
                value.push('7');
                true
            }),
            ExposeHint::LocalOnly,
        );
        add_watcher("cb", Rc::new(RefCell::new(cb)));

        let mut payload = MemoryOStream::new();
        value_to_stream(
            &mut payload,
            &StreamValue::Str(String::new()),
            MODE_READ_ONLY,
        );
        let mut request = WatcherPathRequestV2::new("cb");
        request.set_sequence_number(1);
        request.set_packet_data(payload.data().to_vec());
        assert!(request.set_watcher_value());
        assert!(request.completed());
        // The result tuple's second element is the return value "7".
        assert!(request.data().windows(1).any(|w| w[0] == b'7'));
    }

    /// The V1 scalar reply keeps the reference's swapped argument order:
    /// the value slot carries the comment (`watcher_path_request.cpp:449-483`).
    #[test]
    fn v1_fetches_a_leaf_with_the_reference_argument_swap() {
        fini();
        let value = Rc::new(RefCell::new(11i32));
        add_watcher_value("a/num", &value, Mode::ReadOnly, "the comment");

        let mut request = WatcherPathRequestV1::new("a/num");
        request.fetch_watcher_value();
        assert!(request.completed());
        assert_eq!(request.last_replies(), 1);
        assert_eq!(request.data(), b"a/num\0the comment\0");

        let mut request = WatcherPathRequestV1::new("a/num");
        request.set_use_description(true);
        request.fetch_watcher_value();
        assert_eq!(request.data(), b"a/num\0the comment\x0011\0");
    }

    /// V1 collects one NUL-terminated entry per directory child and
    /// reports the child count; sets read back through the same fetch.
    #[test]
    fn v1_walks_directories_and_sets_values() {
        fini();
        let x = Rc::new(RefCell::new(1i32));
        let s = Rc::new(RefCell::new("two".to_string()));
        add_watcher_value("d/x", &x, Mode::ReadWrite, "");
        add_watcher_value("d/s", &s, Mode::ReadWrite, "");

        let mut request = WatcherPathRequestV1::new("d");
        request.fetch_watcher_value();
        assert_eq!(request.last_replies(), 2);
        assert_eq!(request.data(), b"d/s\0two\0d/x\x001\0");

        let mut request = WatcherPathRequestV1::new("d/x");
        request.set_value_data("9");
        assert!(request.set_watcher_value());
        assert_eq!(*x.borrow(), 9);
        // The readback carries the (empty) comment in the value slot.
        assert_eq!(request.data(), b"d/x\0\0");

        let mut request = WatcherPathRequestV1::new("nope");
        request.set_value_data("9");
        assert!(!request.set_watcher_value());
        assert_eq!(request.last_replies(), 0);
        assert_eq!(request.data_size(), 0);
    }

    /// V1 honours the write permission the same way
    /// (`watcher_path_request.cpp:410-443`).
    #[test]
    fn v1_set_respects_blocked_writes() {
        fini();
        let value = Rc::new(RefCell::new(1i32));
        add_watcher_value("a/num", &value, Mode::ReadWrite, "");
        set_writes_enabled(false);

        let mut request = WatcherPathRequestV1::new("a/num");
        request.set_value_data("9");
        assert!(!request.set_watcher_value());
        assert_eq!(*value.borrow(), 1);
        assert_eq!(request.last_replies(), 0);
    }

    /// Accessors and the degenerate result arms.
    #[test]
    fn request_accessors_and_degenerate_results() {
        let mut request = WatcherPathRequestV2::new("x");
        assert_eq!(request.path(), "x");
        assert_eq!(request.data_size(), 0);
        // Directory and Invalid answers are ignored outside a visit.
        request.set_result_value("", Mode::Invalid);
        request.set_result_value("", Mode::Directory);
        assert_eq!(request.data_size(), 0);
        // The raw counter write.
        request.add_watcher_count(2);
        assert_eq!(request.data_size(), 4);

        let request = WatcherPathRequestV1::new("y");
        assert_eq!(request.path(), "y");
    }

    /// Fetching the root answers each child under its bare label, in
    /// both protocols.
    #[test]
    fn root_fetches_use_bare_labels() {
        fini();
        let x = Rc::new(RefCell::new(3i32));
        add_watcher_value("x", &x, Mode::ReadOnly, "");

        let mut request = WatcherPathRequestV2::new("");
        request.set_sequence_number(1);
        request.fetch_watcher_value();
        let mut expected = MemoryOStream::new();
        expected.write_u32(1);
        value_to_stream(
            &mut expected,
            &StreamValue::Str("<DIR>".to_string()),
            MODE_DIRECTORY,
        );
        expected.write_i32(1);
        value_to_stream(&mut expected, &StreamValue::Int(3), MODE_READ_ONLY);
        expected.write_string("x");
        assert_eq!(request.data(), expected.data());

        let mut request = WatcherPathRequestV1::new("");
        request.set_use_description(true);
        request.fetch_watcher_value();
        assert_eq!(request.last_replies(), 1);
        assert_eq!(request.data(), b"x\x003\x00\x00");
    }

    /// A V1 fetch of a missing path still completes, with nothing.
    #[test]
    fn a_v1_fetch_of_a_missing_path_completes_with_nothing() {
        fini();
        let mut request = WatcherPathRequestV1::new("gone");
        request.fetch_watcher_value();
        assert!(request.completed());
        assert_eq!(request.last_replies(), 0);
        assert_eq!(request.data_size(), 0);
    }

    /// A watcher serving only the string protocol falls back to the
    /// trait's stream defaults, and a directory walk reports the failure
    /// triple with the label.
    #[test]
    fn stream_defaults_refuse_string_only_watchers() {
        fini();
        struct BareWatcher;
        impl crate::Watcher for BareWatcher {
            fn get_as_string(
                &self,
                _path: &str,
                _result: &mut String,
                _desc: &mut String,
                _mode: &mut Mode,
            ) -> bool {
                false
            }
            fn set_from_string(&mut self, _path: &str, _value: &str) -> bool {
                false
            }
            fn comment(&self) -> &str {
                "bare"
            }
            fn set_comment(&mut self, _comment: String) {}
        }
        add_watcher("dir/bare", Rc::new(RefCell::new(BareWatcher)));

        // The walk emits the failure triple plus the label.
        let mut request = WatcherPathRequestV2::new("dir");
        request.set_sequence_number(1);
        request.fetch_watcher_value();
        let mut expected = MemoryOStream::new();
        expected.write_u32(1);
        value_to_stream(
            &mut expected,
            &StreamValue::Str("<DIR>".to_string()),
            MODE_DIRECTORY,
        );
        expected.write_i32(1);
        expected.write_u8(TYPE_UNKNOWN);
        expected.write_u8(MODE_READ_ONLY);
        expected.write_string_length(0);
        expected.write_string("bare");
        assert_eq!(request.data(), expected.data());

        // A set through the bare watcher fails into the default result.
        let mut request = WatcherPathRequestV2::new("dir/bare");
        request.set_sequence_number(2);
        request.set_packet_data(vec![TYPE_INT, MODE_READ_WRITE, 4, 1, 0, 0, 0]);
        assert!(request.set_watcher_value());
        assert_eq!(
            request.data(),
            &[2, 0, 0, 0, TYPE_UNKNOWN, MODE_READ_ONLY, 0, 0]
        );
    }

    /// The writes toggle answers every protocol: the stream fetch, the
    /// string read/write, and the V1 set.
    #[test]
    fn the_toggle_answers_every_protocol() {
        fini();
        add_watchers();
        set_writes_enabled(false);

        // V2 fetch: the flag as a READ_WRITE BOOL stream.
        let mut request = WatcherPathRequestV2::new(WRITES_TOGGLE_PATH);
        request.set_sequence_number(1);
        request.fetch_watcher_value();
        let mut expected = MemoryOStream::new();
        expected.write_u32(1);
        value_to_stream(&mut expected, &StreamValue::Bool(false), MODE_READ_WRITE);
        assert_eq!(request.data(), expected.data());

        // The string read carries the flag plus the reference comment.
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        let value = get_watcher_string(WRITES_TOGGLE_PATH, &mut desc, &mut mode);
        assert_eq!(value.as_deref(), Some("false"));
        assert!(!desc.is_empty());
        assert_eq!(mode, Mode::ReadWrite);
        assert!(set_watcher_string(WRITES_TOGGLE_PATH, "true"));
        assert!(writes_enabled());

        // The V1 set drives the same leaf.
        let mut request = WatcherPathRequestV1::new(WRITES_TOGGLE_PATH);
        request.set_value_data("false");
        assert!(request.set_watcher_value());
        assert!(!writes_enabled());

        // No pseudo paths, and the comment accessor round-trips.
        let mut request = WatcherPathRequestV2::new(&format!("{WRITES_TOGGLE_PATH}/__doc__"));
        request.fetch_watcher_value();
        assert!(request.completed());
        let toggle = get_watcher(WRITES_TOGGLE_PATH).unwrap();
        assert!(!toggle.borrow().comment().is_empty());

        // The string protocol refuses deeper paths and junk values.
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert_eq!(
            get_watcher_string(&format!("{WRITES_TOGGLE_PATH}/x"), &mut desc, &mut mode),
            None
        );
        assert!(!set_watcher_string(
            &format!("{WRITES_TOGGLE_PATH}/x"),
            "true"
        ));
        assert!(!set_watcher_string(WRITES_TOGGLE_PATH, "junk"));

        // A stream set without a payload still answers the flag.
        let mut request = WatcherPathRequestV2::new(WRITES_TOGGLE_PATH);
        request.set_sequence_number(2);
        assert!(request.set_watcher_value());
        let mut expected = MemoryOStream::new();
        expected.write_u32(2);
        value_to_stream(&mut expected, &StreamValue::Bool(false), MODE_READ_WRITE);
        assert_eq!(request.data(), expected.data());
    }
}
