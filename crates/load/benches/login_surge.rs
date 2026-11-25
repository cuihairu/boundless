//! OPT-06-01-02 login-surge benchmark.
//!
//! A surge parks many refused bots in the retry queue at once and then
//! re-admits them on one tick. The cost under comparison is the drain of
//! `botsForLogOnRetry_` — the old shape removed each due bot in its own
//! `Vec::remove` pass (O(n^2)), the new shape partitions in one pass (O(n)).
//! No wall clock is claimed as a login round trip: the bench measures the
//! dispatch machinery, same as the server-runtime tick bench.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use load::{FD_PER_BOT, Fleet};

/// Park `count` bots with staggered retry times, all due at the drain tick.
fn parked(count: usize) -> Fleet {
    let mut fleet = Fleet::new("0", 10_000 * FD_PER_BOT);
    for i in 0..count {
        fleet.add_bot();
        fleet.bots_mut()[i].feed_log_on_failure(Some((i + 1) as f64));
    }
    fleet.tick(0.0);
    fleet
}

fn bench_surge_drain(c: &mut Criterion) {
    let mut group = c.benchmark_group("login_surge");
    for count in [64usize, 512] {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_function(format!("drain_{count}"), |bench| {
            bench.iter_batched(
                || parked(count),
                |mut fleet| {
                    fleet.tick((count + 1) as f64);
                    black_box(fleet.len())
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_surge_drain);
criterion_main!(benches);
