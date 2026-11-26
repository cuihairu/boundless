# OPT-08-01-02 — The AOI index and Witness scheduling candidates

The acceptance reports **candidate gains** (`报告候选收益`). Three
optimization candidates were evaluated against the OPT-08-01-01
baseline, each emulated on the live faces — the `RangeList` index with
`Recorder` triggers, the `Witness` scheduler under its packet budget —
without touching production behavior.

## C1 — Move-threshold shuffle elision (index)

The trigger only detects a crossing when its node shuffles, so shuffling
every `k` ticks instead of every tick cuts the calls `k`-fold. The
ladder on the d400 block (all strides walk 176 ticks; the lagged leaves
still fire):

| Stride | Shuffle calls | Shuffle ns (total) | Saved | Max detection lag | Events |
| --- | --- | --- | --- | --- | --- |
| 1 (baseline) | 176 | 862 297 | — | 0 | 800 |
| 2 | 90 | 963 698 | −101 401 | 1 tick | 800 |
| 4 | 48 | 1 036 644 | −174 347 | 3 ticks | 800 |

**Verdict: the call count drops 73% (176 → 48), but the wall time does
not shrink — the total shuffle nanoseconds come out flat to slightly
worse.** The index cost is proportional to the distance moved per
shuffle, not to the number of shuffles: a stride-4 shuffle moves four
times the distance of a stride-1 shuffle, and the whole walk covers the
same 1 400 units either way. The candidate's real value is elsewhere —
the per-call bookkeeping (the expand/contract passes over both
triggers) amortizes into fewer, larger moves — and its price is the
deferred detection: a crossing is seen up to `k` ticks late (measured:
1 tick at k=2, 3 at k=4). Not worth taking on this index.

## C2 — The AoI exit hysteresis (index + witness)

The reference enters at the AoI radius and leaves at radius + the
hysteresis (`Witness::setAoIRadius`'s `hyst`). The port keeps
`aoi_hyst` as bookkeeping; the candidate policy is emulated with two
live triggers — enter from the radius trigger, leave from the
radius+hyst trigger (`HYST = 50`).

**The clean-pass cost** (a monotonic walk, both forms on one walk):

| Face | Value |
| --- | --- |
| Events under both forms | 800 = 800 — identical |
| Leave-detection lag | 6–7 ticks (the 50-unit band at 8 units/tick) |
| Enter lag | 0 — the policy never delays an enter |

**The flutter gain** (an observer oscillating ±25 at the block's edge,
10 periods):

| Face | Value |
| --- | --- |
| Plain form (hyst 0) | 1 000 events = 500 enter + 500 leave |
| Candidate policy | 800 events = 400 + 400 — **the same clean-pass totals** |
| Suppressed | 200 churn events, 20 entities re-admitted |
| Admission handshakes saved | 300 messages (enter+create per re-admit, leaveAoI) |

**Verdict: the hysteresis pays exactly where it is aimed — boundary
flutter — and only there.** A clean pass fires the identical event
stream; a fluttering observer stops re-admitting the entities it has
not actually left (each suppressed re-admit is a 300-message handshake
saved per 400-entity window). The price is a 6–7 tick delay in
leave-detection, bounded by the hysteresis band.

## C3 — Skip the no-change update frames (witness)

`update()` pushes an `EntityUpdate` for every popped queue entry even
when the volatile gate produced no position, no direction and no
history events — a stationary population spends its budget on id-only
frames. The witness replay classifies every post-update frame:

| Face | Value |
| --- | --- |
| Update frames | 12 094 |
| Id-only frames (pos/dir/events all empty) | **11 519 = 95.25%** |
| Update bytes | 29 474 |
| Bytes on id-only frames | **21 239 = 72.06%** |
| Player-position floor | 175 messages (1/tick, never suppressible) |
| Worst-tick reclaim | 412 bytes of the 597-byte budget |

**Verdict: this is the largest single gain of the three — but it is a
scheduler-side change, not an index one.** Nearly every frame the dense
AoI's update pass sends is an id-only frame; skipping them would hand
72% of the update budget back to real movers and shrink the 12 094-frame
traffic by 95%, while the player's own projection (the floor that rides
the budget) stays untouched. The frames are also what carries the
`lastVolatileUpdateNumber` stamp — the skip must keep the stamping
semantics so a genuinely moving entity is not mistaken for a quiet one.

## Cross-face pins

- The C2 clean-pass ledger digest (`49bf554c6f1c96b7`) equals the
  OPT-08-01-01 d400 index digest — the hysteresis form fires the
  baseline's exact event stream.
- The C3 witness replay digest (`9a9a7c6478fc6e3d`) equals the
  OPT-08-01-01 witness digest — the classification changed no behavior.
- The C1 stride-1 digest also matches — the ladder is the same walk.

## Harness

`crates/server-runtime/tests/aoi_candidates.rs` — the three
evaluations, one deterministic run.

```sh
AOI_CANDIDATES_OUT=<abs path> cargo test -p server-runtime \
    --test aoi_candidates -- --nocapture
```

## Fixtures

`docs/review/fixtures/aoi-candidates/aoi-candidates.tsv` — 44 rows,
captured with the command above.
