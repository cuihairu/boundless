# OPT-07-01-03 — Shared-data propagation, quantified

The acceptance quantifies **the message-count change**
(`量化消息量变化`). The row lands the propagation faces the reference
fills a joining app's bundle with, ships the batched join-sync beside
the per-entry form, and measures both — the per-change broadcast stays
as it was.

## The landed faces

**The join fill (the reference form).** `CellAppMgr::addApp`
(`cellappmgr.cpp:1351-1377`) streams every authoritative entry into the
new CellApp's bundle — cell data first, then global data, one
`setSharedData` message per entry; `BaseAppMgr::synchronize`
(`baseappmgr.cpp:1414-1430`) does the same for a ready BaseApp —
base-app data first, then global data. The port was missing both:
`AddAppOutcome::Accepted` now carries the `shared_data` fill, and
`synchronize` gained the missing global map (it streamed only the
base-app map before).

**The batched join-sync (the optimization).** `join_sync()` on both
managers packs the same entries — same order, same state — into **one**
message (`SharedDataSync`: a count, then every `(type, key, value)`).
Applying either form rebuilds the authoritative maps exactly; the
expansion of the batch *is* the per-entry fill (asserted element-wise).

**The per-change broadcast (unchanged).** A set or delete is one
message per app per change — already the minimal form, untouched by
the optimization. The `GLOBAL_FROM_BASE_APP` chain keeps its fixed
four steps (`baseappmgr.cpp:2112-2170`, `cellappmgr.cpp:1924`):
forward, cell broadcast, relay, base broadcast.

## Harness

`crates/server-runtime/tests/shared_data_tuning.rs` — the fixture
carries 24 CELL_APP + 16 GLOBAL entries on the CellAppMgr and 12
BASE_APP + 16 GLOBAL on the BaseAppMgr; apps join one at a time so
every join's fill is measured, then an entry-count ladder walks the
fill from 8 to 256 entries.

```sh
SHARED_DATA_TUNING_OUT=<abs path> cargo test -p server-runtime \
    --test shared_data_tuning -- --nocapture
```

## The message-count change

| Face | Per entry | Batched |
| --- | --- | --- |
| CellApp join (40 entries) | 40 messages | **1** |
| BaseApp join (28 entries) | 28 messages | **1** |
| Ladder at 64 entries | 64 | **1** |
| Ladder at 256 entries | 256 | **1** |

**Verdict: the join cost drops from linear in the map size to
constant — one message per joining app.** For a fleet of `A` apps
joining into maps of `E` entries, the fill drops from `A·E` messages
to `A`. The per-change broadcast is unchanged and stays minimal: a
CELL_APP set is 4 messages for 4 CellApps, a BASE_APP set is 3 for 3
BaseApps, and one logical `GLOBAL_FROM_BASE_APP` change costs 9
messages end to end (1 forward + 4 cell broadcasts + 1 relay + 3 base
broadcasts); the delete chain mirrors it at 8 (no forward — the entry
is already global on both managers).

Payload bytes are recorded, not ranked
(`join_*_bytes`/`ladder_*_bytes`): the payload-only model puts the
batch 4 bytes above the per-entry fill (the entry count), because the
batch's real saving is the `E−1` per-message header costs the model
excludes. The message rows carry the verdict.

## Fixtures

`docs/review/fixtures/shared-data-tuning/shared-data-tuning.tsv` —
50 rows, captured with the command above.
