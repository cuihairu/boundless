# OPT-23-01-01 — Collection off/on long-run baseline

The acceptance: **preserve CPU, RSS, tick tail and dropped-event
counts** (`留存 CPU、RSS、Tick 及丢事件数`). Two legs run the same
200-frame scripted workload (20 zero-width sections = 40 entries per
frame, 8000 submissions) over fresh local profilers, one with
collection explicitly disabled, one enabled through the
pending-activate tick.

## The preserved baseline

| Metric | Collection off | Collection on |
| --- | --- | --- |
| Total leg wall | 1.882 ms | 3.926 ms |
| Per-entry average | 235 ns | 491 ns |
| Per-tick average | 9.3 µs | 19.6 µs |
| Tick max / p99 | 32.2 / 26.6 µs | 45.2 / 40.9 µs |
| RSS before / after | 3216 / 3216 KiB | 3220 / 3220 KiB |
| Dropped events | **8000** | **0** |
| Final buffer / frame no. | 0 / 0 | 40 / 200 |
| High watermark | 0 | 40 (of 64 budget) |

**The collection overhead is ~2.1× on this workload** (tick ratio
2.10, entry ratio 2.09) — recording doubles the cost of a gated frame
at 40 entries, consistent with the 12-01-02 finding that the gates
close cheaply but open paths pay for buffer writes and inline
processing. The numbers are observations (machine load moves them);
the structural half is gated:

- Off: every submission drops (8000/8000), `profile_frame_number`
  never advances, the final buffer stays empty — the drop face
  (`add_entry` returns before the seam when `is_active` is false).
- On: the last frame records all 40 entries, all 200 frames process,
  the watermark pins at exactly 40 inside the 64-slot budget
  (`memory / (128*2 + 160/2)`) — nothing clamps.
- `watcher_enabled()` mirrors the state in both legs (the self-report
  face the watcher reads).
- RSS is flat across both legs: the double-buffered pool, display pool
  and per-thread history are pre-allocated by `init` — collection
  state does not move memory.

## Harness and fixtures

`crates/profiler/tests/long_stability_collection.rs` — one test, two
legs over fresh `Profiler::new()` instances (no global state), tick
timings sampled per iteration and reduced to avg/max/p99, `VmRSS` read
from `/proc/self/status` (recorded as `-1` when unavailable).

```sh
LONG_STABILITY_OUT=<abs path> cargo test -p profiler \
    --test long_stability_collection -- --nocapture
```

`docs/review/fixtures/long-stability/long-stability.tsv` — 31 rows,
captured with the command above.
