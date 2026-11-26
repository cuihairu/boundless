# OPT-12-01-02 — Lower the observation cost: the enabled/disabled delta

Acceptance: **报告启用和关闭时的差值** (the enabled/disabled delta is
reported).

`crates/profiler/tests/channel_gate_delta.rs` drives the OPT-12-01-01
frame shape (64 frames × 8 Start/End pairs + a tick, deterministic
clock) through the Profiler's runtime gate — the only enable/disable
face the three observation channels ship (`enable`, surveyed below) —
in halves: frames 1..=32 enabled, frames 33..=64 disabled, then a
re-arm leg.

## The measured delta (p50 wall-clock per operation, debug runner)

| Operation | enabled p50 | disabled p50 | delta | allocs on → off |
|---|---|---|---|---|
| add one entry (Start) | 400 ns | 312 ns | ~22% saved | 0 → 0 |
| add one entry (End) | 395 ns | 308 ns | ~22% saved | 0 → 0 |
| tick (close a frame) | **5,039 ns** | **312 ns** | **~16× saved** | 0 → 0 |

Wall-clock is probe data (printed `GATE-DELTA` lines, never asserted);
the structural claims are asserted: the disabled path never allocates
more than the enabled path, and the per-frame ledgers rerun
identically. The disabled `add_entry` still pays the lock + clock
stamp before the active check — the entry write itself (name copy,
buffer slot, 0-alloc fixed-pool) is what's skipped; the disabled
`tick` skips the whole frame processing (swap, hierarchy build,
display pool), which is where the 16× lives.

## The gate contract (asserted, live in the walk)

- **disabling drops what is buffered** — pairs recorded before
  `enable(false)` are gone from the recording buffer after it
  (`recording_entry_count == 0`);
- **the disabled half is a no-op** — every entry lands nowhere
  (asserted per entry), ticks close nothing, the display pool stays
  empty of new work, and the processed-frame number freezes at 32;
- **the closed-frame views freeze, not reset** — while disabled,
  `final_entry_count` / `display_entry_count` keep answering the last
  processed frame's values (16 / 8): disabled ticks perform no buffer
  swap, and the re-activation tick on the empty recording buffer swaps
  nothing either — the frozen view is visible until a new frame closes;
- **the gate re-arms through the same faces** — `enable(true)` +
  activation tick + one pair + one tick closes a 2-entry frame with 1
  display entry, processing resuming at the next frame number (33);
- `watcher_enabled()` answers the gate state at every frame
  (asserted).

## The other two channels (the survey half of the report)

The log and Watcher channels ship **no runtime gate face** — the
delta for them is nil by construction, and that finding is part of
this row's report:

- **message_logger**: `UserSegmentWriter` has no enable/disable; a
  segment is opened at init and written until full — the channel's
  OPT-12-01-01 cost (4 allocs/entry record, 13/query read) is
  unconditional. The registry's uid/logger-id/name filters gate
  *delivery to subscribers* (the tool side), not the recording cost.
- **watcher**: `add_watcher_value` has no off state; `Mode` gates
  writes (ReadOnly vs ReadWrite), not the observation cost — the
  OPT-12-01-01 costs (1–5 allocs per string op) are unconditional.

So the row's answer for the section: the shipped lowering mechanism is
the Profiler's gate, worth ~16× on its most expensive operation (the
frame-processing tick) and ~22% per recorded event; the other two
channels would need new gate faces to lower (out of this row's scope —
their costs are already the section's smallest).

## Gates

- `cargo test -p profiler`: 43 lib + 2 + 2 integration passed, 0
  failed (2 new: the gate-delta report, the rerun shape).
- `cargo fmt -p profiler -- --check`: clean.
- `cargo clippy -p profiler --all-targets -- -D warnings`: no lint.
