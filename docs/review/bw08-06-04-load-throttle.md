# BW-08-06-04 — Emergency load throttle

**Date:** 2026-11-26
**Lane:** bw17-load (BW-08, `server-runtime`)
**Commit:** 083b99d

## Acceptance

> 慢加载及超预算 Tick 有可对照结果。

The same overloaded frames, measured against the two overload
gates, walk the throttle down in fixed 0.09 steps to the 0.1
floor — the witness budget (`maxPacketSize * throttle`) then sends
a tenth of the entities per update — and climb it back on good
ticks. Over-budget ticks (more than twice the tick period) log the
WARNING face, and the smoothed load trio carries the same samples.

## Reference anchors

| Face | Reference |
|------|-----------|
| `EmergencyThrottle::update` / `shouldScaleBack` / `scaleBack` / `scaleForward` / `estimatePersistentLoadTime` | `server/cellapp/emergency_throttle.cpp:29` / `:68` / `:79` / `:104` / `:124` |
| `ThrottleConfig` option defaults (0.1 / 0.001 / 1.0 / 1.0 / 0.1) | `server/cellapp/throttle_config.cpp` |
| `CellApp::updateLoad` / `addToLoad` / `checkTickWarnings` | `server/cellapp/cellapp.cpp:1270` / `:1258` / `:1167` |
| `numSecondsBehind` / `calcSpareTime` / `calcThrottledLoadTime` | `server/cellapp/cellapp.cpp:1240` / `:1144` / `:1156` |
| the witness budget the throttle scales | `server/cellapp/witness.cpp:1104` |

## Port seams

- `cellapp_throttle.rs` (new): the option-default consts
  [`THROTTLE_BEHIND_THRESHOLD`] through [`LOAD_SMOOTHING_BIAS`],
  the [`EmergencyThrottle`] state machine (value, the once-per-
  outage NOTICE latch, the start tick, the worst-lateness tracker,
  the ERROR/NOTICE notes), [`LoadSample`] (the frame's five
  samples) and the [`CellApp`] faces `update_load`, `add_to_load`,
  `check_tick_warnings`.
- `CellApp` gains `throttle`, `persistent_load`, `transient_load`,
  `total_load`, `total_added_load`, `tick_warnings` and the
  `load_sample` drain slot. `advance_tick` pumps `update_load` at
  its tail when a sample is present — the reference computes them
  off `mainDispatcher_` and the profiler scopes; the synthetic
  tick supplies the frame via `load_sample` (set by the profile,
  `None` holds the throttle).
- The value feeds the already-wired witness face
  `WitnessContext::throttle` (`cellapp_witness.rs:1556`), which the
  desired-packet-size math reads.
- The port's `emergency_throttle` f32 field is now refreshed by
  every `update_load`; the CRITICAL warning arms (the cellAppMgr
  timeout and the unsynchronised-TSC detection) are
  process-terminating faces without a port home and are documented
  out of scope.

## Quirks pinned

- **Two gates, two shapes**: `shouldScaleBack` is `behind > 0.1`
  OR `spare < 0.001` — lateness is strict `>`, starvation is
  strict `<`.
- **The float path is the reference's float path**: the exact-equality
  "fully scaled" tests can fire on one fraction-1.0 step (`1.0 - 0.9`
  is not the minimum; `0.1 + 0.9` is exactly `1.0`) and never on the
  asymptotic 0.09-step approach (`0.10000012 > min`). The tests pin
  both paths rather than paper over them.
- **The NOTICE fires once per outage**: the ERROR re-arms the latch
  each scale-back, so every outage logs its own NOTICE with the
  outage length and the worst lateness.
- **The estimate reads the pre-update throttle** — the reference
  computes `estimatePersistentLoadTime` before `throttle_.update`.
- **`start_tick` is a game-tick stamp**, so the outage length is
  `(current - start) * tick_period` — the `stampsToSeconds`
  equivalent on the port's clock.

## Tests

10 tests in `cellapp_throttle.rs` (TDD): the over-budget scale-back
step, the starvation gate, the full-back latch (ERROR once), the
recovery (NOTICE with outage length, re-arm behavior), the persistent
load unwind, the smoothing fold, the app-level warning + smoothed
loads, the overloaded tick's throttle + estimate + artificial load,
the one-sample-per-tick pump, and the slow-loading contrast
(ten steps down to a tenth of the witness budget, ten back up).

## Gates

- `cargo test -p server-runtime --lib`: 1056 passed (10 new).
- `cargo test --workspace`: green, 3408 passed.
- `cargo clippy -p server-runtime --all-targets -- -D warnings`: clean.
- `rustfmt --edition 2024 --check` on the touched leaf files: clean
  (scoped — a `lib.rs` invocation recurses into sibling modules
  carrying pre-existing fmt debt from other lanes).
- Coverage (llvm-cov 0.9.1, `cargo clean -p server-runtime` first):
  `cellapp_throttle.rs` **100.00%** lines (233/233).
