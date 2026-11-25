# BW-08-06-02 — Noise sensing

**Date:** 2026-11-26
**Lane:** bw17-load (BW-08, `server-runtime`)
**Commit:** 9ab4b5f

## Acceptance

> 不同半径与距离的触发对照。

The maker's level scales the radius off `NoiseConfig::standardRange`
(10 m per level), the two-way x-chain walk collects the candidates with
the `<=` squared gate, and the strict `dist < range` gate decides the
deliveries — a hearer standing exactly on the radius is visited but
never hears, and the diagonal inside x but outside 3D stays out. The
payload rides along: `onNoise(who, propRange, distance, event, info)`.

## Reference anchors

| Face | Reference |
|------|-----------|
| `Entity::makeNoise` (real-only, level × standardRange) | `server/cellapp/entity.cpp:7329` |
| `Entity::outdoorPropagateNoise` (visitor walk, strict `<`) | `server/cellapp/entity.cpp:7273` |
| `Entity::getEntitiesInRange` (two-way x walk, `<=` squared gate) | `server/cellapp/entity.cpp:6913` |
| `Entity::heardNoise` (`onNoise` payload) | `server/cellapp/entity.cpp:7373` |
| `NoiseConfig` option defaults (10.0 / 1e6 / squared in `postInit`) | `server/cellapp/noise_config.cpp` |
| `Witness::newPosition` (stealth `makeNoise( stealthFactor_, 0 )`) | `server/cellapp/witness.cpp:2371` (the call at `:2398`) |

## Port seams

- `crates/server-runtime/src/cellapp_noise.rs` (new): [`NOISE_STANDARD_RANGE`],
  [`NOISE_VERTICAL_SPEED`], [`NOISE_HORIZONTAL_SPEED`] carry the option
  defaults; `NoiseCall` records each delivery — the payload tuple the
  script call alone cannot carry.
- `CellApp::make_noise` (`entity.cpp:7329`): real-only (`isRealToScript`
  is plain `isReal` here — the port carries no
  `treatAllOtherEntitiesAsGhosts`), then the outdoor face is the
  propagation body. The distances are read off the live positions
  before any callback runs — the same pump convention the traps use.
- `RangeList::entities_in_x_range` (`cellapp_range_list.rs`): the walk
  itself — starts at the caller's neighbours (the maker never hears
  itself), bound nodes pass through without advancing the walk-out
  test, and the script variant's type filter is not carried (the noise
  walk runs unfiltered).
- `CellApp::dispatch_witness_noises`: the movement-noise producer.
  `Witness::newPosition`'s `makeNoise( stealthFactor_, 0 )` was
  synchronous; the port's witness records `(level, event)` pairs and
  this pump delivers them, wired into `advance_tick` after the sensor
  pump.
- The maker without a range node (the reference's `pChunk_ == NULL`)
  answers true and carries nothing; a candidate gone from the
  population is passed over.

## Quirks pinned

- **Two gates, two shapes.** The walk's `<=` squared gate uses
  `globalPosition_`, the delivery's strict `<` uses the local
  `position()` — equal for unmounted entities, and the port keeps both
  off `position`. A hearer exactly on the radius is visited but silent.
- **The maker never hears itself** — the walk starts at its node's
  neighbours on both sides.
- **A level of zero reaches nobody**, even co-located hearers
  (`0 < 0` is false).
- **`onNoise` is `okIfFnNull`**: a hearer without the method stays
  quiet, no warning.
- **Ghosts and strangers answer false** (`This method can only be
  called on a real entity.`) — nothing propagates, nobody hears.
- **The stealth factor rides the same math**: the witness pump's level
  scales the radius identically, event 0, info 0.

## Deliberately out of scope

- The indoor face: `CellChunk::propagateNoise`'s portal traversal with
  the accumulated `travelDist` payload (`cell_chunk.cpp:119`) needs the
  chunk lane's entity-in-chunk membership and portal graph; the port's
  entities sit in outdoor chunks, where `makeNoise` calls
  `outdoorPropagateNoise` only.

## Tests

9 tests in `cellapp_noise.rs` (TDD): the radius/distance contrast (the
acceptance), level scaling both ways, the strict boundary, zero level,
the maker's own deafness, ghost/stranger refusal, the missing-method
silence, the unspaced/no-list/vanished-candidate trio, and the witness
pump's stealth delivery.

## Gates

- `cargo test -p server-runtime --lib`: 1035 passed (9 new).
- `cargo test --workspace`: green, 3502 passed.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: clean.
- `rustfmt --edition 2024 --check` on the touched leaf files: clean
  (scoped — a `lib.rs` invocation recurses into sibling modules
  carrying pre-existing fmt debt from other lanes).
- Coverage (llvm-cov 0.9.1, `cargo clean -p server-runtime` first):
  `cellapp_noise.rs` **100.00%** lines; `cellapp_range_list.rs` 100.00%
  including the new walk.
