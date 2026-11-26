//! OPT-12-01-01 (the Watcher channel): the Watcher channel's cost,
//! recorded separately from the logging channel's and the Profiler's —
//! one leaf registered, per path, and one string read / one string
//! write / one child enumeration, per operation, through the shipped
//! faces (`add_watcher_value` + `get_watcher_string` +
//! `set_watcher_string` + `visit_children`).
//!
//! The per-operation wall-clock times and allocation counts are the
//! channel's baseline: times are probe data, printed for the review
//! doc, never asserted; the allocation ledger and the value contract
//! (every write lands, every read answers it, every enumeration is
//! label-sorted and complete) are asserted and rerun identically.
//!
//! ```sh
//! cargo test -p watcher --test channel_cost_baseline
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use watcher::{Mode, Watcher, add_watcher_value, get_watcher_string, root, set_watcher_string};

// ---------------------------------------------------------------------------
// The counting allocator (the OPT-01-01-01 pattern).
// ---------------------------------------------------------------------------

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
            BYTES.with(|b| b.set(b.get() + layout.size()));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations + bytes counted during the region, on the calling thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
}

fn tracked<T>(body: impl FnOnce() -> T) -> (T, Count) {
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|c| c.set(0));
    BYTES.with(|b| b.set(0));
    let value = body();
    TRACKING.with(|t| t.set(false));
    let count = Count {
        allocs: ALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
    };
    (value, count)
}

// ---------------------------------------------------------------------------
// The Watcher channel.
// ---------------------------------------------------------------------------

/// Leaves under one walk's prefix.
const LEAVES: usize = 16;

/// One operation's ledger row: the read cost, the write cost and the
/// enumeration cost, allocations counted per region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpLedger {
    read_ns: u128,
    read_allocs: usize,
    write_ns: u128,
    write_allocs: usize,
    visit_ns: u128,
    visit_allocs: usize,
    labels: usize,
}

/// One Watcher-channel walk: LEAVES mixed-type leaves registered under
/// `channel/<prefix>`, then every leaf read, written and re-read, and
/// the whole bench subtree enumerated, once per leaf.
fn channel_walk(prefix: &str) -> Vec<OpLedger> {
    // the values must outlive the registrations
    let mut ints: Vec<Rc<RefCell<i32>>> = Vec::with_capacity(8);
    let mut strings: Vec<Rc<RefCell<String>>> = Vec::with_capacity(4);
    let mut floats: Vec<Rc<RefCell<f64>>> = Vec::with_capacity(4);
    let base = format!("channel/{prefix}");
    for i in 0..LEAVES {
        let path = format!("{base}/leaf{i:02}");
        let comment = format!("bench leaf {i}");
        match i % 4 {
            0 | 1 => {
                let value = Rc::new(RefCell::new(i as i32 * 7));
                ints.push(Rc::clone(&value));
                assert!(
                    add_watcher_value(&path, &value, Mode::ReadWrite, &comment).is_some(),
                    "{path} registers"
                );
            }
            2 => {
                let value = Rc::new(RefCell::new(format!("str{i}")));
                strings.push(Rc::clone(&value));
                assert!(
                    add_watcher_value(&path, &value, Mode::ReadWrite, &comment).is_some(),
                    "{path} registers"
                );
            }
            _ => {
                let value = Rc::new(RefCell::new(i as f64 * 1.5));
                floats.push(Rc::clone(&value));
                assert!(
                    add_watcher_value(&path, &value, Mode::ReadWrite, &comment).is_some(),
                    "{path} registers"
                );
            }
        }
    }

    let mut ledgers = Vec::with_capacity(LEAVES);
    for i in 0..LEAVES {
        let path = format!("{base}/leaf{i:02}");

        // the read: the string protocol answers the leaf's value, and
        // the face's out-params carry the comment and the mode
        let started = std::time::Instant::now();
        let ((answer, desc, mode), read) = tracked(|| {
            let mut desc = String::new();
            let mut mode = Mode::ReadWrite;
            let answer = get_watcher_string(&path, &mut desc, &mut mode);
            (answer, desc, mode)
        });
        let read_ns = started.elapsed().as_nanos();
        assert_eq!(
            desc,
            format!("bench leaf {i}"),
            "{path}: the comment rides the read"
        );
        assert!(
            matches!(mode, Mode::ReadWrite),
            "{path}: the mode rides the read"
        );

        // the value contract: the read answers the registered value
        match i % 4 {
            0 | 1 => assert_eq!(
                answer.as_deref(),
                Some((i as i32 * 7).to_string()).as_deref(),
                "{path}: the int leaf answers its value"
            ),
            2 => assert_eq!(
                answer.as_deref(),
                Some(format!("str{i}").as_str()),
                "{path}: the string leaf answers its value"
            ),
            _ => assert!(answer.is_some(), "{path}: the float leaf answers"),
        }

        // the write: a new value lands, and a read answers it
        let started = std::time::Instant::now();
        let (written, write) = tracked(|| match i % 4 {
            0 | 1 => set_watcher_string(&path, &format!("{}", 100 + i as i32)),
            2 => set_watcher_string(&path, &format!("written{i}")),
            _ => set_watcher_string(&path, "2.5"),
        });
        let write_ns = started.elapsed().as_nanos();
        assert!(written, "{path}: the write lands");

        // the write is visible: the shared cell moved (the write-then-read
        // contract at the value level, not just the string level)
        match i % 4 {
            0 | 1 => assert_eq!(
                *ints[(i / 4) * 2 + (i % 4)].borrow(),
                100 + i as i32,
                "{path}: the write landed in the leaf's cell"
            ),
            _ => {}
        }

        // the enumeration: the bench subtree, label-sorted, complete
        let started = std::time::Instant::now();
        let (labels, visit) = tracked(|| {
            let mut labels = Vec::new();
            root().borrow().visit_children(&base, &mut |label, _| {
                labels.push(label.to_string());
            });
            labels
        });
        let visit_ns = started.elapsed().as_nanos();
        assert_eq!(labels.len(), LEAVES, "{path}: the enumeration is complete");

        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "{path}: the enumeration is label-sorted");

        ledgers.push(OpLedger {
            read_ns,
            read_allocs: read.allocs,
            write_ns,
            write_allocs: write.allocs,
            visit_ns,
            visit_allocs: visit.allocs,
            labels: labels.len(),
        });
    }
    ledgers
}

#[test]
fn the_watcher_channel_answers_per_operation() {
    let ledgers = channel_walk("walk1");
    assert_eq!(ledgers.len(), LEAVES);

    // the value contract: every write landed in the leaf (re-read the
    // whole bench subtree through the string protocol)
    for i in 0..LEAVES {
        let path = format!("channel/walk1/leaf{i:02}");
        let mut desc = String::new();
        let mut mode = Mode::ReadWrite;
        let answer = get_watcher_string(&path, &mut desc, &mut mode).expect("path resolves");
        match i % 4 {
            0 | 1 => assert_eq!(
                answer,
                (100 + i as i32).to_string(),
                "{path}: the write is readable"
            ),
            2 => assert_eq!(
                answer,
                format!("written{i}"),
                "{path}: the write is readable"
            ),
            _ => assert_eq!(answer, "2.5", "{path}: the write is readable"),
        }
    }

    // the cost contract: the ledger is flat per leaf type — leaves of
    // the same type cost the same allocations, so the channel has no
    // per-operation drift within a type
    for (index, ledger) in ledgers.iter().enumerate() {
        let same_type = (0..LEAVES).filter(|j| j % 4 == index % 4);
        for j in same_type {
            let other = &ledgers[j];
            assert_eq!(
                ledger.read_allocs, other.read_allocs,
                "leaf {index} vs {j}: the read cost is flat per type"
            );
            assert_eq!(
                ledger.write_allocs, other.write_allocs,
                "leaf {index} vs {j}: the write cost is flat per type"
            );
            assert_eq!(
                ledger.visit_allocs, other.visit_allocs,
                "leaf {index} vs {j}: the enumeration cost is flat per type"
            );
        }
        assert_eq!(ledger.labels, LEAVES);
    }

    for (index, ledger) in ledgers.iter().enumerate() {
        println!(
            "WATCHER-CHANNEL leaf={index} read_ns={} read_allocs={} write_ns={} write_allocs={} visit_ns={} visit_allocs={} labels={}",
            ledger.read_ns,
            ledger.read_allocs,
            ledger.write_ns,
            ledger.write_allocs,
            ledger.visit_ns,
            ledger.visit_allocs,
            ledger.labels
        );
    }
    let reads: usize = ledgers.iter().map(|l| l.read_allocs).sum();
    let writes: usize = ledgers.iter().map(|l| l.write_allocs).sum();
    let visits: usize = ledgers.iter().map(|l| l.visit_allocs).sum();
    println!(
        "WATCHER-CHANNEL-TOTAL leaves={LEAVES} read_allocs={reads} write_allocs={writes} visit_allocs={visits}"
    );
}

#[test]
fn the_watcher_channel_reruns_to_the_same_ledger() {
    let first = channel_walk("rerun-a");
    let second = channel_walk("rerun-b");
    assert_eq!(first.len(), second.len());
    for (index, (a, b)) in first.iter().zip(second.iter()).enumerate() {
        assert_eq!(
            a.read_allocs, b.read_allocs,
            "leaf {index}: the read ledger reruns"
        );
        assert_eq!(
            a.write_allocs, b.write_allocs,
            "leaf {index}: the write ledger reruns"
        );
        assert_eq!(
            a.visit_allocs, b.visit_allocs,
            "leaf {index}: the enumeration ledger reruns"
        );
        assert_eq!(a.labels, b.labels, "leaf {index}: the enumeration reruns");
    }
    println!(
        "WATCHER-RERUN leaves={LEAVES} read_allocs={} write_allocs={} visit_allocs={} per_op",
        first[0].read_allocs, first[0].write_allocs, first[0].visit_allocs
    );
}
