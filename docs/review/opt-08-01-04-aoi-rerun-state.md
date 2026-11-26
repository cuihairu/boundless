# OPT-08-01-04 — Rerun the visible set, the event order and the migration

Acceptance: **状态正确** (the state is correct).

`crates/server-runtime/src/aoi_rerun.rs` reruns the dense scenario twice,
auditing every tick against a ground truth recomputed from the raw
positions. `TickLedger` now records the ordered seeing starts/stops,
the AoI enters/leaves and the whole client bundle in send order, so the
rerun compares **the event order**, not just counts.

## The saved audit

| Face | Value |
|---|---|
| audited ticks | 48 baseline + 16 migration (destination **and** native) |
| seeing starts / stops | 89 / 47 (ordered, diff-closed) |
| handover → seeded visible | 46 → 46 (silent) |
| re-announcements hitting a held id | **0** |
| final visible set | 42 ids — destination == native |

State assertions, per tick:

- the vision range population equals the shipped window predicate
  `(sub − range < x ≤ sub + range)` on both axes;
- the visible set equals the seeing pass's cone ground truth on the
  ticks the pass runs (the gate is read off the controller's public
  tick counter), and is exactly the carried set on the ticks it skips;
- the start/stop diff closes: starts only hit unseen ids, stops only
  held ones, and the diff reproduces `visible_entities`;
- the `EnterAoI` / `LeaveAoI` messages equal the trigger crossings, and
  the witness's AoI map equals the 500 m window;
- the bundle's per-entity order is `EnterAoI` → `CreateEntity` →
  `EntityUpdate` — no update precedes creation;
- after the migration the visible set, vision range population and
  AoI map all equal the native viewer's — the state, not just the
  event ledger.

**Face finding while landing it:** the client bundle is the sole
replay source for the client-facing AoI state — tick 0's 120
`EnterAoI` messages are the 120 trigger crossings, and replaying the
crossings a second time double-counts them. The audit now checks the
bundle against the crossings; the two views must agree, not compound.

## Gates

- `cargo test -p server-runtime --lib`: 1756 passed, 0 failed (2
  new: the event-order rerun, the migrated-state correctness).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no
  new lint.
