# OPT-07-01-02 — Compare backup queue and recovery under one load

Acceptance: **按同负载比较队列与恢复** (compare the queue and the recovery
under the same load).

`crates/server-runtime/src/backup_tuning.rs` runs the OPT-07-01-01 seeded
48-entity fleet twice through the same real backup faces — once with the
shipped pacing and once with a tuned one — and records both ledgers whole.
The load is identical: same fleet, same seeded properties, same payload
bytes, same persisted set. `BackupComparison::same_load` asserts exactly
that, so every remaining difference is the pacing's effect.

The seam lives on the baseline module: `BackupTuning` (the slice the sender
backs up per tick, the archiver cycle) whose `Default` reproduces the
shipped ledger bit for bit, pinned by `the_default_tuning_reproduces_the_shipped_ledger`.

## The comparison (seed 161_803)

| Metric | Shipped (slice 8, period 8) | Tuned (slice 16, period 4) |
|---|---|---|
| backup ticks | 6 | 3 |
| bandwidth per tick (B) | 1396, 1410, 1412, 1412, 1412, 1412 | 2806, 2824, 2824 |
| payload bytes total | 8 454 | 8 454 (same) |
| archive recovery ticks | 8 | 4 |
| fleet / persisted / restored | 48 / 24 / 48 | identical (`same_load`) |

The queue drains in half the ticks and the recovery cycle re-persists in
half the ticks. The price is a doubled per-tick bandwidth peak — the same
bytes moved in fewer ticks, not more bytes (totals assert equal in
`the_tuned_pacing_pays_a_higher_bandwidth_peak`). The direction assertions
are honest: the tuned queue/recovery are asserted faster, the tuned
bandwidth peak is asserted higher, and the load identity is asserted equal.

## Reproducibility

`the_comparison_reproduces_from_its_seed` runs the whole comparison twice
and asserts both ledgers equal bit for bit.

## Coverage registration

llvm-cov 0.9.1, filter `-- backup_tuning`: **100%** lines and regions
(53 lines). The baseline module itself stays at its registered 98.31% —
the seam refactor preserved the three impossible-state misses registered
in the OPT-07-01-01 doc.

## Gates

- `cargo test -p server-runtime --lib`: 1733 tests green (16 ignored).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no new
  lint (only the pre-existing unused-workspace-dependency warning for
  `blowfish`).
