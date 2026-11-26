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

## CI

No CI wiring yet (per batch ticket): benches stay out of the test gate,
`cargo test --workspace` is unaffected. When a perf gate lands, compare
against `target/criterion` baselines via `--save-baseline` /
`--baseline`, not absolute numbers from this page.
