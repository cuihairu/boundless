# OPT-08-01-04 — The state recheck over the cluster's re-runs

The acceptance: **re-run the visible sets, the event order and the
migrations — the states are correct** (`复跑可见集、事件顺序和迁移。
验收 状态正确。`). The harness replays the three faces the cluster
measured and asserts their state after every tick and every stage, not
just at the end.

## The visible sets (可见集)

The OPT-08-01-01 d400 walk re-runs with the geometric truth recomputed
every tick — the square `(central − range, central + range]` on both
axes, exactly `is_in_x_range`/`is_in_z_range`'s comparisons:

| Face | Value |
| --- | --- |
| Ticks walked | 175 |
| Visible-set equality checks | 175 (one per tick) |
| Alias-state checks | 70 000 (400 entities × 175 ticks) |
| Snapshot-on-grid checks | 50 000 (admitted entities per tick) |
| Enter / leave events | 400 = 400 |
| Warnings | 0 |
| Final AoI | 0 — every alias back in the pool |

Per tick, all of `witness.aoi_ids()`, every alias (`cache_flags`) and
every admitted snapshot's position match the geometric set: the
scheduler's visible set is never wrong for even one tick.

## The event order (事件顺序)

The events are the containment transitions, nothing more:

- Every flip into the range produces exactly one enter, every flip out
  exactly one leave — the per-tick event set equals the expected-set
  difference, both directions, all 175 ticks (800 events = 800 flips).
- Each entity's enter tick and leave tick equal the ticks its
  containment actually flipped, reconstructed from the geometry alone.
- Every entity: exactly one enter, exactly one leave, enter before
  leave; and this walk's sweep admits the whole block before releasing
  any of it (pinned in the harness: `max_enter < min_leave`).
- The whole ledger replays the baseline digest
  (`49bf554c6f1c96b7`) — the re-run is the same event stream, byte for
  byte.

The hysteresis form (the C2 candidate's two live triggers, radius and
radius + 50) re-runs over a 10-period flutter sweep (250 ticks):

| Face | Value |
| --- | --- |
| Inner-ledger alternation | holds — enter/leave strictly alternate per entity |
| Inside the radius ⇒ admitted | holds, 100 000 entity-ticks checked |
| Admitted ⇒ inside the release band | holds, 100 000 entity-ticks checked |
| Admissions / releases / still admitted | 280 = 200 + 80 — the ledger conserves |
| Suppressed churn | 1 720 events (720 re-enters + 1 000 inner leaves) |
| Settled admitted set | 80 = the release band at the swing's end |

## The migration (迁移)

The OPT-08-01-03 sequence re-runs with a per-tick message pump — B
applies each create as it arrives on its channel — and every stage's
end state pinned before the next runs:

| Stage | State after the stage |
| --- | --- |
| Approach (ticks 1–10) | A owns the one real, no ghosts; C silent; B's ghost table goes 0 → 1 exactly on the ghost tick (5), the ghost pointing home |
| The plan (tick 10) | The plan names B and the walker, on the crossing tick |
| The offload | No real left on A; A's ghost points at B; 0 messages to B; the haunt list `[B, A]` |
| The onload | B's ghost table empty (the ghost became the real); the re-point rides to A (1 message); C silent |
| The settle | A's ghost relocated; exactly 1 real in the world, at B |

The teleport ends the same way from a fresh world: the stale
`delGhost` rides to the old haunt first, the destination starts and
ends with no ghosts (`tspace_final_ghosts_b` = 0), the streamed space
id is rewritten to the destination's (7), and the nearby haunt takes
the real.

## Harness

`crates/server-runtime/tests/aoi_state_recheck.rs` — three
deterministic tests, one per leg.

```sh
AOI_STATE_RECHECK_OUT=<abs path> cargo test -p server-runtime \
    --test aoi_state_recheck -- --nocapture
```

## Fixtures

`docs/review/fixtures/state-recheck/state-recheck.tsv` — 38 rows,
captured per test with the command above.
