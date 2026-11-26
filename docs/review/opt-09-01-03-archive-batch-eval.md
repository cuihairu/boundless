# OPT-09-01-03 — Write batches and archive integration, evaluated

Acceptance: **报告提交和恢复时长** (report the commit and recovery
durations).

`crates/db-postgres/tests/archive_batch_eval.rs` lands ONE seeded
64-row workload (values from a fixed LCG, each shape under its own id
base) through three write shapes on the real PostgreSQL server, then
runs the archive round trip:

1. **autocommit** — a `put_entity` per row, each its own commit. 64
   statements, 64 commits.
2. **transaction batch** — the same 64 writes inside one
   `START TRANSACTION` … `COMMIT`. 66 statements, 1 commit.
3. **write batch** — one multi-row `INSERT` built from the schema's own
   columns, every row's values in a single statement. 1 statement, 1
   implicit commit.

The archive integration is the delivered backup face pair:
`export_backup` dumps the whole populated source (12 tables, 193 rows),
`restore_backup` lands it in an empty initialised target, and the
target's own export compares equal to the dump — record identity, so
the recovery is correct, not just fast.

## The report (PostgreSQL 18.6, local unix socket, 2 runs)

| Shape | Statements | Commits | Elapsed (run 1 / run 2) |
|---|---|---|---|
| autocommit | 64 | 64 | 554 / 531 ms |
| transaction batch | 66 | 1 | 171 / 160 ms |
| write batch | 1 | 1 (implicit) | 5.4 / 4.0 ms |
| archive export | 1 face call | — | 51 / 27 ms |
| archive restore (193 rows) | — | — | 144 / 148 ms |

**Candidate gains, as measured on this runner:**

- **Transaction batching** collapses 64 durability flushes into 1: the
  same 64 rows land in ~165 ms instead of ~540 ms — **~3.3×**.
- **The single-statement write batch** goes further: one round trip
  lands all 64 rows in ~4–5 ms — **~100×** the autocommit shape and
  **~35×** the transaction batch. Every row reads back identical in all
  three shapes (`rows_ok = 64` each), so the batching changes cost,
  not content.
- **Recovery**: the empty target reproduces the source exactly —
  `restored == dump` — in ~145 ms for 193 rows; the export/re-export
  pair (the verification read) costs ~30–50 ms. The recovery duration
  is dominated by the restore's INSERT fan-out, which is itself a write
  batch the same evaluation would speed up.

Wall-clock is environment data: printed by the test, never asserted —
the load lane's gate. The assertions are the structural counts
(statements, commits, rows affected) and record identity (row equality,
dump equality), which reproduce anywhere. The MySQL statement shapes are
the same by construction; OPT-09-01-01's common fixture pins them on the
live MariaDB.

## Gates

- `cargo test -p db-postgres`: 135 passed, 0 failed.
- `cargo fmt -p db-postgres --check`: clean.
- `cargo clippy -p db-postgres --all-targets -- -D warnings`: no new
  lint.
