# OPT-07-01-01 — Backup and archive baseline

The acceptance records the **bandwidth, queue peaks and recovery time**
of the fault-tolerance write path (保存带宽、队列及恢复耗时). The
`BW-07-04` port already ships every module the write path needs —
`backup_hash` (target hashing + dead-app chain), `base_backup`
(sender destination, payload, receiver), `offloaded_backup` (retiring
holdings) and `archiver` (the periodic persist cycle). This row
establishes the measurement harness over that shipped machinery and
records the three windows; it changes no hot path.

## Harness

`crates/server-runtime/tests/backup_baseline.rs` — an integration test
(every seam the harness needs is public). The fixture is 96 base
entities on 4 BaseApps (alternating `Account`/`Mailbox` types, five and
three persisted props, every entity has a database id and a cell
address), a `BackupHash` over all four apps, `backupPeriodInTicks =
10`, `archivePeriodInTicks = 8`. The harness drives the shipped code
the way the reference's `BackupSender::tick` does
(`backup_sender.cpp:109`): the per-tick slice is
`size/period + floating remainder` off a cycle queue
(`backup_sender.cpp:125`), the destination comes from the hash, and
the payload is `write_backup_data` — the same stream the archiver
persists, so a backup byte is an archive byte.

```sh
BACKUP_BASELINE_OUT=<abs path> cargo test -p server-runtime \
    --test backup_baseline -- --nocapture
```

All ladders are synthetic-clock deterministic; wall-clock rows
(`recover_backup_wall_ms`, `baseline_wall_ms`) are recorded, never
asserted.

## Bandwidth (one full backup cycle)

| Metric | Value |
| --- | --- |
| Window bytes (96 entities, one period) | 9600 |
| Window messages | 96 |
| Per-tick bytes (10 ticks) | 884–1000 |
| Payload size min / median / max | 84 / 116 / 116 |

The per-destination split (the `BundleSendingMap` face — bytes land in
per-app bundles):

| Destination app | Bytes | Messages |
| --- | --- | --- |
| app 1 | 2452 | 25 |
| app 2 | 2464 | 24 |
| app 3 | 2336 | 24 |
| app 4 | 2348 | 23 |

The hash spreads the load evenly (±5% of a quarter each), and the
asserted cycle coverage pins the remainder arithmetic: over exactly
one period every entity is backed up exactly once (no duplicates, no
gaps). Bytes per entity are fully determined by the type: 84 for the
3-prop `Mailbox`, 116 for the 5-prop `Account` — bandwidth scales
linearly with the persisted property set.

## Queue peaks

| Queue | Peak | Recorded face |
| --- | --- | --- |
| Sender cycle queue (`basesToBackUp_`) | 96 = the population | drains to empty within the period |
| Receiver held bytes | 9600 total; per source app 2784/2016/2784/2016 | one window, no replacements |
| Archive store | 9600 bytes, 96 entries | 12 entities per tick × 8 ticks |

The held bytes per source app are exactly 24 entities × their type's
payload (24 × 116 and 24 × 84): the fixture's ownership rule and the
hash's even split are both visible in the receiving-side peak. The
archiver's slice schedule is the same `size*index/period` arithmetic,
asserted exact per tick.

## Recovery

**Backup takeover.** App 1 dies holding 24 owned entities. The holder
restores all 24 from its stored backups — 2784 bytes consumed, and
every restored entity's property set compares equal to the live entity
(the `write_backup_data` round trip is lossless for the fixture's
value kinds). Wall ms recorded: 0.580.

**Archive recovery.** With a secondary database the death restarts the
archive cycle at once (`archiver.cpp:134`) and
`informOfArchiveComplete` fires one full period after the death — 9
ticks at period 8 (the restart zeroes the index, so the inform lands
at the *next* gate). Recorded, and the persisted state restores equal
for a probe entity. The `NEXT_ONLY` auto-archive policy archives
exactly once across two cycles (asserted).

## Notes for the tuning rows (OPT-07-01-02/03)

- The bandwidth face is a pure function of the population and the
  payload format — any serialization tuning (01-02) must show up as
  smaller payload rows at equal coverage.
- The queue peaks are bounded by the periods; a tuning that changes
  `backupPeriodInTicks` or the slice arithmetic moves the per-tick
  rows, not the window totals.
- Recovery cost is restore-op-dominated: the takeover consumed 2784
  bytes to rebuild 24 entities, ~116 bytes per entity regardless of
  type mix in the restored set.

## Fixtures

`docs/review/fixtures/backup-baseline/backup-baseline.tsv` — 49 rows,
captured with the command above.
