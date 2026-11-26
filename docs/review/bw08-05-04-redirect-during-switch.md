# BW-08-05-04 — Redirect messages during the switch

**Date:** 2026-11-25
**Lane:** bw17-fire-ticket (BW-08, `server-runtime`)
**Commit:** PENDING_HASH (to be filled)

## Acceptance

> 调用到达迁移目标且响应一次。

A `callCellMethod` delivered to a CellApp whose real is mid-migration
is routed to the migration target and answered exactly once.

## Reference anchors

| Face | Reference |
|------|-----------|
| `EntityMessageHandler::handleMessage` | `server/cellapp/message_handlers.cpp:119` |
| Disposition ladder | `server/cellapp/entity_message_routing.hpp:24` |
| `Entity::forwardMessageToReal` | `server/cellapp/entity.cpp:2234` |
| `ReplyForwarder` (relay to the original sender) | `entity.cpp:2234` (the `ReplyForwarder` class) |
| `EntityPopulation::rememberRealChannel` / `forgetRealChannel` / `findRealChannel` / `expireRealChannels` | `entity_population.cpp:151` / `entity_population.hpp` |
| `Entity::destroy` (real branch forgets, ghost branch remembers) | `entity.cpp:2952` / `entity.cpp:2962` |
| `CellApp::handleTrimHistoriesTimeSlice` → `expireRealChannels` | `cellapp.cpp:1484` / `cellapp.cpp:1502` |
| `EntityMessageHandler::sendFailure` (`BWNoSuchEntityError`) | `message_handlers.cpp:263` |

The ladder for a `REAL_ONLY`/`WITNESS_ONLY` message:

```cpp
case DISPOSITION_FORWARD_TO_REAL:
{
    CellAppChannel * pChannel = pEntity ?
        pEntity->pRealChannel() :
        Entity::population().findRealChannel( entityID );

    if (pChannel)
        Entity::forwardMessageToReal( *pChannel, entityID,
            header.identifier, data, srcAddr, header.replyID );
    else
        this->sendFailure( srcAddr, header, data, entityID );
    break;
}
```

`forwardMessageToReal` wraps the bundle: a request (`replyID != NONE`)
is `startRequest`-ed with a `ReplyForwarder(srcAddr, replyID)` that
relays the destination's single reply back to the original sender.

## Port seams

The port has no `CellAppChannel`s (addresses are `u32`), no Mercury
bundles, and no reply channels, so the face is expressed as:

- `RealCallWire { dest, src, entity, method, reply_id }` — the
  `callCellMethod` body crossing to the app that has to route it.
- `CellApp::real_call_outbox: Vec<RealCallWire>` — the forwarded wires
  (`Entity::forwardMessageToReal`), the reply id riding along.
- `CellApp::curr_real_channels` / `prev_real_channels: BTreeMap<u32, u32>`
  — the population's two-generation real-channel cache.
- `CellApp::deliver_real_call(wire) -> DeliverRealCall` — the routing
  ladder run over a `REAL_ONLY` message:
  - `ForwardToReal` — one wire to the cached real, reply id preserved;
  - `Dispatched` — the local real ran the method (one answer on
    `reply_id`);
  - `Failed` — no channel: the single `sendFailure` answer.

The destroy hook (`entity.cpp:2952`/`2962`) lands in
`destroy_entity`: a real's destroy calls `forget_real_channel`, a
ghost's destroy calls `remember_real_channel` with its `real_addr`.
`advance_tick` drives `expire_real_channels` (the trim slice).

## Quirks pinned

- **One answer, not two.** The intermediate app forwards; the
  destination answers the original `src`/`reply_id` directly (the
  `ReplyForwarder` relay). The intermediate never answers, so the
  caller sees exactly one reply.
- **The cache is two generations.** An entry survives one
  `expireRealChannels` (current → previous) and drops on the next —
  `prev.clear()` before the swap, matching `entity_population.cpp:151`.
- **The failure face is one reply.** `sendFailure` fires only when the
  ladder found no channel and the message was a request; a non-request
  real call with no channel is silently dropped (the reference's
  `sendFailure` is a no-op without a reply id).
- **`real_channel_for` consults the entity first.** A live entity's
  `real_addr` wins; only a gone entity falls to the population cache
  (`findRealChannel`), mirroring the `pEntity ? pRealChannel() :
  findRealChannel` ternary.

## Tests

`crates/server-runtime/src/cellapp_ghost.rs`:

- `a_real_call_during_switch_reaches_the_target_and_answers_once` —
  the offload flight: the stranger's call rides `nextRealAddr_` to the
  destination, one wire, reply id `77` preserved; the destination
  dispatches once.
- `a_call_for_a_destroyed_ghost_follows_the_cached_channel` — the
  ghost's destroy leaves a cache entry; a late call follows it to the
  real.
- `a_call_for_an_unknown_entity_fails_once` — no channel, no entity:
  one failure reply, the ERROR note recorded, no wire sent.
- `a_call_for_the_local_real_runs_it_in_place` — the app holding the
  real dispatches locally, no forward.
- `the_real_channel_cache_rolls_through_generations` — the trim
  generation roll.
- `a_dead_real_channel_is_forgotten` — `handleCellAppDeath`'s face.
- `the_destroy_hook_forgets_or_remembers_the_channel` — the real
  destroy forgets, the ghost destroy remembers.

## Gates

- `cargo test -p server-runtime --lib`: 991 passed, 0 failed, 1 ignored.
- `cargo test --workspace`: all green.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: clean
  (the `asset-pipeline`/`blowfish` workspace-dependency warnings are
  pre-existing and unrelated).
- `rustfmt --edition 2024 --check` on the three touched leaf files:
  clean.
- Coverage (llvm-cov 0.9.1, `--lib`): `cellapp_ghost.rs` 99.27%,
  `cellapp_real_entity.rs` 99.93%, `cellapp_space.rs` 100%. The one
  remaining new-code zero line is the `DropNoWitness` match arm — a
  `WITNESS_ONLY` face unreachable from a `REAL_ONLY` message, the
  same exhaustiveness-arm category as the file's pre-existing
  `GhostSetReal::Missing` arm.
