# BW-08-06-05 — Record replay events

**Date:** 2026-11-26
**Lane:** bw17-load (BW-08, `server-runtime`)
**Commit:** (feat — see todo.md)

## Acceptance

> Tick 边界与事件排序一致。

The tick boundary and the event order match the reference: the
initial snapshot rides the first tick flush, moved reals flush in
id-ascending order with the live positions read at the flush (not
at the move), each tick carries its own blocks, the FINAL block
lands on stop and the collector drops at the next flush.

## Reference anchors

| Face | Reference |
|------|-----------|
| `ReplayDataCollector` (per-recording state, data buffer) | `server/cellapp/replay_data_collector.hpp` / `.cpp` |
| `Cell::startRecording` (space snapshot + real create blocks + the declaring space-data entry) | `server/cellapp/cell.cpp:1011` |
| `Cell::tickRecording` (volatile flush, tick handoff, clear) | `server/cellapp/cell.cpp:1078` |
| `Cell::stopRecording` (FINAL block; stopped event only with callbacks) | `server/cellapp/cell.cpp:1144` |
| the game-tick tail flush (`cells_.tickRecordings()`) | `server/cellapp/cellapp.cpp:1432` |
| the volatile queue on real position internals | `server/cellapp/entity.cpp:3949` |
| block types 0–9 | `lib/connection/replay_data.hpp` |

## Port seams

- `cellapp_replay.rs` (new): `ReplayCollector` (name, AoI flag,
  declaring entry id, data buffer, the moved/recorded id sets, the
  stopping flag) and the block writers (`add_entity_state`,
  `add_volatile_data_for_queued_entities`, `delete_entity`,
  `add_space_data`, `add_finish`), plus the `CellApp` faces
  `start_recording` / `stop_recording` / `tick_recordings` /
  `queue_entity_volatile`.
- `CellApp` gains `replay_collectors` (space-keyed), the
  `recording_events` handoff (space, tick stamp, name, payload)
  and `recording_warnings`. `advance_tick` calls
  `tick_recordings()` at its tail — the `cells_.tickRecordings()`
  slot after load/boundary/backup.
- `entity_position_changed_internals` (`cellapp_passenger.rs`)
  queues the mover at the position-internals seam, real-only.
- Cell-only space-data keys
  (`SPACE_DATA_FIRST_CELL_ONLY_KEY` and up) stay out of the
  snapshot; the declaring entry rides `SPACE_DATA_RECORDING`.

## Quirks pinned

- **Ordering is BTreeSet-ascending**: the moved set flushes in id
  order and reads the *live* position at flush time — a mover that
  moved twice in a tick records once, at the latest spot.
- **On-ground vs airborne layouts**: the volatile block carries
  x/z + direction when on ground, the full xyz when airborne.
- **The create block is one-shot per recording** (`recorded_entities`
  stamp) — the reference's per-real `recordingSpaceEntryID_` stamp
  compressed to the collector; a second start of a new recording
  snapshots everyone again.
- **STOPPING_SHOULD_USE_CALLBACKS carries the stopped event**; the
  other stopping state does not — the stopped collector leaves at
  the flush that follows its FINAL block.
- **Ghosts never join the volatile set** (the is_real gate at the
  position-internals seam); vanished ids are skipped at the flush
  when the population already dropped them.
- **The initial snapshot keeps user keys only** and lists the
  cell's reals in list order; an entity still listed but missing
  from the population is skipped.

## Out of scope (documented)

- The client-property stream encoding for create blocks
  (entitydef type streams) — the port carries the reference's
  uncompressed create layout instead.
- AoI-event blocks (`should_record_aoi_events` is carried but the
  witness-side enter/leave feed has no port face yet).
- The TCP/file transport of finished recordings (BW-08-06-03's
  queries cover the wire bytes).

## Tests

17 tests in `cellapp_replay.rs` (TDD): the snapshot (space data +
real create blocks in list order), the second-start refusal, the
id-ordered volatile flush with live positions, the airborne
layout, the delete block + moved-set drop, per-tick isolation, the
stop (FINAL + dropped next tick), the no-callback stop, the
position-internals feed at the tick tail, the vanished skip, the
one-shot create stamp, the no-cell refusal, the cell-only key
filter, the non-initial start (no declaration), the unmounted
stop, ghost movers, and the population-missed space entity +
stranger queue.

## Gates

- `cargo test -p server-runtime --lib`: 1073 passed (17 new).
- `cargo test --workspace`: green, 82 suites, 0 failed.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: clean.
- `rustfmt --edition 2024 --check` on the touched leaf files: clean.
- Coverage (llvm-cov 0.9.1, `cargo clean -p server-runtime` first):
  `cellapp_replay.rs` **100.00%** lines (473/473).
