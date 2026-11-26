# OPT-09-01-04 — rerun of transactions, retries and recovery

The acceptance: **the data stays consistent** (`数据一致`) after the
section reruns 事务、重试与恢复. The reference drives every entity
operation as a background task whose loop retries a rolled-back attempt
on a lock failure, chases a lost connection, and fires the completion
callback exactly once (`MySqlBackgroundTask::doBackgroundTask`,
`tasks/background_task.cpp:51-100`); the port carries that contract in
`db::retry` plus the reconnect legs of `PostgresStorage`
(`map_db_error` calls 40P01/55P03 retryable and the connection-level
family lost). This row reruns the OPT-09-01-01 corpus through those
faces on the live backend and asserts the full 512-row dataset present
and equal after every scenario.

## Scenarios

- **rerun** — the corpus through the retry-driven task loop, one
  transaction per row versus per 64 rows (the 01-03 verdict's
  archive-period shape), same-run walls for the before/after pair.
- **retry (lock)** — the blocker holds row 1 under `FOR UPDATE`, the
  writer's update times out on `lock_timeout` (PostgreSQL's 55P03,
  mapped to `DbError::Retry`), the blocker commits, and the same
  operation reruns on the same connection.
- **retry (scripted)** — a first-attempt `Retryable` failure through
  `run_with_retry`: the real put lands exactly once, on attempt 2.
- **recovery (rollback)** — a transaction dies on a duplicate
  explicit-id put, rolls back; a witness connection sees none of the
  dead batch, and the same batch reruns clean.
- **recovery (lost connection)** — the backend is killed with
  `pg_terminate_backend`: outside a transaction the put recovers under
  the statement's own reconnect leg; inside one the loss is terminal
  (`DbError::LostConnection`), the rollback reports the dead
  connection, the caller reconnects manually and reruns the batch.
- **recovery (restore)** — the table is wiped (512 rows deleted) and
  rebuilt from the exported backup, rows asserted equal.

## Record

`docs/review/fixtures/store-recovery/recovery.tsv` (21 rows), captured
on the same private PostgreSQL 18.6 instance (port 5434) as the
previous rows:

| Scenario | result |
| --- | ---: |
| rerun per row (512 tasks) | 300 ops/s |
| rerun per 64 (8 tasks) | **911 ops/s** |
| retry on 55P03 lock_timeout | 2 attempts, lands after the blocker commits |
| scripted first-attempt failure | 2 attempts, 1 `on_retry`, lands once |
| rollback of a failed transaction | 0 surviving rows, rerun 8/8 batches |
| terminated backend, outside a transaction | 1 auto-recovered put |
| terminated backend, inside a transaction | 1 manual reconnect, rerun 8/8 |
| wiped table restored from backup | 512 rows, 12 tables, 259 ms |

## Reading

**Consistency holds on every path.** The four failure shapes — a
retryable lock, an injected retryable failure, a dead transaction, a
killed backend — all leave the dataset exactly equal to the corpus
after their recovery runs, which is the acceptance. The counts are the
contract's: one retry for a retryable failure (attempts 2, `on_retry`
1), zero surviving rows from a rolled-back batch, exactly one
auto-recovery per killed connection outside a transaction, one manual
reconnect inside one.

**The archive-period commit shape tripled throughput on the
retry-driven loop** (300 → 911 ops/s), consistent with 01-03's
measurement on the direct path — the rerun loop adds one
`begin/commit` pair per batch and the retry bookkeeping, and the
batching still dominates. The rerun-vs-rerun pair in this record is
the same-run before/after the section's verdicts asked for; per-row
commit is the thing to avoid, the batch of 64 is the shape to keep.

Two harness findings worth carrying:

- `pg_terminate_backend` may leave the 57P01 error message in flight
  rather than closing the socket under the caller immediately, so a
  scenario that kills a backend drives a probe statement until the
  loss is actually noticed before asserting the recovery contract.
  Otherwise the first failing statement can surface either the
  connection-level family (auto-recovered) or a server error
  (reported straight to the caller) depending on timing.
- `run_with_retry` loops until the operation commits or fails
  fatally, so a task that always returns `Retryable` never returns —
  the harness only feeds it failures that are bounded by construction.

**Verdict: the transaction, retry and recovery faces are consistent
under the section's failure shapes; keep the per-batch commit.** With
this row the OPT-09-01 chain is closed: baseline (01-01), prepared
statements over pooling (01-02), archive-period batching with the
one-transaction restore kept for atomicity (01-03), and consistency
under rerun/retry/recovery (01-04).

## Harness

`crates/db-postgres/tests/store_recovery.rs` — six scenarios in one
run, each on its own private database (created and dropped per run),
the whole corpus asserted present and equal after every scenario.

```sh
BOUNDLESS_PG_URL='host=... port=... dbname=postgres user=...' \
STORE_RECOVERY_OUT=<abs path> \
cargo test -p db-postgres --test store_recovery -- --nocapture
```

Without a reachable PostgreSQL server the scenarios skip with a
notice; the structural tests (corpus determinism, batch-size knob)
still run.
