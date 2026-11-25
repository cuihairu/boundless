# BW-10-07-05 — the two-cell Bot crossing

Acceptance: **记录两 Cell 的移动和进入离开事件** — a Bot walks from one
cell into its neighbour; the recorder logs every tick's move with the
cell that owns it and the four cell-transition callbacks in the order
the reference engine fires them.

## Scope

One recorder, `crates/server-runtime/src/bot_crossing.rs`, drives the
whole crossing on the live ghost/offload surfaces:

1. **the walk** — a `MoveToPointController` registered as a real-only
   controller carries the Bot from the source cell to a point past the
   boundary (`movement.rs`);
2. **the offload decision** — `GhostMaintainer::check` with
   `should_offload = true` picks the destination cell once the Bot's
   rect ownership moves across the boundary (`ghost.rs`);
3. **the handover** — the source fires the leaving callbacks, the real
   converts to a ghost and streams its controller state; the ghost
   travels the channel, the destination fires its entering callbacks,
   and the streamed controller resumes the walk from the same state
   (`controllers.rs` `write_reals_to_stream` / `read_reals_from_stream`).

## The reference's four cell callbacks

The engine fires four script callbacks per crossing, in this order
(`cell.cpp:212`/`:225`, `entity.cpp:4693`/`:4791`, and the doc block at
`entity.cpp:841-920`):

| # | callback | where | cell |
|---|----------|-------|------|
| 1 | `onLeavingCell` | source real, before the offload | source |
| 2 | `onLeftCell` | source, after the real became a ghost | source |
| 3 | `onEnteringCell` | destination ghost, before `convertGhostToReal` | destination |
| 4 | `onEnteredCell` | destination real, high-priority buffered | destination |

The recorder reproduces exactly this order and appends the arrival's
`onMove` on the destination — one `onMove` per controller, no boundary
callback, matching the BW-10-06-14 migration precedent.

## The recorded trace

The fixed walk (97→108 at 5 m/s, 10 Hz, boundary at x = 100) records:

- moves on cell A for ticks 1–5 (97.0 → 99.5), then cell B from tick 6
  (100.0) through the arrival at tick 22;
- `crossing_tick = 6` — the first tick whose position the destination
  rect owns is the tick the maintainer offloads on;
- the five callbacks `[onLeavingCell@A, onLeftCell@A, onEnteringCell@B,
  onEnteredCell@B, onMove@B]` in firing order.

## Ported surface

- `crates/server-runtime/src/bot_crossing.rs` — the recorder: the
  two-cell world (`CellInfoRecord` rects split at `boundary_x`), the
  Bot (`EntityState` + `MoveToPointController`), the offload check
  (`GhostMaintainer`), the ghost handover (`convert_real_to_ghost`,
  channel `createGhost` delivery, `GhostHost::receive_onload`), and the
  resumed streamed controller.
- `crates/server-runtime/src/lib.rs` — declares `bot_crossing`.

## Verification

- `cargo test -p server-runtime` — 1169 green (1167 prior + 2 new
  recorder tests), 1 ignored.
- `cargo test --workspace --no-fail-fast` — 0 failures.
- `cargo fmt -p server-runtime -- --check` — clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings` — clean.
- `bot_crossing.rs` line coverage: **99.63%** (272/273), llvm-cov 0.9.1,
  DA-max (SF,line) dedupe. The single zero line (291) is the bare `}`
  join of the `if let GhostMessage::CreateGhost` — every queued message
  in the trace is a create, so the implicit else never runs; neighbours
  are all hit (lane artifact precedent).

## Notes

- The offload decision is the live maintainer's, not the recorder's:
  the destination is whatever `GhostMaintainer::check` picks once the
  position leaves the source rect.
- The controller the walk continues on is the one the source streamed
  through `convert_real_to_ghost` — the destination reads the real
  state back out of the arriving controller record, so metres-per-tick
  and the destination point survive the handover unchanged.
- Orphan modules (`offload.rs`, `controller_transfer.rs`) are not
  declared in this lane's `lib.rs` and are not used; every surface here
  is live and compiled.
