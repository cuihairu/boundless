# OPT-08-01-02 — AOI indexing and witness scheduling, evaluated

Acceptance: **报告候选收益** (report the candidate gains).

Two comparisons over the dense baseline's own load (OPT-08-01-01's
seed, schedule and 120-entity field, the shipped faces end to end),
in `crates/server-runtime/src/aoi_tuning.rs`:

## 1. The sector-prefilter index

Before the shipped seeing pass runs, drop targets whose **horizontal**
angle from facing exceeds `vision_angle + 0.25 rad`. With a horizontal
facing, the pass's 3D cone angle is never larger than the horizontal
angle (the elevation term only shrinks it), so the prefilter is
conservative — it can skip work but never change a verdict.

| Measure | Shipped | Candidate |
|---|---|---|
| target rows walked over 48 ticks | 5760 (120 × 48) | **2471** |
| skipped by the sector test | — | **57.1%** |
| visible-set mismatches | — | **0** (48/48 ticks identical) |
| seeing-start mismatches | — | **0** |
| probe wall-clock (seeing passes) | 2.645 ms | 1.917 ms |

The candidate gain is real but bounded: the range list already indexes
*distance* (the pass only walks entities in vision range), so the
sector index only removes the *back sectors* — 57% of the remaining
rows on this fixture, at the cost of one `acos` per row before the
pass. The equality is asserted tick for tick; the wall-clock is probe
data, never asserted.

## 2. The witness scheduling sweep

The scheduler's operator face is the packet budget
(`setWitnessCapacity`). Swept at 1×/2×/4× the dense baseline's
64000 bps (772 / 1572 / 3172 B per tick):

| Capacity | Coverage (ticks to serve all 120) | Steady volume (16 ticks after coverage) |
|---|---|---|
| 64000 bps (772 B) | 3 | 1859 B ≈ 116 B/tick |
| 128000 bps (1572 B) | 2 | 1920 B ≈ 120 B/tick |
| 256000 bps (3172 B) | 2 | 1863 B ≈ 116 B/tick |

**The candidate gain direction flips with the phase.** During the
creation/scheduling ramp the budget binds (deficit payback from the
2704-byte burst), so quadrupling it cut coverage 3→2 ticks. In steady
state the volume is **capacity-insensitive**: `MAX_PRIORITY_DELTA`'s
window `[front, front+5.0)` empties long before a 772-byte budget is
spent (~116 of ~772 bytes/tick), so more bandwidth buys nothing —
the window width, not the link, is the steady-state limiter. Both
capacities cover the whole field (the window starves nobody), asserted
monotone; per-entity update ids come off the bundles'
`EntityUpdate` messages.

## Gates

- `cargo test -p server-runtime --lib`: 1750 passed, 0 failed (2 new:
  the index equality, the sweep monotonicity).
- `cargo fmt -p server-runtime --check`: clean.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: no
  new lint.
