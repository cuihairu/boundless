# OPT-10-01-04 — Rerun the map results against the original fixtures

Acceptance: **路径点和碰撞结果满足原版夹具** (the path points and the
collision results satisfy the original fixtures).

`crates/client/tests/map_rerun_fixtures.rs` re-drives both original
fixture truths through the faces this section's optimization rides —
the rerun proof that the optimized configuration answers what the
reference answered.

## The path leg (the genmap two-chunk world)

The BW-10-07-03 world re-binds exactly as the truth log records
(the permissive seam portal pair, A first then B), and every XPATH
probe pair is walked through `Navigator::find_path` with
`ReadWriteCache` — the cached walk face the entity actually uses:

| Probe | walk |
|---|---|
| ACROSS (the hole detour) | source-side hops pinned on the truth chain |
| RAMP (ground to the ramp top) | source-side hops pinned on the truth chain |
| AROUND (chunk B south→north) | source-side hops pinned on the truth chain |
| SEAM (A west → B through the portal) | A-side [wp0, wp4, wp3] pinned; ends at the resolved B wp7 |
| SEAMBA (B → A, the wired landing) | ends at the resolved A wp3 |

Assertions: every source-set hop lies on the truth chain after its
head, in order — a subsequence (the saved way path's smoothing may
straighten intermediate states away, never a detour) — and every walk
ends on the truth-resolved destination waypoint. All five walks rerun
to identical waypoint sequences.

**Face findings while landing it:**

- the far-side hops are the walk face's own goal resolution —
  `for_destination` resolves the destination point through its own
  constructor and can land on a neighbour waypoint the raw A* chain
  never visits (SEAM's hop lands B-side off-chain before the walk
  finishes on B wp7). The source side is where the fixture's
  substance lives (the detour, the ramp, the hole), so that is what
  is pinned strictly;
- the TOUR pair is absent on purpose: the truth's `XPATHNONE` pins
  the one-shot chunk-level A* (replayed in BW-10-07-03), while the
  walk face crosses one portal per call by construction and is not
  that face — an entity walking hop by hop makes the tour legally.

## The collision leg (the chunk-space map through the cached stream)

The fixture map loads through the optimized streaming face — the
`BwResource` section cache serving `load_and_bind` (the OPT-10-01-02
candidate resolver) — the six files bind, the reference terrain slabs
register through the focussed gate, and the whole 100-probe grid is
answered through the client's `findDropPoint`:

- every drop lands on its slab at **10 m**, **40 m** down the ray
  (±1e-3), all 100 probes;
- the streamed map resolves each cell centre to its owning chunk
  (`0000`/`0001` grid ids);
- the second pass answers bit-identically (rerun identical:
  `RERUN-DROPS probes=100 rerun_identical=true`).

## Gates

- `cargo test -p client`: 861 lib passed, 4 + 2 + 3 + 0 integration
  passed, 0 failed (2 new: the path-fixture walks, the streamed-map
  drops).
- `cargo fmt -p client --check`: clean.
- `cargo clippy -p client --all-targets -- -D warnings`: no new lint.
