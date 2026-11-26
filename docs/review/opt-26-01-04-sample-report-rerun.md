# OPT-26-01-04 — sample report recomputation

The acceptance: **the windows and counts match the pre-optimization
report** (`窗口及计数与优化前一致`). This row replays the same fixed
event stream as OPT-26-01-01, recomputes the sample report from the
*stored* corpus, and compares it against the committed baseline
fixture — the four level windows and the five query-shape counts,
read from `docs/review/fixtures/event-query/event-query.tsv` on disk
rather than re-derived from shared constants. Then the same report is
recomputed after each optimization the section evaluated: the 64-row
commit batching (OPT-26-01-02) and the aggregated name/level indexes
(OPT-26-01-03).

## Design

`crates/db-postgres/tests/sample_report_rerun.rs`, three legs on
three fresh private databases:

- **baseline** — one transaction per row during import (the
  unoptimized shape), three replay generations, 64 checkouts. The
  recomputed report must equal the fixture byte-for-byte across all
  nine keys.
- **batched import** — the corpus lands through 64-row commit
  transactions (OPT-26-01-02's verdict shape); the report must equal
  both the fixture and the baseline leg's recomputation.
- **aggregated queries** — the name→id and level→ids maps are built
  from the stored corpus, every map answer is asserted equal to the
  scan face's answer (all 512 names, the level-21 seeds), and the
  report recomputed through the faces must equal both the fixture and
  the baseline leg.

The report itself is computed from the stored rows — the level
windows count the replayed levels read back from the database, the
query-shape counts come from `lookup_entities` on the live face — so
a drift in the corpus, the window boundaries or the query shapes
fails as a report mismatch, not as a passing constant-vs-constant
comparison. The fixture file is read even when no server is
reachable, so a missing fixture is always a failure.

## Result

All three legs match:

| Leg | report vs pre-optimization baseline |
| --- | --- |
| baseline replay (per-row import) | all nine keys equal |
| 64-row commit batching | all nine keys equal; equal to the baseline leg |
| aggregated name/level indexes | all nine keys equal; equal to the baseline leg |

The counts: ALL 512, online 64 / offline 448, level-21 property query
3 seeds, top-10 LIMIT 10 rows, windows 129 / 150 / 133 / 100 summing
to 512 — the fixture's committed numbers, reproduced on a fresh
database through every optimization.

## Reading

**The optimizations changed the latency, not the report.** The
64-row batching commits eight times instead of 512 and the
aggregation resolves names ~4700× faster, yet every window and count
lands on the same nine values. This is the consistency the section's
acceptance asks for: whatever import or query shape OPT-26-01 later
keeps, the report a user reads is the report the unoptimized corpus
produced.

**The report anchors survive the replay.** The windows are counted
from the stored levels after three +7-step generations (wrapping at
200), not from the generator's constants, and they still partition to
512. The aggregation's answers are pinned against the scan face's
answers before the report comparison runs, so a stale or
wrongly-keyed index cannot pass while producing a different report.

**Verdict: report consistency holds across the section's
optimizations.** With this row the OPT-26-01 chain is closed:
baseline (01-01), batch-size evaluation with flat memory
(01-02), the ~4700× aggregated name lookup with byte-equal answers
(01-03), and the recomputed report equal under every optimization
(01-04).

## Harness

```sh
BOUNDLESS_PG_URL='host=... port=... dbname=postgres user=...' \
cargo test -p db-postgres --test sample_report_rerun -- --nocapture
```

Without a reachable PostgreSQL server the live legs skip with a
notice; the two structural tests (the fixture is present and pinned,
the replay constants) still run — and the fixture-read test fails on
a missing or malformed fixture regardless of server reachability.
