# OPT-12-01-02 — The observation gates' enabled/disabled deltas

The acceptance: **the enabled/disabled deltas reported** (`报告启用和
关闭时的差值`). The 01-01 baseline separated the three channels; this
eval measures each channel's own gate against those same faces.

## The deltas

| Channel gate | Enabled | Disabled | Saving |
| --- | --- | --- | --- |
| Profiler `add_entry` (`profiler::enable(false)`) | 235 ns | 146 ns | **1.61×** |
| Profiler `tick` @ 100 entries | 2 907 ns | 77 ns | **37.7×** |
| Watcher `EventHistoryStats::track_event` (`set_enabled(false)`) | 449 ns | 16 ns | **28.6×** |
| Log write (caller-side priority floor) | 213 ns | 13 ns | **16.0×** |

## What the deltas say

- **The profiler's event gate barely pays** (1.61×): with recording
  off an `add_entry` still takes the mutex and the activity check —
  146 ns of pure lock. The per-frame gate is where the money is: the
  disabled `tick` skips the buffer swap and the inline processing, and
  the frame's observation cost collapses from ~2.9 µs to ~77 ns
  (37.7×). Any further event-side saving needs the gate to move in
  front of the lock (the reference's shape keeps the lock).
- **The Watcher's event-stats gate is the cheapest to hold off**:
  449 ns enabled (a `format!` key plus three map accumulations) versus
  a 16 ns bool check — 28.6×. The collector starts disabled in the
  reference, so the off-state is the default shape, and enabling clears
  the tables for a clean measurement window.
- **The log's gate lives at the call site**: the port has no sink-side
  switch; the reference filters by debug-message priority before
  building the entry. With the floor above the message's priority the
  whole build-and-serialize disappears — 213 ns becomes a 13 ns
  compare (16×).

The gates report themselves: `profiler::watcher_enabled()` answers
false when recording is off, `EventHistoryStats::is_enabled()` mirrors
the toggle — OPT-12-01-03's tool-query compatibility rides those faces.

## Harness

`crates/server-runtime/tests/observation_gate_delta.rs` — the three
gates driven from outside their crates, one deterministic run, with the
disabled states asserted (`watcher_enabled` false, the stats collector
holding its counts, the priority floor rejecting).

```sh
OBS_GATE_OUT=<abs path> cargo test -p server-runtime \
    --test observation_gate_delta -- --nocapture
```

## Fixtures

`docs/review/fixtures/obs-gates/obs-gates.tsv` — 14 rows, captured with
the command above.
