# BW-08-06-01 — Proximity/vision triggers

**Date:** 2026-11-26
**Lane:** bw17-load (BW-08, `server-runtime`)
**Commit:** 1804bb9

## Acceptance

> 进出区域和遮挡回调吻合。

A trap fires `onEnterTrap`/`onLeaveTrap` as entities cross its bounds —
payload `(who, range, controllerID, userArg)`, enter requires the method,
the leaves tolerate its absence (`onLeaveTrapID` for a member gone from the
cell). A seer folds in-range membership from the same range-list crossings
and the periodic vision pass diffs `onStartSeeing`/`onStopSeeing` against
occlusion and the view cone; the restore faces replay both sets off the
offload stream.

## Reference anchors

| Face | Reference |
|------|-----------|
| `ProximityController::addProximity` (0/negative refused) | `server/cellapp/proximity_controller.cpp:142` |
| `ProximityController::startReal` (`OnloadHandler` scope vs standard) | `server/cellapp/proximity_controller.cpp:234` |
| `ProximityController::stopReal` (NoOpHandler + `setRange(0)`) | `server/cellapp/proximity_controller.cpp:281` |
| `ProximityController::writeRealToStream`/`readRealFromStream` | `server/cellapp/proximity_controller.cpp:302/357` |
| `ProximityRangeTrigger::standardEnter`/`standardLeave`/`nonstandardLeave` | `server/cellapp/proximity_controller.cpp:478/533/560` |
| `VisionController::startReal` (`setVisibleEntities` restore, incumbent ERROR) | `server/cellapp/vision_controller.cpp:95` |
| `VisionController::stopReal` (remove without contracting) | `server/cellapp/vision_controller.cpp:127` |
| `VisionController::update` (period tally) | `server/cellapp/vision_controller.cpp:158` |
| `VisionController::writeRealToStream`/`readRealFromStream` | `server/cellapp/vision_controller.cpp:185/210` |
| `VisionController::setVisionRange` (delete/create/resize) | `server/cellapp/vision_controller.cpp:245` |
| `EntityVision::updateVisibleEntities` (occlusion ladder) | `server/cellapp/entity_vision.cpp:258` |
| `EntityVision::canBeSeen(bool)` | `server/cellapp/entity_vision.cpp:905` |
| `VisibilityController` (New factory + start/stop ghost) | `server/cellapp/visibility_controller.cpp:80/90/100` |
| `Vector3::setPitchYaw` | `lib/math/vector3.ipp:520` |

## Port seams

- `crates/server-runtime/src/cellapp_vision.rs` (new): the trap/seer/
  visibility runtime. `TrapTriggerState { entity, trigger, range, user_arg,
  sink }` snapshots the immutable callback payload at attach (the reference
  reads the same `range_`/`userArg_` every time); the recorder sink holds
  crossings until the pump.
- `CellApp::trap_members` (cellapp_space.rs): the live trap membership
  ledger — the sink itself is drained by dispatch, so `trap_live_members`
  (the stream's `writeRealToStream` walk) reads the ledger, not the sink.
  `stop_proximity_controller` drops the entry; `reconcile_trap_onload`
  re-seeds it from the streamed set.
- `CellApp::entities_in_vision_range` = `entitiesInVisionRange_`; every
  trigger face (insert expansion, resize crossing, delete contract) folds
  through `fold_vision_membership`.
- `advance_tick` (`cellapp_real_entity.rs`) gained `dispatch_sensor_events()`
  after `run_movement_pass` — the port's sink pump standing in for the
  reference's synchronous in-shuffle callbacks.
- `update_movement_controller`'s dispatch (`cellapp_movement.rs`) routes the
  `Vision` kind into `update_vision_controller` ahead of the inert arm.
- Stream payloads (`cellapp_navigate.rs` `write_controller_payload` /
  `read_controller_payload`): Proximity = f32 range + u32 count + sorted ids
  (count>0 re-arms `pOnloadedSet_`); Vision = angle/range/ticks/period +
  count + ids — `seeingHeight_` is not streamed and restores to the
  constructor default; Visibility = f32 height (`DOMAIN_GHOST`).
- Offload (`cellapp_ghost.rs` `stop_reals_for_offload`) dispatches the sensor
  stop faces by kind before deregistering the updatable.

## Quirks pinned

- **Blocked replay does not retarget.** A collide `> 0` hits `continue` —
  the SAME full-height point replays; the 3/4-height retry arms only after
  an angle failure. A blocker covering the full-height ray keeps the target
  unseen even when the lower point would clear it.
- **`acos` is unclamped.** A zero-length sight vector normalises to NaN
  (the reference's divide-by-zero), and `NaN < visionAngle` is false — a
  co-located zero-height target is never seen.
- **The period tally starts at 0**, so `updatePeriod_ = 2` fires the pass on
  the third tick.
- **Trap stop contracts, vision cancel does not.** The trap's leaves drain
  into the NoOp swallow; vision's destructor removes without contracting and
  the visible set stays as it was ("the notification methods are not called
  when vision is cancelled").
- **Restore reconcile is silent for matches.** The OnloadHandler sweep's
  enters match the streamed set; strangers take the standard enter and
  unmatched members leave through `standardLeave`/`onLeaveTrapID` (bare id).
- **The restore re-seeds the membership ledger** from the streamed ids —
  the pre-offload ledger died with the trap.
- **`range <= 0` deletes the vision trigger with contracting**: membership
  folds down, no script callbacks.
- **The 1 mm range floor** (`trigger_set_range` clamps below 0.001).
- **The controller container's hard cap is 50** (`absolute_max_controllers`;
  25 is only the reference's warning) — every sensor factory answers
  "controller attach failed" past it.
- **`canBeSeen(bool)` is idempotent** and ghosts never reach it
  (MF_ASSERT → false); a second visibility controller is refused with the
  reference's exact message.
- **`setVisionRange`'s resize re-sorts the entity's trigger list**
  (delTrigger/addTrigger) after `setRange`, matching `setRangeAndMod`.

## Deliberately out of scope

- `ScanVisionController` (`addScanVision`): no script producer exists in
  this port's entity layer; noted for the SDK lane.

## Tests

26 tests in `cellapp_vision.rs` (TDD): trap enter/leave with the payload
ledger, zero/negative refusal, restore reconcile (matched set, strangers,
stale ids), cone + blocker pass, blocked-replay pin, three-quarter retry,
visibility gate, dead/vanished/degenerate members, period tally, cancel
silence, stream round-trips for both sensors, restore stranger
(`onStopSeeingID`), resize move, factory refusals, unspaced inertia, node-
gone stops, bogus-id dispatch, `canBeSeen` flip, destroyed-owner silence,
ghost resize refusal.

## Gates

- `cargo test -p server-runtime --lib`: 1026 passed (26 new).
- `cargo test --workspace`: green.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: clean.
- `rustfmt --edition 2024 --check` on the touched leaf files: clean (scoped;
  a `lib.rs` invocation recurses into sibling modules carrying pre-existing
  fmt debt from other lanes).
- Coverage (llvm-cov 0.9.1, `cargo clean -p server-runtime` first):
  `cellapp_vision.rs` **98.58%** lines / 100.00% functions. The remaining
  zero lines are the known if-let closing-brace artifact family.
