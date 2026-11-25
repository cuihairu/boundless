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

## Network transport (2026-11-26, OPT-02-01-01)

The Mercury transport baseline: criterion benches in
`crates/mercury/benches/network.rs`, plus a deterministic loss-recovery
record in `crates/mercury/tests/loss_recovery_record.rs`. Two layers per
face: **protocol layer** (in-process, no kernel) isolates the Mercury
machinery's CPU cost, and **wire layer** (real loopback sockets) carries
syscall and scheduler cost on top.

These runs landed on a shared box at load average 35–58, and the
wall-clock faces swung ±20–60% between full runs; the table reports the
spread across runs (floor estimate first), not one criterion median.
The loss-recovery record is immune to all of this: it pins exact stamps
in simulated time.

| benchmark | layer | time (spread over runs) | throughput |
|---|---|---|---|
| `udp/protocol_rpc_roundtrip` | protocol | 1.1–2.3 µs per RPC | ~0.5–0.9 M RPC/s |
| `udp/fragment_64k_stream` | protocol | 6.9–12.8 µs per 64 KiB bundle | 5.1–9.5 GB/s payload |
| `udp/reparse_64k_stream` | protocol | 67–127 µs per 64 KiB bundle | 0.5–1.0 GB/s payload |
| `udp/wire_reliable_roundtrip_loopback` | wire | 27–34 µs per reliable message | — |
| `udp/wire_64k_transfer_loopback` | wire | 18.3–23.1 ms per 1024 messages | 2.8–3.6 MB/s payload |
| `tcp/frame_stream_in_memory` | protocol | 277–393 µs per 1 MiB framed stream | 2.7–3.8 GB/s |
| `tcp/wire_stream_loopback` | wire | 3.8–5.4 ms per 1 MiB | 194–276 MB/s |

Reading notes:

- The protocol RPC round trip is ~1 µs of CPU: request bundle build,
  footer, dispatch walk, echo reply, reply dispatch, handler. At the
  internal channel's seeded 0.1s RTT that cost is noise; it matters as
  the floor for what a served RPC costs the process.
- Fragmentation (pack 1024 messages into ~47 datagrams, 6.9–12.8 µs) is
  ~70× cheaper than the parse-back (67–127 µs): the dispatch walk pays
  per-message handler dispatch, a `BinaryStream` build, element clones
  and the per-element EMA bookkeeping, none of which the pack side
  touches.
- The wire 64 KiB transfer is per-datagram (one reliable message per
  bundle): a bundle whose messages straddle packets needs the
  bundle-processor reassembly face, which is not declared in `lib.rs`
  yet (it rides the tcp restore lane). Its cost is ~1024 send + 1024
  receive + 1024 ack send + 1024 ack receive round trips, ~18 µs per
  message — the same order as the single-message round trip, which is
  the consistency check.
- TCP wire throughput over loopback (~200–280 MB/s at this load) is a
  floor estimate; the in-memory face (2.7–3.8 GB/s) bounds the framing
  machinery's own cost. The frame split on the receive side is inline in
  the bench: the dedicated parser module is an orphan pending the tcp
  restore lane.

The loss-recovery record (simulated time, exact stamps, all three
tests green):

- Resend period is `max(2 × roundTripTime_, minInactivityResendDelay)`
  (`udp_channel.cpp:1113`), and the staleness test is strict: an unacked
  packet ages past the period, not to it — an age exactly equal to the
  period is not yet stale, the resend lands one microsecond past.
- Both arms verified: a 2s minimum delay sets a 2s period on an
  internal channel (seeded 0.1s RTT), and healthy 0.3s acks walking the
  EWMA (0.1 → 0.12 → 0.138) set a 0.276s period that outruns a 50ms
  minimum delay.
- A resent packet's ack never samples the RTT average
  (udp_channel.cpp:987): after recovery the average still reads
  0.138, and the window is fully drained.
- One `checkResendTimers` scan resends at most `windowSize / 8`
  (512 on the internal 4096 window); the next scan takes the remainder.
  Recovery of 600 stale packets therefore completes in two scans.

## CI

No CI wiring yet for the benches (per batch ticket): they stay out of
the test gate. The allocation harness is a test, not a bench — it runs
as part of `cargo test -p foundation -p server-runtime` (and, since
OPT-01-01-03, `cargo test -p resources` for the cache hot path). When a
perf gate lands, compare timings against `target/criterion` baselines
via `--save-baseline` / `--baseline`, not absolute numbers from this
page.
