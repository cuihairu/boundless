# OPT-08-01-01 — The dense-AoI baseline: tick, broadcast volume, migration

Acceptance: **保存 Tick、广播量和迁移耗时** (save the tick, the
broadcast volume and the migration duration).

`crates/server-runtime/src/aoi_baseline.rs` wires the whole shipped
stack into one dense scenario — one viewer at the centre of a 10×12
grid of 120 entities at 4 m spacing, everything inside one AoI — and
runs it tick by tick through the real faces:

- the cross-range list shuffles every moved entity
  (`RangeListWorld::move_entity`, deterministic ±0.75 m steps from the
  ledger seed),
- the viewer's AoI trigger crossings feed the witness
  (`add_to_aoi`/`remove_from_aoi` → `flush_state_changes` → EnterAoI →
  the client's `request_entity_update` reply → CreateEntity),
- the vision trigger crossings feed the seeing pass
  (`VisionWorld::vision_update`, 1.2 rad cone, 28 m range, seen every
  tick, clear terrain),
- and the budget scheduler sends what the packet allows
  (`Witness::update` at `setWitnessCapacity(64000, 10)` — 772 payload
  bytes per tick, which **binds** at this density).

## The saved baseline (this tree, this runner)

| Face | Value |
|---|---|
| ticks | 48 + 16 after the migration |
| tick 0 (creation burst) | 289 messages, 2704 bytes — 120 EnterAoI + 120 CreateEntity + the first scheduled updates |
| tick 1 (first seeing burst) | 44 seeing starts |
| steady window (ticks 8..48) | ~114–120 messages/tick, ~116 bytes/tick, **4654 bytes total** |
| broadcast total | 5708 messages, 9188 bytes over 48 ticks |
| AoI crossings | 120 enters (the initial square-window population), 0 leaves |
| seeing churn | 77 starts / 31 stops — the cone filters the 28 m circle |
| migration | handover carried 46 visible ids, the destination seeded exactly those 46 silently, re-announced 12 genuinely new ids, **0 duplicates**, converged to the native viewer's set in **2 ticks** (42 ids) |

The assertions are the structural ledger, and they reproduce anywhere:
the burst is one-time (no creations or entries after tick 8), every
steady tick respects the packet budget and always sends (the budget
binds), the migration seeds exactly the handover's set without a
single callback, no start ever hits an id the client already holds
(the diff contract), and the migrated viewer's visible set converges
to a viewer that had always stood at the new position. The whole
ledger reruns identical, tick for tick. Wall-clock probes (seeing
passes 4.0 ms, witness scheduling 392 ms over 64 ticks) are printed,
never asserted.

**Face findings while landing it:** the destination's vision trigger
must be built by `vision_start_real`, not pre-built — a pre-existing
trigger turns the restore's `set_vision(None→Some)` reset into the
clearing path and the destination's `entities_in_vision_range` stays
empty; the seed's visible set then walks into the seeing pass blind.
What a migrating entity does — `startReal` builds the trigger fresh —
is also what keeps the range population correct.

## Gates

- `cargo test -p server-runtime --lib`: 1748 passed, 0 failed
  (3 new: the saved baseline, the rerun identity, the silent seed).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no
  new lint.
