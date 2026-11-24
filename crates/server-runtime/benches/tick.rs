//! Update-loop dispatch baselines (perf bench batch 1).
//!
//! `Updatables` is the port of the reference's level-partitioned update
//! collection — the per-tick dispatch every role app runs. Two shapes:
//! the full `call()` pass over 1000 registered objects spread across
//! three levels, and the register/deregister churn of a dynamic
//! population. The updatables are no-ops, so the numbers are the
//! dispatch machinery itself; fixtures are fixed.

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use server_runtime::tick::{Updatable, Updatables};

struct Noop;

impl Updatable for Noop {
    fn update(&mut self, _updatables: &mut Updatables) {}
}

fn noop(i: usize) -> Rc<RefCell<Noop>> {
    let _ = i;
    Rc::new(RefCell::new(Noop))
}

fn bench_call_1000(c: &mut Criterion) {
    let mut group = c.benchmark_group("updatables");
    group.throughput(Throughput::Elements(1000));
    group.bench_function("call_1000_over_3_levels", |bench| {
        bench.iter_batched(
            || {
                let mut updatables = Updatables::new(3);
                for i in 0..1000 {
                    updatables.add(noop(i), (i % 3) as i32);
                }
                updatables
            },
            |mut updatables| {
                updatables.call();
                black_box(updatables.size())
            },
            criterion::BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_register_churn(c: &mut Criterion) {
    let mut group = c.benchmark_group("updatables");
    group.throughput(Throughput::Elements(200));
    group.bench_function("register_deregister_100", |bench| {
        bench.iter(|| {
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
            black_box(updatables.size())
        })
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(100);
    targets = bench_call_1000, bench_register_churn
}
criterion_main!(benches);
