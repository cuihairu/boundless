# OPT-09-01-03 — commit batching and archive consolidation

The acceptance: **report commit and restore durations**
(`报告提交和恢复时长`). The reference buffers entity writes into a
`Transaction` and hands the DBMgr **one `COMMIT` per archive period**
(`sqlite_database.cpp:131-177`, the 5 s default `db_config.cpp:247-262`)
or per explicit `WRITE_EXPLICIT` flag, and its consolidation app clears
secondary entries inside a single transaction
(`consolidate_dbs_app.cpp:503-519`). The Boundless PostgreSQL port
exposes the same knobs — `begin_transaction` / `commit_transaction` and
the one-transaction `restore_backup` — but nothing measured what the
batching actually buys. This row measures it on the frozen OPT-09-01-01
corpus.

## Candidates

All legs store or restore the same deterministic 512-row dataset and
assert every row reads back equal — a candidate cannot win by skipping
work.

- **commit batching** — put every row with one `COMMIT` per row, per
  batch of 64 (the archive-period analogue), or once for all 512.
- **archive restore** — `export_backup` a populated database, then
  restore it into a fresh database either as one `restore_backup`
  transaction, or as one transaction per table (the dump split into
  per-table `BackupDump`s), asserting the re-exported dump equals the
  source.

## Record

`docs/review/fixtures/store-commit/commit-candidates.tsv` (16 rows),
captured on the same private PostgreSQL 18.6 instance (port 5434) as
01-01/01-02:

| Leg | commits | wall ms | ops/s |
| --- | ---: | ---: | ---: |
| commit per row | 512 | 1,080.0 | 474 |
| commit per 64 | 8 | 667.5 | 767 |
| commit all | 1 | 706.8 | 724 |
| archive export (512 rows) | — | 36.8 | — |
| archive restore, one txn (12 tables) | 1 | 349.7 | — |
| archive restore, per table (12 txns) | 12 | 364.8 | — |

## Reading

**Batching commits to the archive period nearly doubles write
throughput; beyond one commit per ~64 rows there is nothing left to
win.** Per-row commit costs 1,080 ms for the corpus (474 ops/s);
committing every 64 rows — the reference's archive-period shape —
drops it to 668 ms (767 ops/s, 1.6×). Going all the way to a single
commit is *slower* here (707 ms) because the single 512-row
transaction holds locks and build-up the shorter batches do not, and
the legs are serial anyway — the archive period, not the whole sync,
is the right batch size. This is why the reference commits per archive
cycle rather than once per run.

**The restore is table-shaped, not transaction-shaped.** Restoring
all 12 tables in one transaction (350 ms) and one transaction per
table (365 ms) are within noise of each other — the 512 rows plus the
fixed catalogue dominate, not the `COMMIT` count. So the
consolidation app's single-transaction shape
(`consolidate_dbs_app.cpp:503-519`) buys *atomicity* (a half-cleared
secondary is never observable) rather than speed, and the port's
one-transaction `restore_backup` already matches it. A mid-restore
failure leaves the target database empty either way, because the
per-table leg re-clears each table before inserting.

A harness bug found on the way is worth recording:
`register_entity_type` is in-memory, but `export_backup` discovers
entity tables through the persisted `bigworldEntityTypes` row — so a
populate that only registers gets you a dump with the fixed tables
and no entity rows, and a restore that reads back green but empty.
The harness now persists the type with `add_entity_type` the way
`sync_db` does (`TypesCollector::addType`), which is also the shape a
real DBApp run has.

**Verdict: keep one commit per archive period; the restore face needs
no change.** Both OPT-09-01-02's prepared-statement reuse and this
row's batched commit go into the same-run before/after comparison at
OPT-09-01-04; cross-record numbers move with fleet load.

## Harness

`crates/db-postgres/tests/store_commit_candidates.rs` — one
deterministic run of the five legs; each leg gets a private database
(created and dropped per run) and the whole corpus is asserted present
and equal after every leg.

```sh
BOUNDLESS_PG_URL='host=... port=... dbname=postgres user=...' \
STORE_COMMIT_OUT=<abs path> \
cargo test -p db-postgres --test store_commit_candidates -- --nocapture
```

Without a reachable PostgreSQL server the timed legs skip with a
notice; the structural tests (corpus determinism, batch-size knob)
still run.
