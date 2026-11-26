//! OPT-06-01-03 download-drain benchmark.
//!
//! A resource stampede queues many downloads and one bundle budget drains
//! the queue. The cost under comparison is the per-download staging in
//! `ServerDownloads::add_to_bundle`: the `d741450` shape built a fresh
//! `Vec<ServerChunk>` per download and extended the result with it — one
//! allocation and copy per download per call — the delivered shape streams
//! the chunks straight into the result vector. Both shapes are benched in
//! this one binary — `legacy_queue.rs` ports the `d741450` mechanics
//! verbatim — so the interleaved comparison needs no second tree. No wall
//! clock is claimed as a network round trip: the bench measures the queue
//! machinery, same as the tick bench.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use server_runtime::server_download::ServerDownloads;

#[path = "legacy_queue.rs"]
mod legacy;
use legacy::LegacyServerDownloads;

/// Queue `count` one-fragment downloads: a one-byte description and a
/// 16-byte payload, so every download finishes in the first drained bundle
/// (header 4 + fragment overhead 4 + payload 16 = 24 bytes each).
fn queued(count: usize) -> (ServerDownloads, LegacyServerDownloads) {
    let mut current = ServerDownloads::new();
    let mut old = LegacyServerDownloads::new();
    for _ in 0..count {
        let description = b"f".to_vec();
        let data = vec![7u8; 16];
        current.stream(description.clone(), data.clone(), u16::MAX);
        old.stream(description, data, u16::MAX);
    }
    (current, old)
}

fn bench_download_drain(c: &mut Criterion) {
    let mut group = c.benchmark_group("download_drain");
    for count in [256usize, 2048] {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_function(format!("drain_{count}/into_chunks"), |bench| {
            bench.iter_batched(
                || queued(count),
                |(mut current, _)| {
                    let result = current.add_to_bundle((count * 24) as i32);
                    black_box(result.chunks.len());
                    black_box(result.finished.len())
                },
                criterion::BatchSize::SmallInput,
            );
        });
        group.bench_function(format!("drain_{count}/per_download_vec"), |bench| {
            bench.iter_batched(
                || queued(count),
                |(_, mut old)| {
                    let result = old.add_to_bundle((count * 24) as i32);
                    black_box(result.chunks.len());
                    black_box(result.finished.len())
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_download_drain);
criterion_main!(benches);
