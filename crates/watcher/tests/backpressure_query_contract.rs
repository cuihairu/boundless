//! OPT-12-01-03: rerun backpressure and tool interoperability —
//! the Watcher channel's side of the query/data-record contract.
//!
//! The tool drives the tree through the shipped V2 path-request
//! protocol while the engine side answers through the shipped free
//! functions, over one shared tree: a protocol write lands where the
//! free-function read answers it, a protocol fetch decodes to exactly
//! the leaf's value, and a directory fetch answers the
//! count-then-entries wire order. The channel's backpressure is the
//! shipped write-permission gate (`set_writes_enabled`): with it off,
//! a protocol write completes as a failed set (the `TYPE_UNKNOWN`
//! default reply, leaf untouched) while engine-side writes keep
//! landing, and the toggle path itself stays writable so the gate can
//! be flipped back through the same protocol.
//!
//! ```sh
//! cargo test -p watcher --test backpressure_query_contract
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use watcher::{
    MODE_READ_ONLY, MemoryIStream, MemoryOStream, Mode, StreamValue, TYPE_UNKNOWN,
    WRITES_TOGGLE_PATH, WatcherPathRequestV2, add_watcher_value, get_watcher_string, mode_byte,
    set_watcher_string, set_writes_enabled, stream_to_value, value_to_stream, writes_enabled,
};

/// One walk's tree: an int leaf and a string leaf under a unique
/// prefix (each test thread owns its root, but the prefixes keep the
/// reports readable).
struct Tree {
    int_cell: Rc<RefCell<i32>>,
    str_cell: Rc<RefCell<String>>,
    prefix: String,
}

impl Tree {
    fn register(prefix: &str) -> Self {
        let int_cell = Rc::new(RefCell::new(7));
        let str_cell = Rc::new(RefCell::new("written".to_string()));
        assert!(
            add_watcher_value(
                &format!("{prefix}/int"),
                &int_cell,
                Mode::ReadWrite,
                "the int leaf",
            )
            .is_some(),
            "{prefix}/int registers"
        );
        assert!(
            add_watcher_value(
                &format!("{prefix}/str"),
                &str_cell,
                Mode::ReadWrite,
                "the string leaf",
            )
            .is_some(),
            "{prefix}/str registers"
        );
        Self {
            int_cell,
            str_cell,
            prefix: prefix.to_string(),
        }
    }

    /// A protocol write: the V2 request with a `value_to_stream`
    /// payload at `path`.
    fn protocol_set(path: &str, value: &StreamValue) -> WatcherPathRequestV2 {
        let mut payload = MemoryOStream::new();
        value_to_stream(&mut payload, value, mode_byte(Mode::ReadWrite));
        let mut request = WatcherPathRequestV2::new(path);
        assert!(
            request.set_packet_data(payload.data().to_vec()),
            "payload attaches"
        );
        assert!(
            request.set_watcher_value(),
            "the set answers true (reference)"
        );
        request
    }

    /// A protocol read: the V2 fetch, whose reply decodes from the head
    /// of the result stream.
    fn protocol_fetch(path: &str) -> WatcherPathRequestV2 {
        let mut request = WatcherPathRequestV2::new(path);
        request.fetch_watcher_value();
        request
    }
}

/// The failed-set default reply the reference fills
/// (`completeFailedSet`): TYPE_UNKNOWN, MODE_READ_ONLY, an empty
/// string, false.
fn failed_set_reply() -> Vec<u8> {
    let mut out = MemoryOStream::new();
    out.write_u8(TYPE_UNKNOWN);
    out.write_u8(MODE_READ_ONLY);
    out.write_string_length(0);
    out.write_bool(false);
    out.data().to_vec()
}

#[test]
fn the_write_gate_backpressures_the_protocol_and_the_toggle_flips_back() {
    assert!(writes_enabled(), "the gate starts open");
    let tree = Tree::register("bp");

    // tool write, engine read: the protocol write lands in the leaf and
    // the free-function read answers it
    let request = Tree::protocol_set(&format!("{}/int", tree.prefix), &StreamValue::Int(4242));
    assert!(request.completed(), "the set completed");
    assert_eq!(*tree.int_cell.borrow(), 4242, "the protocol write landed");
    let mut desc = String::new();
    let mut mode = Mode::ReadWrite;
    assert_eq!(
        get_watcher_string(&format!("{}/int", tree.prefix), &mut desc, &mut mode),
        Some("4242".to_string()),
        "the engine read answers the protocol write"
    );

    // tool read, engine state: the fetch reply decodes to the leaf's
    // value
    let fetch = Tree::protocol_fetch(&format!("{}/int", tree.prefix));
    let answer = stream_to_value::<i32>(&mut MemoryIStream::new(fetch.data()));
    assert_eq!(answer, Some(4242), "the fetch decodes to the leaf's value");

    // the directory query: the `<DIR>` marker leads, then the
    // count-then-entries wire order, both leaves counted
    let fetch = Tree::protocol_fetch(&tree.prefix);
    assert!(fetch.completed(), "the directory fetch completed");
    let mut reply = MemoryIStream::new(fetch.data());
    let _type_byte = reply.read_u8();
    let _mode_byte = reply.read_u8();
    assert_eq!(reply.read_string(), "<DIR>", "the directory marker leads");
    assert_eq!(reply.read_i32(), 2, "the count answers both leaves first");
    assert!(fetch.data_size() > 12, "the entries follow the count");

    // backpressure: the gate closes
    set_writes_enabled(false);
    assert!(!writes_enabled(), "the gate closed");

    // a protocol write completes as a failed set: the default reply,
    // leaf untouched
    let request = Tree::protocol_set(
        &format!("{}/str", tree.prefix),
        &StreamValue::Str("nope".into()),
    );
    assert_eq!(
        request.data(),
        failed_set_reply().as_slice(),
        "the blocked set answers the failed-set default"
    );
    assert_eq!(
        tree.str_cell.borrow().as_str(),
        "written",
        "the blocked write never landed"
    );

    // the engine side is not backpressured: the free-function write
    // still lands while the protocol refuses
    assert!(
        set_watcher_string(&format!("{}/str", tree.prefix), "engine-side"),
        "the engine write lands"
    );
    assert_eq!(
        tree.str_cell.borrow().as_str(),
        "engine-side",
        "the engine write is not gated"
    );

    // a fetch on a path that does not resolve answers the same default
    let fetch = Tree::protocol_fetch(&format!("{}/nope", tree.prefix));
    assert_eq!(
        &fetch.data()[..2],
        &[TYPE_UNKNOWN, MODE_READ_ONLY],
        "the unknown path answers the default"
    );

    // the toggle is exempt from its own gate: the protocol flips it back
    Tree::protocol_set(WRITES_TOGGLE_PATH, &StreamValue::Bool(true));
    assert!(
        writes_enabled(),
        "the toggle flipped back through the protocol"
    );

    // the gate released: a protocol write lands again, and the fetch
    // answers it
    let request = Tree::protocol_set(
        &format!("{}/str", tree.prefix),
        &StreamValue::Str("protocol-back".into()),
    );
    assert_ne!(
        request.data(),
        failed_set_reply().as_slice(),
        "the released set is not a failed set"
    );
    assert_eq!(
        tree.str_cell.borrow().as_str(),
        "protocol-back",
        "the released write landed"
    );
    let fetch = Tree::protocol_fetch(&format!("{}/str", tree.prefix));
    assert_eq!(
        stream_to_value::<String>(&mut MemoryIStream::new(fetch.data())),
        Some("protocol-back".to_string()),
        "the fetch answers the released write"
    );
    println!(
        "WATCHER-INTEROP prefix={} blocked_reply_bytes={} count_leads=true gate_flips_back=true",
        tree.prefix,
        failed_set_reply().len()
    );
}

#[test]
fn the_contract_reruns_to_the_same_replies() {
    assert!(writes_enabled(), "the gate starts open");
    let tree = Tree::register("rerun");

    // the same interop walk, byte for byte on the observable replies
    Tree::protocol_set(&format!("{}/int", tree.prefix), &StreamValue::Int(99));
    assert_eq!(*tree.int_cell.borrow(), 99);
    let fetch = Tree::protocol_fetch(&format!("{}/int", tree.prefix));
    assert_eq!(
        stream_to_value::<i32>(&mut MemoryIStream::new(fetch.data())),
        Some(99),
        "the fetch reruns"
    );

    set_writes_enabled(false);
    let request = Tree::protocol_set(
        &format!("{}/str", tree.prefix),
        &StreamValue::Str("no".into()),
    );
    assert_eq!(
        request.data(),
        failed_set_reply().as_slice(),
        "the blocked reply reruns byte for byte"
    );
    assert_eq!(
        tree.str_cell.borrow().as_str(),
        "written",
        "still untouched"
    );
    Tree::protocol_set(WRITES_TOGGLE_PATH, &StreamValue::Bool(true));
    assert!(writes_enabled(), "the toggle reruns");
    println!(
        "WATCHER-RERUN prefix={} replies_equal=true gate_flips_back=true",
        tree.prefix
    );
}
