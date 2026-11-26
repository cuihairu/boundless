# OPT-05-01-02 — Tuning the load reporting and the split/merge balance

The acceptance for this row is *compare the scheduling effect on fixed
scenarios* (`比较固定场景的调度效果`). The OPT-05-01-01 baseline fixed
the three windows (allocation, recycling, recovery) and pinned their
geometry; this row varies the tuning knobs the reference exposes as
runtime config and compares the ladders. **The engine is unchanged** —
every knob the comparison varies is read by the shipped code at
`do_balance` time, the way an operator would set it in
`cellappmgr`'s config section.

## Knobs under comparison

| Knob | Shipped value | Reference seam |
| --- | --- | --- |
| Load-report smoothing | `LOAD_SMOOTHING_BIAS = 0.05` EMA | `CellApp::informOfLoad` |
| Split/merge aggression | `AGGRESSION_DECREASE = 0.5` on reversal, `aggression_increase()` per pass, clamp `[min_aggression(), 1.0]` | `InternalNode::adjustAggression` |
| Chunk limiting | `BalanceContext.limit_to_chunks` (`SHOULD_LIMIT_BALANCE_TO_CHUNKS` shipped) | `CellAppMgrConfig::shouldLimitBalanceToChunks` |

The harness lives beside the baseline
(`cellappmgr_tuning.rs`, `#[cfg(test)] #[path]` module, sink env
`CELLAPPMGR_TUNING_OUT`):

```sh
CELLAPPMGR_TUNING_OUT=<abs path> cargo test -p server-runtime --lib \
    cellappmgr_tuning -- --exact --nocapture
```

## Scenario 1 — hotspot ramp across reporting biases

The baseline's allocation window, re-run with the harness EMA at the
candidate biases `{0.02, 0.05 (shipped), 0.1, 0.2}`. The harness EMA
is validated element-wise against the shipped `inform_of_load` at the
default bias first (`ramp_default_bias_informs`), so the candidate
numbers are comparable.

| bias | informs to 0.6 | passes to the entity edge | final edge |
| --- | --- | --- | --- |
| 0.02 | 52 | 2 | 4000 |
| 0.05 (shipped) | 21 | — | — |
| 0.1 | 10 | 2 | 4000 |
| 0.2 | 5 | 2 | 4000 |

**Verdict: the bias only trades detection latency, not the landing
geometry.** Every candidate binds the same entity edge (`4000`) in the
same two passes; only the informs-to-threshold count moves, and it
moves inversely with the bias (52 → 21 → 10 → 5). The reference's
default 0.05 sits between the smoothing stability of 0.02 and the
reaction speed of 0.1.

## Scenario 2 — the reversal across the aggression tuner's branches

B becomes the hot side of the OPT-05-01-01 split; the partition must
reverse and travel up. The fixture seeds `prev_direction` to place the
first post-swap pass on the reversal branch (halve) or the continuation
branch (stay at max), then records the aggression after each of 8
passes. Both branches land on the same entity level — `closest_limit`
is monotone (`Right => max(position, min(l1, l2))`), the line never
moves backward — so the tuner's observable is the aggression trajectory
itself, not the landing position (`swap_final_edge = 5000`, the level-1
bound, for both).

| pass | damped (reversal) | live (continuation) |
| --- | --- | --- |
| 1 | 0.5000 | 1.0000 |
| 2 | 0.5359 | 1.0000 |
| 4 | 0.6156 | 1.0000 |
| 8 | 0.8123 | 1.0000 |

**Verdict: the damping is the anti-oscillation face.** A reversal
halves the aggression (the first move after the flip is deliberately
half-sized), and the recovery is `×1.0718` per pass back to the max —
about 10 passes to fully re-arm. The continuation branch keeps the
max the whole window. Both cost nothing in final geometry.

## Scenario 3 — the retirement drain across the chunk-limit switch

The baseline's held arm (a real-area retiring cell that the survivor's
chunk ghost margin pins at its line), re-run both ways:

| `limit_to_chunks` | passes | CellRemovals | survivor range |
| --- | --- | --- | --- |
| on (shipped) | 8 (held, line stays 4000) | 0 | n/a |
| off | 1 | 1 | `Rect::unbounded()` |

The fixture detail that makes the off-arm honest: the retiring cell is
empty, so its CellApp reports the bounds an empty cell reports in the
reference (`Space::writeEntityBoundsForEdge`, cellapp side) — every
offload level at the unbounded edge with the level's offload floor as
load. That is what lets `entity_limit_in_direction` travel to the
world edge and shrink the cell to zero area once the chunk limit stops
binding. With the limit on, the survivor's loaded-chunk margin
(`chunk_limit = survivor edge - GHOST_DISTANCE`) pins the line at
4000 and the drain is *held* — the same face OPT-05-01-01 recorded.

**Verdict: the chunk limit trades retire latency against ghost-margin
safety.** Off, retirement completes in one pass; on, the space keeps
paying the retiring cell until the survivor's chunks cover the area.

## Comparison method notes

- All ladders are synthetic-clock deterministic; no wall-clock number
  is asserted (the `tuning_wall_ms` row is recorded only).
- The harness drives the live tree through `Space::load_balance` with
  an operator-chosen `BalanceContext.limit_to_chunks`, replicating
  `CellAppMgr::load_balance`'s loop (the shipped switch is a constant
  read at the same seam).
- Port behavior gaps inherited from the baseline unchanged: a lone
  leaf's removal returns the leaf unchanged (single-cell spaces don't
  hit the delete arm), and the estimated-load bump belongs to the
  creation seam.

## Fixtures

`docs/review/fixtures/load-hotspot/cellappmgr-tuning.tsv` — 27 rows,
captured with the command above.
