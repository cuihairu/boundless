# BW-08-06-03 — Cell Viewer queries

**Date:** 2026-11-26
**Lane:** bw17-load (BW-08, `server-runtime`)
**Commit:** 1a0a648

## Acceptance

> 与实时 Space/Cell 状态匹配。

Every reply is read off the live `CellApp` state at handle time: the
entity listings walk the space's real/ghost membership, the grid reply
reads the mounted physical backend's bounds, and the type-name reply
lists the registered `entity_scripts`. The tests assert wire bytes
against the same fixture entities they created.

## Reference anchors

| Face | Reference |
|------|-----------|
| `CellViewerConnection::handleInputNotification` (one ASCII command per request) | `server/cellapp/cell_viewer_connection.cpp:56` |
| `CellViewerConnection::receiveData` (`Received wrong number of bytes.`) | `server/cellapp/cell_viewer_connection.cpp:313` |
| `sendReply` (length-prefixed) / `sendReply_brief` (raw) | `server/cellapp/cell_viewer_connection.cpp:338` / `:354` |
| `'n'` GET_ENTITY_TYPE_NAMES (`numTypes` + len-prefixed names) | `cell_viewer_connection.cpp` |
| `'b'` GET_ENTITIES (`which` selector + `spaceID`) | `cell_viewer_connection.cpp` |
| `'e'` GET_GRID (bbox + grid size, disabled grid-positions `#if 0`) | `cell_viewer_connection.cpp` |
| TCP listener / connection lifecycle | `server/cellapp/cell_viewer_server.cpp` |

## Port seams

- `CellApp::handle_viewer_input(&[u8]) -> Option<Vec<u8>>`: the
  connection's data pump minus the socket. `None` closes (empty
  input); `Some` is exactly one reply frame. The listener, poll group
  and connection table stay in `cell_viewer_server.cpp` — transport
  out of scope.
- Short reads push the reference's warning text and read zeroes, so a
  truncated `'b'` still walks the ghost selector + space lookup and
  answers the `-1` frame — the reference's `receiveData` semantics.
- Unknown commands drain the input and answer a bare `u32 0` length
  prefix; the space/cell misses push `"space not found."` /
  `"cell not found."` / `"space == NULL"` / `"pCell == NULL"` before
  their frames, matching the reference's `WARNING_MSG` texts.
- `Entity` listing records are 14 bytes: `f32 x, f32 z, u16 type_id,
  u32 id` — the reference packs floats then the type/id pair.
- The grid reply is `brief u32 1` + 6 f32 bbox + grid size + `u32 0`
  (the loaded-grid-positions block is `#if 0` in the reference).
  `sub_bounds`/`set_grid_size` on `PhysicalSpace` carry the backend
  split: chunk → grid size slot, delegate → `min(width, depth)` with
  the non-square warning.
- The entities selector is one byte: `'0'` walks real entities,
  anything else takes the ghost arm.

## Quirks pinned

- **Empty input deletes the connection** — `handle_viewer_input`
  answers `None` and the test asserts it.
- **Short reads read zeroes, not errors**: both warnings fire and the
  frame still shapes itself as a normal ghost query on space 0.
- **The grid fallback warns once per non-square answer**;
  square delegate bounds answer silently.
- **`set_grid_size` on a delegate is a no-op** — only the chunk
  backend owns a grid slot.

## Deliberately out of scope

- The TCP transport: `CellViewerServer`'s listener, its poll group and
  the `Mercury`-adjacent connection table. The port's deliverable is
  the query semantics and the exact wire bytes.

## Tests

12 tests in `cellapp_viewer.rs` (TDD): the type-name listing, the real
and ghost listings against the live space, the `-1` frame on missing
space/cell, the chunk grid reply, the delegate fallback (non-square
warn + square silent), the `0` frame on missing space/cell, the
unknown command drain, the empty-input close, and the two short-read
shapes (selector present, bare command).

## Gates

- `cargo test -p server-runtime --lib`: 1046 passed (11 new).
- `cargo test --workspace`: green, 3513 passed.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: clean.
- `rustfmt --edition 2024 --check` on the touched leaf files: clean
  (scoped — a `lib.rs` invocation recurses into sibling modules
  carrying pre-existing fmt debt from other lanes).
- Coverage (llvm-cov 0.9.1, `cargo clean -p server-runtime` first):
  `cellapp_viewer.rs` **100.00%** lines (319/319); the new
  `PhysicalSpace` helpers in `cellapp_space.rs` fully covered (the
  module's two remaining DA:0 lines, 625/637, sit in the pre-existing
  vision blocker occlusion loop, untouched here).
