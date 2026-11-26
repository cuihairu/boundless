# OPT-26-01-02 — import batch-size evaluation

The acceptance: **throughput and peak memory are reported**
(`报告吞吐和峰值内存`). The OPT-26-01-01 baseline landed the fixed
corpus through the migration import one row at a time; this row
evaluates the import's commit batching — the same 512-row corpus lands
on a fresh table under five transaction shapes (one commit per 1, 16,
64, 256 rows, and the whole corpus in one transaction), each shape
reporting its wall, its rows/s throughput and its counting-allocator
peak, with the corpus read back equal after every shape.

## Shapes

`crates/db-postgres/tests/import_batch_candidates.rs`: the migration
face (`import_entities`, get-then-put per row) runs inside a
`begin/commit` pair per chunk; after each shape the table is truncated
so every shape pays the same fresh-table writes. The record:
`docs/review/fixtures/import-batch/import-batch.tsv` (30 rows),
captured on the private PostgreSQL 18.6 instance (port 5434):

| batch | commits | wall ms | rows/s | allocs | peak KiB |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 512 | 1116.5 | 459 | 131 166 | 16 |
| 16 | 32 | 806.8 | 635 | 105 733 | 16 |
| 64 | 8 | 822.0 | 623 | 104 461 | 16 |
| 256 | 2 | 807.6 | 634 | 104 148 | 16 |
| 512 (one) | 1 | 819.7 | 625 | 104 113 | 16 |

## Reading

**Per-row commit is the shape to avoid; every batched shape converges.**
One commit per row costs ~1 117 ms (459 rows/s) — the commit round
trips dominate. From 16 rows per commit onward the wall flattens to
~810 ms (~630 rows/s) regardless of batch size: **~1.4×** over
per-row, and the gain saturates immediately. The migration face's own
get-then-put per row becomes the floor once commits stop dominating,
which is why 256 and 512-row transactions buy nothing over 16 — and
why the all-in-one transaction is not a win here the way it looked
worth testing after OPT-09-01-03's flatter result. The archive-period
shape (batch ≈ 64) remains the safe default: it sits on the flat part
of the curve without holding a long-lived transaction open.

**Batching trades no memory.** The allocator peak is flat at 16 KiB
across all five shapes — the row values are streamed chunk by chunk,
so the batch size only changes how long rows stay inside one
transaction, not how many live at once. The allocation *count* does
move: per-row pays 131 166 allocs, the batched shapes ~104-106k
(the ~26k delta is the per-commit statement objects). The counts were
bit-identical across three capture runs, so any change in allocation
shape from OPT-26-01-03's aggregation shows up as a count delta, not
noise.

**Verdict: keep 64-row commits for the import.** 16 already reaches
the flat part; 64 matches the OPT-09-01-03 archive-period verdict and
leaves per-commit cost with margin. The throughput and peak-memory
figures are the report the acceptance asks for; OPT-26-01-03 takes
the query side.

## Harness

```sh
BOUNDLESS_PG_URL='host=... port=... dbname=postgres user=...' \
IMPORT_BATCH_OUT=<abs path> \
cargo test -p db-postgres --test import_batch_candidates -- --nocapture
```

Without a reachable PostgreSQL server the live leg skips; the two
structural tests (the shapes partition the corpus, corpus
determinism) still run.
