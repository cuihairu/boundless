# OPT-10-01-03 — Cached navigation and the collision tails

Acceptance: **报告缓存和尾延迟** (report the cache and the tail
latency).

`crates/client/tests/query_cache_opt.rs` replays two shipped query
faces over the reference fixture worlds:

- **navigation**: a six-set permissive waypoint chain (all outside
  chunks, so no shell-boundary bypasses the saved set path). The
  cached face is `Navigator::find_path` with `ReadWriteCache` riding
  the member `NavigatorCache` hop by hop; the recompute face is
  `navigatePathPoints` / `findFullPath`, which builds a scratch cache
  per call. Both start from the cached walk's own hops, so the routes
  are compared step for step.
- **collision**: `find_drop_point` (the shipped `findDropPoint` face)
  over the BW-10-07-04 clientquery world — two shell chunks, the two
  slats and the platform BSP fixtures — six RAY probes × 40 repeats.

## The saved report (this tree, this runner)

### Navigation cache

| Face | allocations | bytes | hop latencies |
|---|---|---|---|
| cached walk, fill hop | **47** | 3652 | 96.0 µs |
| cached walk, four hits | **8 each** | 984 each | 31.7 / 20.7 / 18.7 / 17.3 µs |
| cached total (5 hops) | **79** | 7416 | — |
| recompute walk, 5 steps | **281** (81/73/63/44/20) | 23628 | 179 / 136 / 147 / 77 / 26 µs |

The set-path size pops `[6, 5, 4, 3, 2]` as the cursor advances —
the hit arm of `matches` is exercised on every later hop, not the
full search. The cached walk does 3.6× less allocation work than the
recompute walk; wall-clock scales with it on this runner (the
per-hop numbers are probe data, printed, never asserted). The whole
ledger reruns exactly: `[47, 8, 8, 8, 8]` allocations on both runs,
the same answers and set sizes.

### Collision tails

| Probe | hit | p50 | p99 / max |
|---|---|---|---|
| GA | yes | 5.90 µs | 18.41 µs |
| GHOLED (hole under platform) | yes | 5.98 µs | 26.25 µs |
| GB | yes | 4.15 µs | 6.90 µs |
| GPLAT (platform top) | yes | 5.91 µs | 19.47 µs |
| GNONE (unfocussed corner) | no | 1.72 µs | 4.20 µs |
| GSEAM (boundary seam) | yes | 6.84 µs | 10.66 µs |

All 40 repeats per probe answer bit-identically (drop distance,
point and triangle, or a miss). The tail is the grid walk crossing
column boundaries and the hull-tree visit on the seam — the miss
never enters a column's obstacle tree, hence its much lower floor.
Wall-clock is probe data, printed, never asserted.

## Structural assertions

- each cached hop advances exactly one set; the last hop answers the
  destination itself (`findPath`'s "use dst as point" face);
- the recompute face's answer is the same remaining crossing
  sequence plus the hidden crossing into the destination set and the
  clipped terminus (`findFullPath`'s own contract), with the raw
  source prepended only when `almostEqual` does not suppress it;
- the cached walk is strictly cheaper in allocations than the
  recompute walk, and reruns to the identical ledger;
- every collision repeat reproduces the previous answer exactly.

**Face finding while landing it:** `findFullPath` clips the
destination and keeps the crossing into the destination set in its
route, while `findPath`'s final hop replaces the next with the raw
destination, hiding that crossing. The two faces' routes agree on
the shared crossings, not on the whole point list — the audit
compares the shared sequence and pins each face's own terminus
separately.

## Gates

- `cargo test -p client`: 861 lib passed, 4 + 3 + 0 integration
  passed, 0 failed (3 new: the cached/recompute route comparison,
  the walk rerun, the collision tails).
- `cargo fmt -p client --check`: clean.
- `cargo clippy -p client --all-targets -- -D warnings`: no new lint.
