# OPT-23-01-02 — Collection buffering and export evaluation

The acceptance: **report the overhead change on the same scenario**
(`按同场景报告开销变化`). One scenario — 100 frames × 20 sections (40
entries/frame) — runs under three buffer budgets and, on the largest,
with the JSON dump live for its first 20 frames.

## Buffer budgets

| Budget | Slots (of 40 needed) | Tick avg | Dropped | Watermark |
| --- | --- | --- | --- | --- |
| generous | 256 | ~26 µs | 0 | 40 |
| exact | 40 | 16.6 µs | 0 | 40 |
| tight | 10 | 9.7 µs | **3000** (30/frame) | **10** |

- The exact budget (40 slots for 40 entries) records everything — the
  clamp at `startNextEntry` only fires when a submission arrives with
  the buffer full, so a frame that exactly fits pays nothing.
- The tight budget clamps every frame: the watermark pins at the
  ceiling, the final buffer holds exactly `slots` entries and 30 of
  40 submissions drop per frame — the drop face is visible in the
  counts, not guessed.
- Per-tick cost falls with the budget because processing scales with
  recorded entries; these are observations.

## Export overhead

The same scenario with `watcher_set_dump_profile(true)` and a count of
20: the first 20 frames render Chrome-trace JSON into a scratch
directory, the remaining 80 run dump-free. Splitting the per-tick
samples at the dump boundary (identical frames on both sides):

| Window | Tick avg | Tick p99 |
| --- | --- | --- |
| Dump frames 1–20 | 56.3 µs | 84.7 µs |
| Tail frames 21–100 | 20.1 µs | 37.4 µs |

**The export overhead is ~2.81× on this scenario** — 60,901 bytes of
trace JSON across 20 frames (~3 KB/frame), written inline in
`processEntries` before the display build.

Gated contracts: the state machine lands `Completed` and the dump flag
clears itself; the output path follows the pinned
`{exe}_profile_YYYYMMDD_HHMMSS.json` schema; the file opens with `[`
and the stop closes it with `]`. The failure face: an unwritable dump
path (a directory occupied by a file) flips the state to `Failed`
with an empty output path — the frame survives, matching the
reference's null-return handling.

## Harness and fixtures

`crates/profiler/tests/buffer_export_evaluation.rs` — three budget
legs and the export leg over fresh profilers, per-tick samples reduced
to avg/max/p99, plus the failure leg.

```sh
BUFFER_EXPORT_OUT=<abs path> cargo test -p profiler \
    --test buffer_export_evaluation -- --nocapture
```

`docs/review/fixtures/buffer-export/buffer-export.tsv` — 27 rows,
captured with the command above.
