# BW-08-05-05 — Cross-Space teleport

**Date:** 2026-11-25
**Lane:** bw17-load (BW-08, `server-runtime`)
**Commit:** 78c908c

## Acceptance

> 新旧 Space 的可见集吻合。

An observer in the old Space and an observer in the new Space report
the same visible set for the teleported real: `entities_visible_to_client`
before the teleport equals it after the teleport landed.

## Reference anchors

| Face | Reference |
|------|-----------|
| `RealEntity::teleport` (refuse, writeGhostData, onLeavingCell, offload) | `server/cellapp/real_entity.cpp:1183-1337` |
| `CellApp::onloadTeleportedEntity` (createGhost + onload + bounce) | `server/cellapp/cellapp.cpp:2130-2222` |
| `Space::createGhost` (ghost in the nearby's space) | `server/cellapp/space.cpp:228-295` |
| `EntityPopulation::rememberBaseChannel` | `server/cellapp/entity_population.cpp:136-144` |
| `writeGhostDataToStream` (id then body) | `server/cellapp/entity.cpp` |
| `CellAppInterface::onloadTeleportedEntity` wire | `server/cellapp/cellapp_interface.hpp:143` |

## Port seams

The port has no Mercury bundles or base channels, so the face is
expressed as:

- `TeleportWire { dest, src, nearby, ghost_data, onload }` — the
  teleport's head (the nearby id the ghost lands near and the ghost
  body) with the tail `OnloadWire` boxed inside, one wire on
  `CellApp::teleport_outbox`.
- `CellApp::teleport_entity(&mut spaces, entity, destination, nearby,
  position, direction) -> bool` — `RealEntity::teleport`: refusals
  (nearby `0`, a vehicle with passengers, a stranger, a ghost, a
  missing/destroyed nearby) touch nothing; the success path relocates
  via `relocate_entity_extras`, writes the teleport position/direction,
  deletes every ghost (one `GhostWire::Del` per haunt), resets
  `periods_without_witness`, re-haunts on the destination, writes the
  ghost body (`write_ghost_data_to_stream`), fires `onLeavingCell`,
  offloads with `is_teleport = true`, restores the old position/direction,
  and clones the tail `OnloadWire` into the `TeleportWire`.
- `CellApp::onload_teleported_entity(&mut spaces, wire) -> bool` —
  `onloadTeleportedEntity`: a missing/destroyed nearby records the
  ERROR face and bounces the `OnloadWire` back with `teleportFailure`
  set (`data[4] = 1`, `dest = src`); otherwise `create_ghost_message`
  in the nearby's space, `onload_entity`, and `on_teleport_success`
  with the nearby id.

## Quirks pinned

- **The destination bounces on failure.** When the nearby is gone the
  onload is sent back to the origin with `teleportFailure = true`; the
  origin's `onload_entity` re-lands the real and strips the failed
  haunt, then fires `onTeleportFailure` — a teleport never leaves a
  stale ghost behind because every ghost was deleted up front.
- **The ghost body starts at the id.** `writeGhostDataToStream` writes
  the teleporting id ahead of the body; `create_ghost_message` takes
  both as arguments, so `onload_teleported_entity` validates
  `ghost_data[..4] == entity` before stripping it.
- **`periods_without_witness` resets on teleport.** The reference
  clears `recordingSpaceEntryID_`-adjacent state; the port resets its
  stand-in so a fresh Space starts the witness clock at zero.
- **The position/direction write is only for the flight.** The old
  position/direction are saved and restored around the offload — the
  origin keeps its state intact while the wire carries the teleport
  position to the destination.
- **A vehicle with passengers refuses.** `passengers_state` containing
  the entity is the teleport-refusal face (the reference refuses a
  teleport while it carries passengers).

## Tests

`crates/server-runtime/src/cellapp_ghost.rs`:

- `a_cross_space_teleport_makes_the_visible_sets_agree` — the
  acceptance: baseline set in the old Space, teleport, land, observe
  from the new Space, sets equal.
- `a_teleport_refusal_touches_nothing` — nearby `0`, vehicle, stranger,
  ghost, unknown nearby, destroyed nearby: all refuse, outboxes stay
  empty, population untouched.
- `a_teleport_deletes_the_old_ghosts` — one `Del` per haunt before the
  flight; the haunts hold only the destination; the bounce path cannot
  resurrect the deleted ghost.
- `a_teleport_to_a_missing_nearby_bounces_the_onload` — the ERROR face,
  `teleportFailure` set, the origin re-lands the real and fires
  `onTeleportFailure`.
- `a_malformed_teleport_is_refused_at_each_face` — a body whose id does
  not match, an empty body, an unknown type, a truncated tail: each
  refused, no `onTeleportSuccess`.
- `on_teleport_success_falls_back` — the no-nearby arm of the success
  callback.

## Gates

- `cargo test -p server-runtime --lib`: 996 passed, 0 failed, 1 ignored.
- `cargo test --workspace`: all green (exit 0).
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: clean
  (the `asset-pipeline`/`blowfish` workspace-dependency warnings are
  pre-existing and unrelated).
- `rustfmt --edition 2024 --check` on the two touched leaf files: clean.
- Coverage (llvm-cov 0.9.1, `cargo clean -p server-runtime` first):
  `cellapp_ghost.rs` 99.11% lines, `cellapp_space.rs` 100%.
