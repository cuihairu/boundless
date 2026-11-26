# BW-08-05-06 — Migration target lost and recovered

**Date:** 2026-11-25
**Lane:** bw17-load (BW-08, `server-runtime`)
**Commit:** d78aa36

## Acceptance

> 角色及消息状态对照通过。

Across the death sweep the roles (real / ghost / recovered ghost /
zombie) and the message states (what buffers, what flows, which
wires go out) read back against the reference ladder.

## Reference anchors

| Face | Reference |
|------|-----------|
| `CellApp::handleCellAppDeath` (haunt strip, zombie sweep, emergency asks) | `server/cellapp/cellapp.cpp:1765-1878` |
| `Entity::checkIfZombied` (zombie / healthy / recover ladder) | `server/cellapp/entity.cpp:3309-3344` |
| `AckCellAppDeathHelper` (`shouldOffload(false)`, criticals) | `server/cellapp/ack_cell_app_death_helper.cpp:38-45` |
| `EntityPopulation::handleCellAppDeath` (both generations forget) | `server/cellapp/entity_population.cpp:205-213` |
| `BaseAppIntInterface::emergencySetCurrentCell` (space id + realAddr) | `cellapp.cpp:1854-1858` |
| `Entity::shouldBufferMessagesFrom` | `server/cellapp/entity.cpp:7849` |

## Port seams

- `CellApp::check_if_zombied(entity, dying_addr) -> bool` —
  `checkIfZombied` verbatim: destroyed or real → false; a
  `next_real_addr` that is the dying addr (or
  `hasRecentlyDied`-marked) → zombied; a healthy current real →
  false; otherwise the recovery arm promotes the next real to
  current and survives.
- `CellApp::handle_cell_app_death(spaces, dead_addr) -> Vec<u32>` —
  the death sweep: `mark_app_dead` (`hasRecentlyDied`), the cells'
  `should_offload` freeze (`AckCellAppDeathHelper`'s constructor
  face), the population listener (`forget_dead_real_channel` on both
  generations), every real strips its haunts on the dead app, every
  ghost runs the zombied check, and each zombie sends one
  `EmergencySetCurrentCellWire { base, entity, space_id, real_addr }`
  before it is destroyed.
- `CellApp::dead_apps: BTreeSet<u32>` and
  `CellApp::emergency_outbox` (cellapp_space.rs).

## Quirks pinned

- **The recovery call is the current real's address.** Calling the
  check with the current real's own address is the recovery path, not
  a health check: `addr != dyingAddr` fails, so the next real is
  promoted. A health check needs an address the ghost has never
  heard of.
- **A zombie's wire names the old real.** The base's
  `emergencySetCurrentCell` carries `realAddr()` as read after the
  check — a zombie never recovered, so the answer comes back to where
  the real was when the flight died.
- **The recovery keeps `nextRealAddr_`.** Only the current real is
  promoted; the sign-off stays until the new real's first message
  clears it (`entity.cpp:4950`'s arm), so `shouldBufferMessagesFrom`
  keeps comparing against the promoted address — the dead real's
  stragglers buffer, the promoted real's messages flow.
- **A base-less zombie dies unwired.** `hasBase()` gates the
  emergency ask; the sweep destroys it all the same.
- **Offloads freeze on death.** Every cell's `should_offload` drops
  until the app is in a good state again — the sweep is not just an
  entity pass.

## Tests

`crates/server-runtime/src/cellapp_ghost.rs`:

- `a_death_zombies_the_ghost_of_the_dying_real` — the zombie arm:
  destroyed, one emergency wire (base, space, dying real), a
  re-check answers no, and the base-less stray dies unwired.
- `a_death_recovers_a_ghost_whose_next_real_survived` — the
  recovery arm: the promotion, the surviving role, and the message
  states (dead real buffers, promoted real flows,
  `entity_real_addr` answers the promotion).
- `a_death_zombies_a_ghost_whose_next_real_is_the_target` — the
  migration target's own death: zombied despite the healthy current
  real, the wire names the old real, and `hasRecentlyDied` zombiest
  a later check.
- `the_death_sweep_strips_haunts_and_freezes_offload` — haunts
  pruned to the live apps, the cells frozen, the population cache
  forgetting the dead app's entry from its generation while the live
  one stays.

## Gates

- `cargo test -p server-runtime --lib`: 1000 passed, 0 failed,
  1 ignored.
- `cargo test --workspace`: all green (exit 0).
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: clean.
- `rustfmt --edition 2024 --check` on the two touched leaf files:
  clean.
- Coverage (llvm-cov 0.9.1, `cargo clean -p server-runtime` first):
  `cellapp_ghost.rs` 99.18% lines (no new misses), `cellapp_space.rs`
  100%.
