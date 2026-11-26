# OPT-07-01-01 — Backup and archive baseline

Acceptance: **save the bandwidth, the queue and the recovery duration**.

`crates/server-runtime/src/backup_baseline.rs` streams a seeded 48-entity
fleet through the real faces — `write_backup_data` payloads, the
`backup_destination` hash/hash-chain choice, the `BackupReceiver` store, the
takeover `restore`, and the periodic `Archiver` — and records the whole
ledger. Every number is a deterministic count or byte total: no wall-clock
value is sampled, so the ledger reproduces from the seed.

## The scenario

Seed `161_803`. Each entity gets an LCG-derived property set (int, string,
bool with a seeded zero state, float and vector) and a database id when its
id is even (24 of 48 persist). Some entities carry a cell binding.

1. **Backup** — the sender drains the fleet 8 entities per tick through the
   real payload writer and destination selector into the real receiver.
2. **Takeover** — the source app dies; `BackupReceiver::restore` rebuilds
   every stored backup and each is compared against its source properties.
3. **Recovery** — the taking-over app's archiver re-persists the restored
   persisted entities 8 ticks' worth of slices per cycle until the archive
   store holds them all, then every persisted entity round-trips out of the
   store through `restore_archived`.

## Recorded ledger (seed 161_803)

| Field | Value |
|---|---|
| entities / persisted | 48 / 24 |
| backup ticks | 6 (48 at 8 per tick) |
| bandwidth per tick | 1396, 1410, 1412 × 4 bytes |
| backup bytes total | 8 454 |
| sender queue peak | 48 |
| receiver queue peak | 48 |
| restored entities | 48 (all with matching properties) |
| archive recovery duration | 8 ticks |
| archived / round-tripped | 24 / 24 |

The bandwidth is the payload bytes written per backup tick — the telemetry
face the acceptance names; the queue peaks are the outstanding sender and
receiver depths; the recovery duration is the archiver ticks until every
persisted restored entity is back in the archive store.

## Reproducibility

`the_baseline_reproduces_from_its_seed` runs the whole scenario twice and
asserts the ledger equal bit for bit. `run_backup_baseline` panics unless
the drain completes in six slices, every stored backup rebuilds with the
source's properties, and every persisted entity round-trips the archive.

## Coverage registration

llvm-cov 0.9.1, filter `-- backup_baseline`: **98.31%** lines, 100%
regions. The three missed lines are an assertion guard on the drain count,
the restore-expect guard on an already-checked `Result`, and the archive
round-trip `else` arm — all impossible states inside paths the scenario
exercises and asserts.

## Gates

- `cargo test -p server-runtime --lib`: 1728 tests green (16 ignored).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no new
  lint (only the pre-existing unused-workspace-dependency warning for
  `blowfish`).
