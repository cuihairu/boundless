# OPT-07-01-04 — Re-run the takeover: entities and shared state correct

Acceptance: **恢复实体和共享状态正确** (the restored entities and the
shared state are correct).

`crates/server-runtime/src/takeover_rerun.rs` runs the whole takeover
once per seed:

1. **Entities** — a seeded 12-entity fleet streams through the real
   backup faces (`write_backup_data`, the `backup_destination` hash/hash
   chain, the `BackupReceiver` store); the source app dies and
   `BackupReceiver::restore` rebuilds every stored backup, each compared
   against its source's properties.
2. **Shared state** — 16 seeded mutations build the two managers'
   authoritative maps through the real `set_shared_data` chain (all four
   scopes, the global scopes through the forward/relay loop); both maps
   then stream out in `recoverSharedDataFromStream`'s count-then-pairs
   shape and a **fresh** manager pair recovers each through the real
   `recover_shared_data_from_stream` faces.

## Recorded ledger (seed 424 242)

| Check | Value |
|---|---|
| entities / restored / property matches | 12 / 12 / 12 |
| BaseAppMgr recovered BASE_APP / GLOBAL entries | 2 / 4 |
| CellAppMgr recovered CELL_APP / GLOBAL entries | 2 / 4 |
| recovered maps equal originals | all four |

Every correctness flag is pinned by the tests: the takeover restores the
whole fleet with matching properties, and every recovered map equals its
original bit for bit. `the_correctness_holds_across_seeds` repeats the
run at three seeds and asserts the same shape every time (the entry
counts are seed-derived: e.g. seed +5 recovers 1/2/1/2).

## Reproducibility

`the_rerun_reproduces_from_its_seed` runs the whole takeover twice and
asserts the ledger equal bit for bit. No wall-clock value is sampled.

## Coverage registration

llvm-cov 0.9.1, filter `-- takeover_rerun`: **99.65%** lines, 100%
regions. The one missed line is the property-match `if let`'s false path
— a restored entity whose properties differ from its source — which the
asserted correctness of every rebuild makes unreachable in this harness.

## Gates

- `cargo test -p server-runtime --lib`: 1745 tests green (16 ignored).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no new
  lint (only the pre-existing unused-workspace-dependency warning for
  `blowfish`).
