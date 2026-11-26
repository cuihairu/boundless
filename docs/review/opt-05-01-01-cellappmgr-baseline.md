# OPT-05-01 — CellAppMgr load and failure baseline

The section measures the `CellAppMgr`'s hot path (load balancing,
cell recycling, dead-CellApp detection) and then evaluates tuning
against that baseline. This row (OPT-05-01-01) establishes the
measurement harness and records the allocation, recycling and recovery
windows; it does not change any hot path.

## Harness

`crates/server-runtime/src/cellappmgr.rs` hosts the `CellAppMgr` load
balancer (`BW-05-04` port). The harness lives in-crate so it can reach
the private BSP internals (`BspInternal`, `Space.root`), declared as
`#[cfg(test)] #[path = "cellappmgr_baseline.rs"]` — the same module
idiom the crate's other fixture tests use. It runs three legs against
a synthetic clock, so every ladder is deterministic:

1. **Allocation** — a two-cell space absorbs a hotspot. The idle app
   B holds a zero-area sliver at `SPACE.ymax`; the hot app A's smoothed
   load ramps through `inform_of_load` (the `LOAD_SMOOTHING_BIAS` EMA
   = 0.05, the load-reporting face OPT-05-01-02 tunes), then balance
   passes move the partition until A's reported entity edge at
   `ENTITY_EDGE` binds. The EMA ladder is recorded per inform and the
   per-pass A/B ranges are pinned.
2. **Recycling** — `retire_all_cells_of` marks an app retiring, two
   arms: a zero-area retiring cell drains on the first pass (the
   `hasNoArea && retiring && no entities` delete arm), and a cell with
   real area is *held* at its post-absorption line because the
   survivor's loaded-chunk ghost margin (`GHOST_DISTANCE`) binds the
   reverse move.
3. **Recovery** — a CellApp stops reporting; `tick` detects it via the
   strict `now - lastHeard > timeout` check and `handle_cell_app_death`
   broadcasts the death and reports it to the BaseAppMgr. A two-cell
   space's dead-app cell is erased; the survivor's cell keeps the
   unbounded range; the last-app death triggers the controlled
   shutdown. The `ESTIMATED_INITIAL_CELL_LOAD` bump belongs to the
   cell-creation seam (`create_space` / `add_cell_auto` /
   `add_cell_for_app`), so it is recorded there.

Wall-clock numbers are **recorded, never asserted tight** (machine
dependent — the network and Python-bridge baseline discipline). The
record sink writes `metric<TAB>value` rows to stdout and, when
`CELLAPPMGR_BASELINE_OUT` is set, to that path:

```sh
CELLAPPMGR_BASELINE_OUT=<abs path> cargo test -p server-runtime --lib \
    cellappmgr_baseline -- --exact --nocapture
```

## Leg-by-leg findings

**Allocation.** With A's load at 0.9 (`LOAD_SMOOTHING_BIAS`-EMA from
0.05) and B idle, the load difference drives the line and the partition
converges to `B.ymin = 4000 = ENTITY_EDGE` on the first pass (a single
`do_balance` step with `aggression = 1.0`). The EMA ladder reaches a
final ratio 0.49; `alloc_final_ratio` records the converged point. This
is the face any later load-tuning (OPT-05-01-02/03) must not change.

**Recycling.** The sliver arm drains in one pass — `drain_passes == 1`,
one `CellRemoval`, the sibling promoted to `Rect::unbounded()`. The
real-area arm is held: with the growing child A below the 0.85 safety
bound, `should_move` is true and the load difference drives the line
back, but `chunk_limit_in_direction` reads A's `balance_chunk_bounds()`
(`SPACE` cannot contain A's desired inflated range because
`Space::load_balance` re-derives child ranges from the unbounded root,
so the fallback pins `chunk_limit` to `A.range.ymax - 500 = 3500`).
`closest_limit(Right)` then keeps the line at 4000. The records contrast
the immediate sliver drain with the held real-area window.

**Recovery.** Detection lands at `TICK_MS + TIMEOUT_MS + TICK_MS` —
the first tick past the strict `now - lastHeard > timeout` (A last
heard at 1000 ms). The death faces are the `CellAppDeath` broadcast and
the `ReportDeathToBaseAppMgr`; a two-cell space's dead-app cell is
erased and the survivor keeps the unbounded range. The estimate bump
(`ESTIMATED_INITIAL_CELL_LOAD`) is recorded on `create_space`'s cell
creation. A single-cell space is not covered on this path: `remove_cell`
on a lone leaf returns the leaf unchanged, so the dead app's cell is
the space's only cell and the handler leaves it for the manager's
`checkLoadingSpaces` to reclaim on a later tick.

## Record

`docs/review/fixtures/load-hotspot/cellappmgr-baseline.tsv` (56 rows)
is the rolling canonical record: the EMA ladder, the allocation pass
ranges, the recycle arms, the recovery detection window, the outbox
face counts, and the wall-clock windows (never asserted).
