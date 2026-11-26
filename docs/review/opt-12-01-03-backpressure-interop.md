# OPT-12-01-03 — Rerun backpressure and tool interoperability

Acceptance: **查询及数据记录符合契约** (queries and data recording
match contracts).

Each channel's shipped backpressure face is driven to (and past) its
edge, and the records that survive are queried back through the
shipped tool faces — the two sides of the row's contract, per channel,
in one integration test each.

## The log channel — `crates/message_logger/tests/query_backpressure_interop.rs`

The backpressure face is `UserSegmentWriter::is_full(budget)`: the
writer itself accepts unconditionally — refusing is the caller's
contract (the reference's component checks `isFull` before each
record). The walk offers 64 records to a 748-byte budget (34 packed +
4 args per record) and stops at the first refusal, which lands exactly
on the arithmetic edge (20 × 38 = 760 ≥ 748, 19 × 38 = 722 < 748 —
asserted, plus `is_full` answering true at the budget). Every landed
record reads back with its time, priority, source and category intact
and its args blob re-rendering the original message.

Tool interop: the records load into the shipped `QueryableLog`
(component, format string, category, lines) and `run_query` answers
with the real filter pipeline — time window, both walk directions
(backwards is exactly the mirror), severity mask, source mask,
category, include `frame (1[0-4])` and exclude `frame 12` — the
hand-computed survivor set `{frame 11, frame 13}` in both directions.
The typed error paths surface on bad input (`UnknownCategory`,
`UnknownComponentId`, `UnknownStringOffset`), an out-of-span window
answers empty rather than erroring, and an inverted window answers
empty in both directions. The result rows carry the component, pid,
username, mapped source name (`CPP`/`SCRIPT`), resolved category name
and severity through.

## The Watcher channel — `crates/watcher/tests/backpressure_query_contract.rs`

The backpressure face is the write-permission gate
(`set_writes_enabled`): with it off, a V2 protocol write completes as
the failed-set default (`TYPE_UNKNOWN`, `MODE_READ_ONLY`, empty
string, false — asserted byte for byte against the same writer
primitives) and the leaf is untouched, while engine-side writes
(`set_watcher_string`) keep landing — the gate backpressures the tool
boundary only. The toggle path `watcherWrites/allowed` stays writable
through the same protocol, so the gate flips back; the released write
lands and the fetch answers it.

Tool interop: protocol write ↔ engine read agreement (a V2 set lands
in the leaf's shared cell and the free-function read answers it),
protocol read ↔ engine state agreement (the fetch reply decodes to
exactly the leaf's value via `stream_to_value`), the directory fetch
answers the count-then-entries wire order (`<DIR>` marker, count 2,
entries after), and a fetch on an unresolvable path answers the
`TYPE_UNKNOWN` default. The replies rerun byte for byte.

## The Profiler channel — `crates/profiler/tests/backpressure_query_contract.rs`

The backpressure face is the entry budget: a profiler inited at
`4 × 336` (budget 4) records a six-entry frame and clamps — the
recording buffer stops at 4, and the kept set is the **prefix** (e0..e3
answer through `final_entry`, nothing beyond index 3 does, the
high-water marks 4), with the display pool clamping at its own budget.
The refused entries allocate nothing (asserted zero). The channel is
not wedged: the next frame records its own four entries, closes fully,
collapses two display pairs and advances the processed-frame number to
2. The kept sets and the refusal cost rerun identically.

## Section finding

The three backpressure faces are shaped differently and the contracts
line up with their shapes: the log channel's pressure is **storage
size** (the caller must honor `is_full`, or the writer happily writes
past any budget), the Watcher's is **permission** (the gate refuses
tool writes but never engine writes, and is self-unlocking through the
toggle path), and the Profiler's is **buffer capacity** (clamped by
the face itself, prefix-keep, self-recovering). In all three the
query faces keep answering the records that survived the pressure —
the acceptance's query/data-record contract holds at the edge.

## Gates

- `cargo test -p message_logger`: 160 lib + 2 + 2 integration passed,
  0 failed (2 new).
- `cargo test -p watcher`: 77 lib + 2 + 2 integration passed, 0 failed
  (2 new).
- `cargo test -p profiler`: 43 lib + 2 + 2 + 2 integration passed,
  0 failed (2 new).
- `cargo fmt -p {message_logger,watcher,profiler} -- --check`: clean.
- `cargo clippy -p {message_logger,watcher,profiler} --all-targets
  -- -D warnings`: no lint.
