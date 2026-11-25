# Performance baseline — 2026-11-24 (perf bench batch 1)

First performance baseline for the workspace. The framework is
[criterion](https://github.com/bheisler/criterion.rs) (`harness = false`
`[[bench]]` targets in each crate's `benches/`); every benchmark below
runs **100 samples** after a 3 s warm-up, with **deterministic
fixtures** (inputs built from index arithmetic, no RNG), so numbers are
comparable across runs on the same machine.

Reproduce with:

```sh
cargo bench -p foundation -p server-runtime
```

Criterion writes its full reports (charts, estimates, raw samples)
under `target/criterion/`.

## Scope

Batch 1 covers the four hot surfaces named in the batch ticket:

| surface | crate | bench file |
|---|---|---|
| scheduler core (timer queue) | `foundation` | `benches/time_queue.rs` |
| math (matrix / vector / quaternion) | `foundation` | `benches/math.rs` |
| message serialization (binary stream) | `foundation` | `benches/stream.rs` |
| event loop (update dispatch) | `server-runtime` | `benches/tick.rs` |

The python-host Python↔Rust bridge is **not** benched yet: a bridge
bench needs a live libpython interpreter in the bench process, which is
deferred until the python-host embedding story settles.

All fixtures are no-I/O and allocation-only where the measured function
allocates; queue/stream construction is moved out of the timed region
(`iter_batched`) so the numbers are the work itself. No main-code logic
was touched — this batch added `benches/` files, criterion
dev-dependencies and `[[bench]]` sections only.

## Numbers

Machine: Linux dev box, shared workload; numbers are means over 100
samples with the sample standard deviation. Throughput is criterion's
(bytes or elements per second).

| benchmark | input scale | mean ± std dev | throughput |
|---|---|---|---|
| `matrix/multiply_4x4_f32` | one 4×4 f32 multiply | 16.1 ns ± 1.6 ns | 977 M elem/s |
| `matrix/apply_point_x256` | 256-point batched transform | 0.79 µs ± 0.07 µs | 316 M elem/s |
| `vector3/normalise_lerp_dot_x256` | 256 normalise+lerp+dot chains | 4.31 µs ± 0.33 µs | 61.6 M elem/s |
| `quaternion_multiply_slerp` | 64 multiply+slerp steps | 3.57 µs ± 0.35 µs | ~18 M step/s |
| `stream/encode_mixed_payload` | mixed payload, 5.8 KiB total | 6.67 µs ± 0.58 µs | 846 MiB/s |
| `stream/decode_mixed_payload` | same payload, read back | 1.33 µs ± 0.15 µs | 4.2 GiB/s |
| `time_queue/add_1000_timers` | 1000 staggered registrations | 73.3 µs ± 6.7 µs | 13.6 M add/s |
| `time_queue/process_1000_due` | one pass firing 1000 timers | 161.6 µs ± 63.7 µs | 6.3 M fire/s |
| `time_queue/add_cancel_process_100` | 100 add + 50 cancel + process | 12.7 µs ± 1.0 µs | 16.3 M op/s |
| `updatables/call_1000_over_3_levels` | one `call()` pass, 1000 objects / 3 levels | 53.3 µs ± 5.2 µs | 18.2 M dispatch/s |
| `updatables/register_deregister_100` | 100 adds + 100 removes | 36.0 µs ± 3.9 µs | 5.5 M op/s |

Reading notes:

- The scalar (non-`EXT_MATH`) 4×4 multiply lands at ~16 ns — the
  reference `Matrix::multiply` order preserved at ~60 Gflop/s-class
  throughput for the port.
- Stream decode is ~5× faster than encode for the same payload: encode
  pays the packed-int byte-selection and string allocation; decode is
  bounds-checked memmove-shaped reads.
- `process_1000_due` shows the widest variance (single-pass samples are
  sensitive to machine load); its slope CI is tight — re-run on an idle
  box before using it for regression calls.
- Per-dispatch cost of the update loop is ~53 ns per object pass,
  including the `Rc` clone, borrow and the mid-pass placement checks.

## Allocation counts (2026-11-25, OPT-01-01-01)

Measured by the counting-allocator harness in
`crates/foundation/tests/alloc_counts.rs` and
`crates/server-runtime/tests/alloc_counts.rs`: the same fixtures as the
timing table, shape for shape (where a bench moves setup out of the
timed region, the same split applies). Every test asserts that two runs
allocate identically, so each number below is exactly reproducible on a
given toolchain — debug profile, same machine as the timing table.
Bytes are the cumulative requested size, not peak live size.

| fixture | allocations | bytes allocated |
|---|---|---|
| `time_queue/add_1000_timers` | 1,010 | 112,440 |
| `time_queue/process_1000_due` | 0 | 0 |
| `time_queue/add_cancel_process_100` | 108 | 12,504 |
| `stream/encode_mixed_payload` | 29 | 26,563 |
| `stream/decode_mixed_payload` | 20 | 11,481 |
| `matrix/multiply_4x4_f32` | 0 | 0 |
| `matrix/apply_point_x256` | 0 | 0 |
| `vector3/normalise_lerp_dot_x256` | 0 | 0 |
| `quaternion_multiply_slerp` | 0 | 0 |
| `updatables/call_1000_over_3_levels` (call pass only) | 0 | 0 |
| `updatables/register_deregister_100` | 114 | 12,424 |

Reading notes:

- Registration costs ~1 allocation per timer (1,010 for 1,000 adds,
  ~111 B each); the firing pass allocates nothing.
- The whole math surface is allocation-free: the scalar 4×4 multiply,
  the batched transforms and the vector/quaternion chains never touch
  the heap.
- The update dispatch pass is allocation-free (`Rc` clones are refcount
  bumps), so a steady-state tick costs zero allocations; only
  register/deregister churn allocates, ~1 per object.
- Encode pays 29 allocations for its string and blob writes; decode
  pays 20 (16 owned strings + 3 blobs + stream state).

## Load peaks (2026-11-25, OPT-01-01-01)

Peak resident set of the load generator (the `bots` binary, debug
profile) driving each PR smoke scenario through the offline faces at
the harness low-load envelope (`LOAD_CLIENTS=4`), measured with
`/usr/bin/time -v` on the same machine against main `70a4672`:

| scenario | load metrics at the envelope | peak RSS |
|---|---|---|
| `steady-move` | `bots_online` 4 | 3,164 KiB |
| `login-hurricane` | `online_peak` 4, 4/4 admitted | 3,180 KiB |
| `dense-aoi` | `bots` 4, `max_visible` 3 | 3,148 KiB |

All three scenarios pass `ci/load.sh` (3/3 records). The offline faces
run the scenario in simulated time, so these peaks describe the
generator's own footprint, not a served topology; per-period RSS/CPU
sampling of a live run (`ResourceTracker`, BW-17-03-04) exists in
`crates/load` but is not yet wired into the driver report.

## OPT-01-01-03 (2026-11-26): hot-path allocation fixes

Two fixes, each pinned by a counting-allocator test that failed against
the old implementation first (TDD red phase = the "before" numbers).

`DataSectionCache` (`crates/resources`) relink machinery: the LRU chain
previously stored neighbour links as `Option<String>` path copies, so
every relink allocated 2-6 Strings. The chain now lives in a slot arena
(`Vec<CacheNode>`) with integer links and an intrusive free chain; `add`
pays its two Strings (map key + in-slot name copy) once, and the
steady-state relinks allocate nothing. Sections drop when their entry is
freed, like the reference deleting the list node. Per-operation counts
(`crates/resources/tests/data_section_cache_alloc.rs`):

| operation | before | after |
|---|---|---|
| `find` head hit | 0 | 0 |
| `find` middle hit | 6 / 36 B | 0 |
| `find` tail hit | 5 / 30 B | 0 |
| `find` miss | 0 | 0 |
| `purgeLRU` | 3 / 18 B | 0 |
| `remove` middle / tail | 3 / 18 B | 0 |
| `remove` head | 2 / 12 B | 0 |
| in-place re-add | 0 | 0 |
| `add` new name | 4 | 2 |

`BinaryStream::new()` now reserves the reference constructor's 64-byte
buffer (`MemoryOStream( int size = 64 )`,
`lib/cstdmf/memory_stream.hpp`), so a small write pays one allocation
instead of a growth sequence (61-byte write: 2 → 1 allocation, the
first allocation requesting exactly 64 bytes). The
`stream/encode_mixed_payload` fixture drops 29 → 26 allocations
(26,563 → 26,507 bytes).

Load-peak A/B (`bots` generator, `LOAD_CLIENTS=4`, `LOAD_DURATION=2`,
median of 3 runs, main `33e030a` ± the optimization commit):
steady-move 3,216 → 3,456 KiB, login-hurricane 3,284 → 3,152 KiB,
dense-aoi 3,248 → 3,168 KiB. Every delta sits inside the ±150-200 KiB
run spread, so the generator's peak RSS is unchanged in noise — the
expected outcome for allocation-count work on the offline faces.

## CI

No CI wiring yet for the benches (per batch ticket): they stay out of
the test gate. The allocation harness is a test, not a bench — it runs
as part of `cargo test -p foundation -p server-runtime` (and, since
OPT-01-01-03, `cargo test -p resources` for the cache hot path). When a
perf gate lands, compare timings against `target/criterion` baselines
via `--save-baseline` / `--baseline`, not absolute numbers from this
page.
