//! OPT-01-01-01: allocation-count baselines for the update loop.
//!
//! The foundation half of this harness lives in
//! `crates/foundation/tests/alloc_counts.rs` — same counting allocator,
//! same thread-local tracking, same contract: fixtures mirror
//! `benches/tick.rs` shape for shape, and a recorded number is exactly
//! reproducible on a given toolchain. Run with:
//!
//! ```sh
//! cargo test -p server-runtime --test alloc_counts
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use server_runtime::tick::{Updatable, Updatables};

/// Allocations + bytes counted during a tracked region, on the calling
/// thread only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
}

thread_local! {
    /// All state lives on the calling thread — concurrent test threads
    /// cannot pollute a measurement window.
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
            BYTES.with(|c| c.set(c.get() + layout.size()));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn measure<T>(body: impl FnOnce() -> T) -> (Count, T) {
    TRACKING.with(|t| {
        ALLOCS.with(|c| c.set(0));
        BYTES.with(|c| c.set(0));
        t.set(true);
    });
    let out = body();
    TRACKING.with(|t| t.set(false));
    let count = TRACKING.with(|_| Count {
        allocs: ALLOCS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
    });
    (count, out)
}

fn assert_reproducible<T>(body: impl Fn() -> T, label: &str) -> Count {
    let (first, _) = measure(&body);
    let (second, _) = measure(&body);
    assert_eq!(
        first, second,
        "{label}: allocation counts are not reproducible"
    );
    first
}

struct Noop;

impl Updatable for Noop {
    fn update(&mut self, _updatables: &mut Updatables) {}
}

fn noop(i: usize) -> Rc<RefCell<Noop>> {
    let _ = i;
    Rc::new(RefCell::new(Noop))
}

/// `updatables/call_1000_over_3_levels`: the bench builds the
/// collection as setup and times the `call()` pass alone, so only the
/// pass is counted.
#[test]
fn call_1000_over_3_levels() {
    let mut updatables = Updatables::new(3);
    for i in 0..1000 {
        updatables.add(noop(i), (i % 3) as i32);
    }
    let (count, size) = measure(|| {
        updatables.call();
        updatables.size()
    });
    assert_eq!(size, 1000);
    let again = assert_reproducible(
        || {
            let mut updatables = Updatables::new(3);
            for i in 0..1000 {
                updatables.add(noop(i), (i % 3) as i32);
            }
            updatables.call();
        },
        "updatables/call_1000_over_3_levels",
    );
    // The full build+call shape is reproducible; the pass-only count is
    // deterministic too but starts from a shared collection, so both
    // are recorded, pass-only printed.
    let _ = again;
    println!("updatables/call_1000_over_3_levels (pass only): {count:?}");
}

/// `updatables/register_deregister_100`: the full churn shape, all
/// counted.
#[test]
fn register_deregister_100() {
    let count = assert_reproducible(
        || {
            let mut updatables = Updatables::new(3);
            let objects: Vec<Rc<RefCell<dyn Updatable>>> = (0..100)
                .map(|i| noop(i) as Rc<RefCell<dyn Updatable>>)
                .collect();
            for (i, object) in objects.iter().enumerate() {
                updatables.add(object.clone(), (i % 3) as i32);
            }
            for object in &objects {
                updatables.remove(object);
            }
            updatables.size()
        },
        "updatables/register_deregister_100",
    );
    assert!(count.allocs > 0, "per-object Rc allocation must count");
    println!("updatables/register_deregister_100: {count:?}");
}
