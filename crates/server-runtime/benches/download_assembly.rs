//! OPT-06-01-03 download assembly benchmark.
//!
//! The face under comparison is the completion of one streamed download:
//! the last fragment arrives, the collector joins the segments in arrival
//! order and reports `Complete`. The baseline joined through
//! `Vec::concat` (the full result buffer alive beside every segment); the
//! candidate pre-sizes and consumes the segments as it copies. Fragment
//! counts at two scales, fragments of 512 bytes each.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use server_runtime::data_download::{DataDownloads, DownloadEvent};

/// One fragment's payload size (bytes).
const FRAGMENT_BYTES: usize = 512;

/// Feed a header and every fragment but the last; the measured call feeds
/// the final fragment and runs the completion path.
fn queued(id: u16, fragments: usize) -> DataDownloads {
    let mut downloads = DataDownloads::default();
    let mut header = vec![(b"bench.txt".len()) as u8];
    header.extend_from_slice(b"bench.txt");
    downloads.resource_header(id, &header);
    let payload = vec![0u8; FRAGMENT_BYTES];
    for seq in 0..fragments - 1 {
        downloads.resource_fragment(id, seq as u8, 0, &payload);
    }
    downloads
}

fn bench_completion(c: &mut Criterion) {
    let mut group = c.benchmark_group("download_assembly");
    for fragments in [64usize, 512] {
        group.throughput(Throughput::Elements(fragments as u64));
        group.bench_function(format!("complete_{fragments}"), |bench| {
            bench.iter_batched(
                || queued(7, fragments),
                |mut downloads| {
                    let last = vec![0u8; FRAGMENT_BYTES];
                    match downloads.resource_fragment(7, (fragments - 1) as u8, 1, &last) {
                        Some(DownloadEvent::Complete(complete)) => black_box(complete.data.len()),
                        _ => black_box(0),
                    }
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_completion);
criterion_main!(benches);
