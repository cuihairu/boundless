# OPT-07-01-04 — The takeover rerun

The acceptance: **restored entities and shared state correct**
(`恢复实体和共享状态正确`). The drill re-runs the takeover with the
whole cluster in place — the compression-wrapped backup transport
(OPT-07-01-02), the quantified shared-data propagation (OPT-07-01-03) —
and verifies both halves:

## The drill

One run drives, in order:

1. **The BaseApp death.** Every backed-up entity rebuilds on the holder
   through the wrapped frames (`BackupReceiver::restore_wrapped`,
   `backed_up_base_app.cpp:176`); each type ships at its
   `internalNetworkCompressionType` choice (`Account` → zip speed,
   `Mailbox` → zip best).
2. **The CellApp death.** The `DeadCellApps` walk re-points every base
   whose cell part was on the dead app to its space's new CellApp
   (`dead_cell_apps.cpp:234`); bases elsewhere stay untouched.
3. **The archive rebuild.** Every persisted entity rebuilds through the
   compressed archive bytes (`restore_archived_with_compression`,
   `createBaseFromStream`, `baseapp.cpp:2589`).
4. **The manager restart.** Both managers' authoritative maps come back
   through the recovery stream (`recoverSharedDataFromStream`,
   `baseappmgr.cpp:1249`), and a replacement CellApp and BaseApp join
   the recovered managers: the per-entry fill carries the whole
   authoritative set, and the batched sync carries the same state in
   **one** message (the OPT-07-01-03 form).

## The verdict

| Face | Result |
| --- | --- |
| Dead BaseApp's entities restored | 24/24, each equal to the live entity |
| Dead CellApp's bases re-pointed | 24/24 to the space's new CellApp; 72 untouched |
| Archived entities rebuilt | 96/96, each equal to the live entity |
| Recovered maps equal pre-death | CELL_APP 24, BASE_APP 12, GLOBAL 16 — exact |
| Replacement CellApp join | fill 40 messages, batch **1** |
| Replacement BaseApp join | fill 28 messages, batch **1** |
| Rerun equality | entity digest `0395ae5ba2899fa5`, shared digest `1998c4d915699085` — both runs identical |

**Correctness holds end to end**: the wrapped backup frames, the
re-point walk, the compressed archive bytes and the recovery stream all
rebuild the exact pre-death state (lossless per entity, asserted field
by field through the shared projection), and a replacement app joining
a recovered manager rebuilds the authoritative maps from one batched
message. The rerun produces the same entity and shared-state digests —
the takeover is deterministic, so rerunning it is safe.

## Harness

`crates/server-runtime/tests/takeover_rerun.rs` — the baseline load (4
BaseApps × 24 entities, alternating types, cell parts on 4 CellApps
across 3 spaces), both deaths, both rebuilds, the manager restart and
the replacement joins, run twice.

```sh
TAKEOVER_RERUN_OUT=<abs path> cargo test -p server-runtime \
    --test takeover_rerun -- --nocapture
```

## Fixtures

`docs/review/fixtures/takeover-rerun/takeover-rerun.tsv` — 42 rows,
captured with the command above.
