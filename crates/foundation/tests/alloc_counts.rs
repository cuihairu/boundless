//! OPT-01-01-01: allocation-count baselines for the foundation hot
//! surfaces.
//!
//! The timing baselines live in `benches/` (criterion measures time);
//! this harness measures the other half of the acceptance — how many
//! allocations the same fixtures make. A counting global allocator with
//! a thread-local tracking flag counts only the calling thread's
//! allocations, so concurrent test threads cannot pollute a count and
//! every number here is exactly reproducible on a given toolchain.
//!
//! The fixtures mirror `benches/{time_queue,stream,math}.rs` shape for
//! shape; where a bench moves setup out of the timed region
//! (`iter_batched`), the same split applies here so counts and timings
//! describe the same work. Run with:
//!
//! ```sh
//! cargo test -p foundation --test alloc_counts
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use foundation::math::{Matrix, Quaternion, Vector3};
use foundation::stream::BinaryStream;
use foundation::time_queue::{TimeQueue64, TimerHandler, TimerHandlerFn};

use std::rc::Rc;

/// Allocations + bytes counted during a tracked region, on the calling
/// thread only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Count {
    allocs: usize,
    bytes: usize,
}

thread_local! {
    /// const-init: the flag and counters never allocate, so turning
    /// tracking on inside the allocator cannot recurse. All three live
    /// on the calling thread — concurrent test threads cannot pollute a
    /// measurement window, because none of this state is shared.
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

/// Count the calling thread's allocations inside `body`. The bytes
/// figure is the cumulative requested size, not peak live size.
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

/// Two runs of a deterministic fixture must allocate identically — that
/// is what makes the recorded numbers a baseline rather than a sample.
fn assert_reproducible<T>(body: impl Fn() -> T, label: &str) -> Count {
    let (first, _) = measure(&body);
    let (second, _) = measure(&body);
    assert_eq!(
        first, second,
        "{label}: allocation counts are not reproducible"
    );
    first
}

// --- time_queue (benches/time_queue.rs shapes) ---------------------------

fn counting_handler() -> Rc<dyn TimerHandler<u64>> {
    Rc::new(TimerHandlerFn::new(
        move |_handle, _user| {},
        "alloc_counts",
    ))
}

fn deadline(i: usize) -> u64 {
    (i % 97) as u64 + 1
}

/// `time_queue/add_1000_timers`: the bench times queue build + 1000
/// registrations together, so the count covers the same span.
#[test]
fn add_1000_timers() {
    let handler = counting_handler();
    let count = assert_reproducible(
        || {
            let queue = TimeQueue64::new();
            for i in 0..1000 {
                queue.add(deadline(i), 0, Rc::clone(&handler), i, "alloc_counts");
            }
            queue.size()
        },
        "time_queue/add_1000_timers",
    );
    assert!(count.allocs > 0, "queue construction must allocate");
    println!("time_queue/add_1000_timers: {count:?}");
}

/// `time_queue/process_1000_due`: the bench builds the queue as setup
/// and times only the firing pass, so only the pass is counted.
#[test]
fn process_1000_due() {
    let handler = counting_handler();
    let queue = TimeQueue64::new();
    for i in 0..1000 {
        queue.add(deadline(i), 0, Rc::clone(&handler), i, "alloc_counts");
    }
    let (count, fired) = measure(|| queue.process(97));
    assert_eq!(fired, 1000, "the pass must fire every timer");
    println!("time_queue/process_1000_due: {count:?}");
}

/// `time_queue/add_cancel_process_100`: the full churn shape, all
/// counted.
#[test]
fn add_cancel_process_100() {
    let handler = counting_handler();
    let count = assert_reproducible(
        || {
            let queue = TimeQueue64::new();
            let mut handles = Vec::with_capacity(100);
            for i in 0..100 {
                handles.push(queue.add(deadline(i), 0, Rc::clone(&handler), i, "alloc_counts"));
            }
            for (i, handle) in handles.iter_mut().enumerate() {
                if i % 2 == 0 {
                    handle.cancel();
                }
            }
            let fired = queue.process(97);
            (queue.size(), fired)
        },
        "time_queue/add_cancel_process_100",
    );
    println!("time_queue/add_cancel_process_100: {count:?}");
}

// --- stream (benches/stream.rs shapes) -----------------------------------

/// The bench's mixed payload builder, verbatim.
fn encode_fixture() -> BinaryStream {
    let mut stream = BinaryStream::new();
    for i in 0..64u32 {
        stream.write_packed_int(i % 1000).unwrap();
    }
    for i in 0..16 {
        let text: String = (0..24)
            .map(|j| (b'a' + ((i * 7 + j * 3) % 26) as u8) as char)
            .collect();
        stream.write_string(&text).unwrap();
    }
    stream.write_blob(&[0xA5u8; 64]).unwrap();
    stream
        .write_blob(&(0..1024u32).map(|i| (i % 251) as u8).collect::<Vec<_>>())
        .unwrap();
    stream
        .write_blob(&(0..4096u32).map(|i| (i % 241) as u8).collect::<Vec<_>>())
        .unwrap();
    for i in 0..32i32 {
        stream.write_i32(i * 31 - 500);
    }
    for i in 0..32 {
        stream.write_f32(i as f32 * 0.25 - 4.0);
    }
    stream
}

/// `BinaryStream::new()` reserves the reference `MemoryOStream`'s
/// 64-byte inline buffer: small writes (up to 64 bytes) fit in the
/// first allocation, with no growth reallocations.
#[test]
fn new_stream_reserves_64_bytes() {
    let (count, stream) = measure(|| {
        let mut stream = BinaryStream::new();
        stream.write_blob(&[0xA5u8; 60]).unwrap();
        stream
    });
    assert_eq!(stream.length(), 61);
    assert_eq!(count.allocs, 1, "64-byte reserve covers a 60-byte write");
    assert_eq!(count.bytes, 64);
}

/// `stream/encode_mixed_payload`.
#[test]
fn encode_mixed_payload() {
    let count = assert_reproducible(encode_fixture, "stream/encode_mixed_payload");
    assert!(count.allocs > 0, "string/blob writes must allocate");
    println!("stream/encode_mixed_payload: {count:?}");
}

/// `stream/decode_mixed_payload`: bytes prepared as setup, only the
/// read-back counted.
#[test]
fn decode_mixed_payload() {
    let bytes = encode_fixture().as_bytes().to_vec();
    let count = assert_reproducible(
        || {
            let mut stream = BinaryStream::from_slice(&bytes);
            let mut packed = 0u32;
            for _ in 0..64 {
                packed = packed.wrapping_add(stream.read_packed_int().unwrap());
            }
            for _ in 0..16 {
                let text = stream.read_string().unwrap();
                assert_eq!(text.len(), 24);
            }
            for _ in 0..3 {
                let blob = stream.read_blob().unwrap();
                assert!(!blob.is_empty());
            }
            for _ in 0..32 {
                stream.read_i32().unwrap();
            }
            for _ in 0..32 {
                stream.read_f32().unwrap();
            }
            stream.finish();
            packed
        },
        "stream/decode_mixed_payload",
    );
    assert!(count.allocs > 0, "string/blob reads must allocate");
    println!("stream/decode_mixed_payload: {count:?}");
}

// --- math (benches/math.rs shapes) ---------------------------------------

fn fixture_matrix(seed: u32) -> Matrix {
    let mut m = Matrix::zero();
    for (row, line) in m.m.iter_mut().enumerate() {
        for (col, cell) in line.iter_mut().enumerate() {
            let v = ((seed + (row * 4 + col) as u32) % 19) as f32 - 9.0;
            *cell = if v == 0.0 { 2.5 } else { v };
        }
    }
    m
}

fn fixture_points() -> Vec<Vector3> {
    (0..256)
        .map(|i| {
            Vector3::new(
                (i % 13) as f32 + 0.5,
                (i % 7) as f32 - 1.0,
                (i % 11) as f32 + 0.25,
            )
        })
        .collect()
}

/// `matrix/multiply_4x4_f32`: setup outside, the multiply alone counted.
#[test]
fn multiply_4x4_f32() {
    let a = fixture_matrix(0);
    let b = fixture_matrix(7);
    let mut m = fixture_matrix(3);
    let (count, _) = measure(|| m.multiply(&a, &b));
    println!("matrix/multiply_4x4_f32: {count:?}");
    let again = assert_reproducible(
        || {
            let mut m = fixture_matrix(3);
            m.multiply(&a, &b);
        },
        "matrix/multiply_4x4_f32",
    );
    assert_eq!(count, again);
}

/// `matrix/apply_point_x256`.
#[test]
fn apply_point_x256() {
    let m = fixture_matrix(2);
    let points = fixture_points();
    let count = assert_reproducible(
        || {
            for point in &points {
                let _ = m.apply_point(point);
            }
        },
        "matrix/apply_point_x256",
    );
    println!("matrix/apply_point_x256: {count:?}");
}

/// `vector3/normalise_lerp_dot_x256`.
#[test]
fn normalise_lerp_dot_x256() {
    let points = fixture_points();
    let count = assert_reproducible(
        || {
            let mut acc = 0.0f32;
            for pair in points.windows(2) {
                let mut a = pair[0];
                a.normalise();
                let lerped = Vector3::lerp(&a, &pair[1], 0.37);
                acc += lerped.dot_product(&pair[1]);
            }
            acc
        },
        "vector3/normalise_lerp_dot_x256",
    );
    println!("vector3/normalise_lerp_dot_x256: {count:?}");
}

/// `quaternion_multiply_slerp`.
#[test]
fn quaternion_multiply_slerp() {
    let axis = Vector3::new(0.3, -0.5, 0.8);
    let q_a = Quaternion::from_angle_axis(0.42, &axis);
    let q_b = Quaternion::from_angle_axis(-1.1, &Vector3::new(0.0, 1.0, 0.0));
    let count = assert_reproducible(
        || {
            let mut acc = 0.0f32;
            let mut current = q_a;
            for i in 0..64 {
                current = Quaternion::multiply(&current, &q_b);
                let blended = Quaternion::slerp(&current, &q_b, (i % 8) as f32 / 8.0);
                acc += blended.dot_product(&current);
            }
            acc
        },
        "quaternion_multiply_slerp",
    );
    println!("quaternion_multiply_slerp: {count:?}");
}
