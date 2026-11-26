# OPT-23-01-03 — Collection integrity regression

The acceptance: **the expected sequence, cancellation and overflow
statistics are correct** (`预期序列、取消及溢出统计正确`).

## Sequence

A mixed workload from two threads replays field-exact from the final
buffer, in submission order: worker `Start(7)`/`End(0)` (thread slot 1),
main-thread `fps` counter (60), `StartIdle`/`End` on `idle::wait`,
`Start`/`End` on `render` (slot 0). All 7 entries match name, event
kind, value, thread slot and recording frame number (7 × 5 field
checks); no eighth entry exists.

## Cancellation and deferred removal

- `enable(false)` mid-frame discards in-flight data: the recording
  buffer drops from 3 to 0 immediately, and the following tick
  processes an empty frame (final count 0) — the port's documented
  "ignore all current data" face.
- Re-enable + tick collects fresh: the next frame records exactly the
  single `fresh` counter; the torn frame never leaks.
- A `remove_thread` request while active defers: the worker slot still
  names `WorkerThread` at request time, frees only after the next
  frame's processing, and the thread count drops 2 → 1.

## Overflow statistics

| Face | Measurement |
| --- | --- |
| Entry budget 4, six submissions | final 4 (`e0..e3`), 2 clamped, watermark pins at 4, slot 4 absent |
| Display budget 2, three nested starts | display count 2 + the reference `ERROR: Too many display entries` string |
| Stack depth 65 nested sections | 64 kept, the 65th skipped, display chain length exactly 64 |

The three faces need separate profilers: the 4-slot entry budget
would clamp a 130-entry stack workload before the stack overflow
could be observed.

## Harness and fixtures

`crates/profiler/tests/collection_integrity_regression.rs` — one test,
four fresh local profilers (no global state).

```sh
COLLECTION_REGRESSION_OUT=<abs path> cargo test -p profiler \
    --test collection_integrity_regression -- --nocapture
```

`docs/review/fixtures/collection-regression/collection-regression.tsv`
— 12 rows, captured with the command above.
