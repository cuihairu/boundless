# OPT-09-01-04 — Transactions, retries and recovery, rerun to identical data

Acceptance: **数据一致** (the data is consistent).

`crates/db-postgres/tests/rerun_recovery.rs` runs one seeded workload —
32 entity rows, values from a fixed LCG — through the three recovery
disciplines on the real PostgreSQL server, and every leg must
reproduce the very same data:

1. **Transaction rerun** — the same `START TRANSACTION` … `COMMIT` run
   on two fresh databases exports two **identical dumps**: the rerun
   reproduces the database record-for-record. The rollback leg opens a
   transaction, stages an overwrite (visible to the same connection's
   dirty read) and a staged delete (the row reads `None` mid-
   transaction), rolls back, and the post-rollback export equals the
   pre-transaction snapshot; the overwritten row and the deleted row
   are both back exactly as they stood.
2. **Retries** — `retry_query`, `query_with_reconnect` and the
   reference `RetryTask` loop (`run_with_retry`) each take a scripted
   first-attempt failure and then perform a **real read** on the live
   server through a second connection:
   - the lock conflict (`DbError::Retry` — what the store itself
     raises for SQLSTATE `40P01`/`55P03`, `store.rs:48-55`) retries
     once and returns the row (2 attempts);
   - the dropped connection (`DbError::LostConnection`) reconnects and
     re-sends the very same read once (2 attempts, 1 reconnect); with
     no reconnect available the loss surfaces and the statement is
     never re-sent (1 attempt, the caller's in-transaction guard);
   - a fatal failure (MySQL `1045` classed by
     `DatabaseFailure::classify`) reports straight to the handler:
     1 attempt, no retry, `fatal` set, completion `false`;
   - the completion callback fires **exactly once** per operation
     whatever the attempt count.
3. **Recovery** — an empty initialised target `restore_backup`s the
   source's dump, then re-runs the same transactional workload over
   the recovered rows (updates this time: the rows exist, so the
   rerun goes through the update face) and exports the source's dump
   again — recovery plus rerun agree on every record.

## The consistency ledger (asserted, reproduces anywhere)

| Leg | Assertion |
|---|---|
| transaction rerun | dump A == dump B (12 tables, identical records) |
| in-transaction flags | `in_transaction()` true between begin and commit, false after commit/rollback |
| rollback | post-rollback dump == pre-transaction snapshot; dirty read saw the staged write mid-transaction |
| lock retry | 2 attempts, 1 `on_retry`, row equal, completion once (`true`) |
| reconnect | 2 attempts, 1 reconnect, row equal; guard: 1 attempt, error surfaced |
| fatal | 1 attempt, `fatal = "access denied"`, completion once (`false`) |
| recovery rerun | restored target's post-rerun export == source dump |

## The report (PostgreSQL 18.6, local unix socket, 2 green runs)

| Leg | Run 1 / run 2 |
|---|---|
| transaction (32 writes + verify reads) | 125.6 / 87.3 ms |
| transaction rerun on the second database | 73.1 / 89.4 ms |
| archive restore (193 rows) | 36.0 ms |
| rerun over the recovered rows | 79.1 ms |

Wall-clock is environment data: printed by the test, never asserted —
the load lane's gate (BW-17-02-08). The consistency ledger above is
the acceptance, and it reproduces on any runner with a reachable
server.

**Face contract caught a real shape error before the green run:** the
first draft re-put the rerun rows with the create flag set, which is
the INSERT face — the store answered `AlreadyExists` on every leg that
revisits existing rows. The rerun shapes now write through the update
face (`put_entity(..., false)`), which is the contract
(`put_entity_task.cpp:93-97`) doing its job.

**Scope note:** `recovery::retry_transaction`'s image-restore loop is
the in-memory backend's discipline — `EntityStorageImage` is
implemented for `MemoryStorage`/`MemoryConnection` only
(`db/src/storage.rs:884-901`), so there is no PG image to restore;
its PG-side counterpart is the transaction/rollback leg above, and
the loop itself stays covered by the db crate's own tests.

## Gates

- `cargo test -p db-postgres`: 138 passed, 0 failed (real server; the
  usual skip gate applies where none is reachable).
- `cargo fmt -p db-postgres --check`: clean.
- `cargo clippy -p db-postgres --all-targets -- -D warnings`: no new
  lint.
