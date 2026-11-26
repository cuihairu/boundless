# BW-10-07-04 — client-side map query

Acceptance: **固定射线及导航显示吻合** — the client query surface answers
against the loaded world's real navmesh geometry, and the fixed-map
truth replay pins the probe answers to the reference engine's recorded
responses.

## Scope

The client-facing query face for a loaded map. A client asks three
questions of the server's world navmesh:

1. **collide** — a ray/sweep against the loaded chunk grid;
2. **navigate** — the navmesh path between two points;
3. **map view** — the navmesh vertices as seen through the chunk/bsp
   seam planes.

All three answer through the same `world_navmesh` decode and the same
mounted-chunk grid, so the client's picture of the map matches the
server's.

## Ported surface

- `crates/client/src/navigation.rs` — the client query harness: a
  read-only view over the loaded world's sets, exposing collide (the
  chunk obstacle sweep + space-grid traversal), navigate (the smooth
  path), and the map-view projection (the navmesh vertices).
- `crates/chunk/tests/clientquery_collide.rs` — the collide leg against
  a real chunk grid.
- `crates/client/tests/genmap_clientview.rs` — the map-view leg: the
  fixed map's navmesh vertices replay the reference probe answers.

The path-quality face (`smooth_path`, `crates/waypoint/src/chunk_waypoint_state_path.rs`)
is the funnel port the navigate leg runs through.

## The funnel (`smoothPath`)

`chunk_waypoint_state_path.cpp:116-268` string-pulls the found walk
through the shared-edge corridor, then rebuilds each kept state through
the `NavLoc(guess, point)` constructor. The port carries the reference's
own quirks:

- **the +0.01 y lift** — the guess constructor lifts the rebuilt point
  by 0.01 (`navloc.cpp:97-151`).
- **the maxY chain** — the running `maxY` of the skipped states is
  carried across each segment, exactly as the reference threads it.
- **the clip chunk-bb fallback with the 0.99 nudge** — when a query
  point misses the loaded set's bounding box, the clip falls back to
  the chunk's bounding box nudged inward by 0.99.
- **the mounted-chunk bb seam** — the seam between mounted chunks keeps
  the per-chunk bounding box as the clip target.

### Dead-arm discipline

- **ON_LINE** — the reference's `processCorridorPoint` keeps an ON_LINE
  arm (`:398-411`, the closer of the two ends becomes the root), but
  `isPointInsideFunnel` (`:299-330`) never answers ON_LINE — its two
  line tests compare against LEFT/RIGHT only. The arm is dead in the
  reference as well and is not carried; the port's `OnLine` variant is
  an explicit `unreachable!` with a note.
- **the crossing arm's last-edge root** (`:560-565`) and the **last-edge
  skip** (`:628-629`) are the funnel's residual dead-arms under the
  fixed-map geometries; they are left unexercised on purpose rather than
  driven with a synthetic corridor that no real walk produces.

## Verification

- `cargo test -p waypoint` — 136 green, including the new white-box
  funnel tests (the abort, the corner pull, the root arms, the bad-root
  reset, the destination close, the full-path walk).
- `cargo test --workspace --no-fail-fast` — 118 green result lines, zero
  failures.
- `cargo fmt -p waypoint -- --check` — clean.
- `cargo clippy -p waypoint --all-targets -- -D warnings` — clean.
- `chunk_waypoint_state_path.rs` line coverage: **99.59%** (733/736),
  measured with `llvm-cov` 0.9.1 and the DA-max (SF,line) dedupe.

## Truth replay

The fixed-map probes (`crates/client/tests/genmap_clientview.rs`)
replay the reference engine's recorded answers point by point; the
world-navmesh decode restores the old graph exactly. The client query
surface answers against that same geometry, so the client's map view and
navigation match the server's.
