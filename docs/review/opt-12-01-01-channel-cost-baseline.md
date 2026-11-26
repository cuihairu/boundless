# OPT-12-01-01 — Record the observation channels' costs

Acceptance: **分离日志、Watcher 和 Profiler** (the log, Watcher and
Profiler channels are separated).

The three observation channels' costs are recorded in three separated
integration tests — one per channel crate, one process each, so no
test shares a global (the log segment dir, the Watcher root, the
Profiler instance) with another channel. Each walk drives only the
shipped faces and answers two kinds of numbers: **wall-clock
per operation** (probe data, printed for the record, never asserted —
this is a shared debug runner) and the **allocation ledger per
operation** (asserted flat and rerun-identical — the structural cost
identity).

## The log channel — `crates/message_logger/tests/channel_cost_baseline.rs`

64 messages (`"baseapp frame N"`, `%s frame %d` args) recorded into a
fresh user segment via `UserSegmentWriter::add_entry`, each read back
via `read_entry` and re-rendered via `stream_to_string`:

| Operation | allocs | wall-clock (debug) |
|---|---|---|
| record one entry | **4** flat | ~7–40 µs |
| read + render one entry | **13** flat | ~15–26 µs |

The record contract: every entry readable in write order, every read
re-renders the original message (asserted per entry), the segment ends
with 64 entries, and the ledger reruns identically (4 allocs/entry
record, 13/entry query).

## The Watcher channel — `crates/watcher/tests/channel_cost_baseline.rs`

16 mixed-type leaves (8 i32, 4 String, 4 f64) registered under
`channel/<prefix>` via `add_watcher_value`, then per leaf one
`get_watcher_string`, one `set_watcher_string`, one `visit_children`
enumeration of the bench subtree:

| Operation | allocs (int leaf) | allocs (String) | allocs (f64) | wall-clock (debug) |
|---|---|---|---|---|
| read | 2 | 2 | 5 | ~2–9 µs |
| write | 1 | 2 | 0 | ~2–6 µs |
| enumerate 16 leaves | 19 | 19 | 19 | ~4–6 µs |

The flatness contract is per leaf type (same type, same cost — a
String write re-allocates its leaf, an f64 write parses in place).
The value contract: every read answers the registered value with the
comment and mode riding the face's out-params, every write lands in
the leaf's shared cell (asserted at both the string level and the
value level), every enumeration is complete and label-sorted, and the
ledger reruns identically.

## The Profiler channel — `crates/profiler/tests/channel_cost_baseline.rs`

A `with_clock` profiler over a hand-stepped atomic (deterministic
stamps), one thread registered, the first tick activating recording,
then 64 frames × 8 Start/End event pairs + one tick per frame:

| Operation | allocs | wall-clock (debug) |
|---|---|---|
| add one Start/End entry | **0** | ~0.3–1.5 µs |
| tick (close one frame) | **0** | ~3–12 µs |

The record contract, per closed frame: `final_entry_count == 16`
(every event carried), `display_entry_count == 8` (every pair
collapsed), every `final_entry` answers its name, the recording buffer
swapped empty, the frame-end stamp exactly
`1_000 + frame·(8·100 + 1_000)` (the deterministic clock's schedule),
the high-watermark never exceeding one frame's entries, and the ledger
(durations included — they ride the stepped clock) reruns identically.

## Section finding

The channel separation is real, not just structural: the log channel
costs allocations per operation (it writes files), the Watcher channel
costs a few per operation at the string protocol (it builds answer
strings) but almost nothing at the value level, and the Profiler
channel costs zero — its buffers are preallocated at `init`. That
gradient (4/13 → 1–5 → 0) is the baseline OPT-12-01-02's
enable/disable deltas are measured against.

## One inherited lint fixed

`cargo clippy -p message_logger` failed on pre-existing lib debt
(`entry.rs:119`, empty line after doc comment on `PACKED_SIZE`) —
removed; the crate's own gates then ran clean.

## Gates

- `cargo test -p message_logger`: 160 lib + 2 new integration passed,
  0 failed.
- `cargo test -p watcher`: 77 lib + 2 new integration passed, 0 failed.
- `cargo test -p profiler`: 43 lib + 2 new integration passed, 0
  failed.
- `cargo fmt -p {message_logger,watcher,profiler} -- --check`: clean.
- `cargo clippy -p {message_logger,watcher,profiler} --all-targets
  -- -D warnings`: no lint.
