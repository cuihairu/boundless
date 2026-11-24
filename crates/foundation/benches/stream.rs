//! Binary-stream codec baselines (perf bench batch 1).
//!
//! `BinaryStream` is the message serialization core every network and
//! DB path shares. Two shapes: encode a mixed payload (packed ints,
//! strings, blobs at 64 B / 1 KiB / 4 KiB, scalar arrays) and decode it
//! back. Payload contents are index arithmetic — fixed inputs, so runs
//! compare; throughput is reported in bytes.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use foundation::stream::BinaryStream;

/// The encoded size of the fixture payload, for the throughput figure
/// (measured from the fixture, not arithmetic that can drift).
fn payload_bytes() -> u64 {
    encode_fixture().length() as u64
}

/// Builds the fixture payload. Every value derives from its index; the
/// strings are 24 ASCII characters, the blobs are the three sizes.
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

fn bench_encode_mixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("stream");
    group.throughput(Throughput::Bytes(payload_bytes()));
    group.bench_function("encode_mixed_payload", |bench| {
        bench.iter(|| black_box(encode_fixture()))
    });
    group.finish();
}

fn bench_decode_mixed(c: &mut Criterion) {
    let bytes = encode_fixture().as_bytes().to_vec();
    let mut group = c.benchmark_group("stream");
    group.throughput(Throughput::Bytes(payload_bytes()));
    group.bench_function("decode_mixed_payload", |bench| {
        bench.iter(|| {
            let mut stream = BinaryStream::from_slice(&bytes);
            let mut packed = 0u32;
            for _ in 0..64 {
                packed = packed.wrapping_add(stream.read_packed_int().unwrap());
            }
            for _ in 0..16 {
                black_box(stream.read_string().unwrap());
            }
            for _ in 0..3 {
                black_box(stream.read_blob().unwrap());
            }
            for _ in 0..32 {
                black_box(stream.read_i32().unwrap());
            }
            for _ in 0..32 {
                black_box(stream.read_f32().unwrap());
            }
            stream.finish();
            black_box(packed)
        })
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(100);
    targets = bench_encode_mixed, bench_decode_mixed
}
criterion_main!(benches);
