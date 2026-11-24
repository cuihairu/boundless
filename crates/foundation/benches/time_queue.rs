//! Timer-queue (scheduler core) baselines (perf bench batch 1).
//!
//! `TimeQueueT` is the port of the reference's `TimeQueueT` (the
//! libstdc++-heap-ordered timer queue every dispatcher runs). The three
//! shapes here cover its main path: the registration burst, the tick
//! that fires a full queue, and the mixed add/cancel/process churn a
//! live dispatcher produces. All fixtures are deterministic: deadlines
//! come from index arithmetic, the handler closures only bump counters.

use std::cell::Cell;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use foundation::time_queue::{TimeQueue64, TimerHandler, TimerHandlerFn};

/// A shared handler that just counts fires.
fn counting_handler(fires: &Rc<Cell<u64>>) -> Rc<dyn TimerHandler<u64>> {
    let fires = Rc::clone(fires);
    Rc::new(TimerHandlerFn::new(
        move |_handle, _user| {
            fires.set(fires.get() + 1);
        },
        "bench",
    ))
}

/// Deadline for timer `i`: spread over the first 97 ticks so a
/// `process(97)` fires everything in a staggered heap order.
fn deadline(i: usize) -> u64 {
    (i % 97) as u64 + 1
}

fn bench_schedule_1000(c: &mut Criterion) {
    let fires = Rc::new(Cell::new(0));
    let handler = counting_handler(&fires);
    let mut group = c.benchmark_group("time_queue");
    group.throughput(Throughput::Elements(1000));
    group.bench_function("add_1000_timers", |bench| {
        bench.iter(|| {
            let queue = TimeQueue64::new();
            for i in 0..1000 {
                queue.add(deadline(i), 0, Rc::clone(&handler), i, "bench");
            }
            black_box(queue.size())
        })
    });
    group.finish();
}

fn bench_process_1000(c: &mut Criterion) {
    let fires = Rc::new(Cell::new(0));
    let handler = counting_handler(&fires);
    let mut group = c.benchmark_group("time_queue");
    group.throughput(Throughput::Elements(1000));
    // Building the queue is setup; the measured work is the one
    // `process` pass that fires all 1000 timers.
    group.bench_function("process_1000_due", |bench| {
        bench.iter_batched(
            || {
                let queue = TimeQueue64::new();
                for i in 0..1000 {
                    queue.add(deadline(i), 0, Rc::clone(&handler), i, "bench");
                }
                queue
            },
            |queue| {
                let fired = queue.process(97);
                black_box((queue, fired))
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_churn(c: &mut Criterion) {
    let fires = Rc::new(Cell::new(0));
    let handler = counting_handler(&fires);
    let mut group = c.benchmark_group("time_queue");
    group.throughput(Throughput::Elements(200));
    // The live shape: per tick, 100 new timers arrive, every second
    // pending timer is cancelled, and the pass fires what is due.
    group.bench_function("add_cancel_process_100", |bench| {
        bench.iter(|| {
            let queue = TimeQueue64::new();
            let mut handles = Vec::with_capacity(100);
            for i in 0..100 {
                handles.push(queue.add(deadline(i), 0, Rc::clone(&handler), i, "bench"));
            }
            for (i, handle) in handles.iter_mut().enumerate() {
                if i % 2 == 0 {
                    handle.cancel();
                }
            }
            let fired = queue.process(97);
            black_box((queue.size(), fired))
        })
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(100);
    targets = bench_schedule_1000, bench_process_1000, bench_churn
}
criterion_main!(benches);
