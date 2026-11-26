# OPT-08-01-01 — The dense-AoI baseline

The acceptance: **record Tick, broadcast volume, and migration time**
(`保存 Tick、广播量和迁移耗时`). The baseline drives a live observer
through dense blocks on the ported faces — the `RangeList` index with
an AoI trigger, the `Witness` scheduler under a packet budget — and
records the per-tick cost, the broadcast bytes, and the boundary
migration window.

## The three legs

1. **The index ladder.** A fixed 200×200 block holds
   `side² = 49 / 100 / 196 / 400` stationary entities; an observer
   walks `x = −600 → +800` at 8 units per tick (175 ticks) through
   `z = 95`, its AoI trigger at `DEFAULT_AOI_RADIUS` (500) firing
   enter/leave into a recorder. Every tick shuffles the observer's
   node and is timed; every entity is entered and left exactly once
   and the chains stay `is_sorted()` through the walk.
2. **The scheduler.** The same walk on the densest block feeds a
   `Witness` in lockstep: each crossing's enter runs the admission
   handshake (`upsert_entity` → `add_to_aoi` → flush →
   `request_entity_update` → flush), each leave releases the alias,
   and `update()` spends the volatile budget every tick. The budget is
   `bps_to_packet_size(50_000, 10) = 597` bytes per tick (50 kbps at
   10 Hz).
3. **The migration window.** The per-tick crossing schedule of the
   same walk: when the boundary first reaches the block, when it
   leaves it, and how many ticks carry churn.

## The Tick cost

| Density | Build (ms) | Shuffle avg (ns/tick) | Shuffle max (ns) | Walk (ms, 175 ticks) |
| --- | --- | --- | --- | --- |
| d49 | 0.739 | 2 302 | 9 641 | 0.442 |
| d100 | 3.118 | 3 944 | 33 821 | 0.735 |
| d196 | 11.824 | 4 374 | 37 250 | 0.824 |
| d400 | 39.046 | 4 806 | 36 262 | 0.898 |

The per-tick shuffle stays flat against density (2.3 → 4.8 µs from 49
to 400 entities): the trigger only touches the two chains it crosses,
so the index cost is log-kinetic, not population-proportional. The
one-off build is the population-proportional face (0.7 → 39 ms).

## The broadcast volume

| Face | Value |
| --- | --- |
| Budget | 597 bytes/tick (50 kbps @ 10 Hz) |
| Admission handshake | 1 200 messages / 9 600 bytes — **3 messages, 24 bytes per entity**, outside the budget |
| Volatile updates | 12 094 messages / 29 474 bytes over 175 ticks |
| Max tick | 276 messages / 601 bytes |
| Warnings | 0 |

The admission handshake rides outside the volatile budget (the
`EnterAoI`/`CreateEntity`/`LeaveAoI` forms), so the 597-byte gate only
constrains the update pass. The max tick lands at **601 bytes — four
over the budget**: the pass checks the budget *before* each send
(`witness.cpp`'s pre-send gate), so one in-flight element may land
after the cut; the harness pins the overshoot to be within the
budget plus that tick's largest message. Per entity, the whole pass
costs ≈ 74 bytes of volatile traffic.

**Cross-face pin: the witness's event-ledger digest equals the d400
index digest (`49bf554c6f1c96b7`)** — the scheduler admitted and
released exactly the crossings the index fired, byte for byte.

## The migration time

| Face | Value |
| --- | --- |
| First enter | tick 13 |
| Last enter | tick 37 |
| Admission window | 25 ticks (2.5 s at 10 Hz) |
| Ticks carrying any crossing | 40 (4.0 s) |
| Events total | 800 = 2 × 400 (enter + leave per entity) |

The boundary takes 25 ticks to sweep the block's admission window and
40 ticks of total churn to clear it — the migration cost the later
evaluation rows (OPT-08-01-02/03) compare candidate changes against.

## Harness

`crates/server-runtime/tests/aoi_baseline.rs` — the ladder, the
scheduler leg and the migration window, one deterministic run.

```sh
AOI_BASELINE_OUT=<abs path> cargo test -p server-runtime \
    --test aoi_baseline -- --nocapture
```

## Fixtures

`docs/review/fixtures/aoi-baseline/aoi-baseline.tsv` — 55 rows,
captured with the command above.
