# OPT-13-01-02 — Cache modes and concurrent conversion

The acceptance: **cold/hot timings compared** (`比较冷/热耗时`).
13-01-01 fixed the asset baseline; this evaluation isolates the cache's
three operating modes and then runs the same work from four threads
sharing one cache directory.

## The sequential mode comparison

| Mode | ns/asset | Ledger |
| --- | --- | --- |
| No cache (both gates off) | **36.9 µs** | 0 reads, 0 stores |
| Cold (reads off, writes on) | **74.0 µs** | 48 stores |
| Hot (reads on) | **64.2 µs** | 48 hits, byte-exact restore |

- **Storing costs 2.01× a bare write** (74.0 vs 36.9 µs): the cold
  leg pays a temp copy into the cache root plus a rename into the
  hash bucket, on top of writing the output.
- **Hot is only 1.15× faster than cold** on the fixed 4 KiB
  passthrough assets. The store overhead and the cache-hit copy are
  near-equal file operations at this size; as with 13-01-01, the
  honest reading is that cache economics need a real (expensive)
  conversion to pay off. The mode costs, not the speedup, are the
  reusable numbers.
- **24 buckets** hold 48 hashed entries — the low byte of each hash
  distributes the assets across the hex buckets; both the sequential
  and the concurrent cache directories end with the same 24.

## The concurrent legs

Four worker threads, each with its own `ContentAddressableCache`
instance pointed at one cache directory (per-process unique temp
names keep the copies disjoint):

| Leg | Wall | Ledger |
| --- | --- | --- |
| Cold, four writers | **1.573 ms** | 48 stores, 0 write misses |
| Hot, four readers | **1.830 ms** | 48 hits, byte-exact restores |

- The hot wall is *slower* in this capture: the 48 concurrent reads
  contend on the page cache and the copy+rename path, while the cold
  leg writes its temp files into the same directory. On this tiny
  fixture throughput is dominated by filesystem scheduling, not the
  cache gate — the sequential per-asset numbers are the meaningful
  comparison.
- **Cross-instance ledger**: a fresh sequential cache instance serves
  all 48 assets written by the four concurrent instances, byte-exact
  (`cross_instance_serves 48`). Content addressing is shared across
  writers and threads; no instance-local state leaks into the cache.

## Harness

`crates/asset-pipeline/tests/cache_concurrency_baseline.rs` — one
deterministic run, one temp root with separate output trees per leg,
fixed position-valued asset bytes. Every leg's count and byte
identity is asserted: the cache's `CacheEvents` seam records the
stores/hits through both a single-threaded `Cell` counter and an
`Arc<AtomicUsize>` counter for the concurrent legs.

```sh
CACHE_CONCURRENCY_OUT=<abs path> cargo test -p asset-pipeline \
    --test cache_concurrency_baseline -- --nocapture
```

## Fixtures

`docs/review/fixtures/cache-concurrency/cache-concurrency.tsv` — 16
rows, captured with the command above.
