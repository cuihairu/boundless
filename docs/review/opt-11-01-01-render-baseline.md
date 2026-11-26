# OPT-11-01-01 — The three-end fixed-camera baseline

Acceptance: **保存帧时间和显存** (save the frame time and the video
memory).

`crates/client/tests/render_baseline_opt.rs` records the baseline the
rest of this section optimizes against: the three platform ends
(linux / windows / macos, the BW-11-04-07 comparability set) each walk
the identical fixed camera — `DeviceConfig::default()` (640×480 RGBA8,
adapter 0), 64 frames of `check_device` → `scene.step` → `present`
through the shipped device faces — and save the frame times, the frame
digests and the device's memory.

## The fixed camera

The presentation parameters never change and the camera contributes
nothing variable: frame N's image is a pure function of the frame
index. The test proves it two ways — a fresh device and scene stepped
directly to the index reproduces the walk's digest for that index
(indices 1, 7, 31, 64), and the whole digest ledger reruns identically
on a second walk. Consecutive frames always differ (the scene's shading
advances every step), so the animation is real under the frozen camera.

## The saved frame times (probe data, printed, never asserted)

| End | p50 / frame | p99 / max |
|---|---|---|
| linux | 18.94 ms | 26.21 ms |
| windows | 19.52 ms | 28.10 ms |
| macos | 18.77 ms | 23.12 ms |

Debug-profile wall-clock for one present (307,200 `set_pixel` writes +
an FNV digest over them). These are this machine's numbers — the face's
contract is the digest ledger, not the latency.

## The saved video memory (structural, asserted)

| Quantity | Value |
|---|---|
| device creation | **2 allocations, 1,228,848 bytes** — the 640×480×4 = 1,228,800-byte back buffer plus the 48-byte device-counter allocation |
| per-frame allocation | **0 bytes, 0 allocations** — present draws and digests the back buffer in place |
| whole-run peak (live high-water) | **1,228,848 bytes** — the creation footprint; a frame never raises it |

The frames-allocate-nothing property is the baseline's substance: the
device's working set is flat across the whole run, so any upload or
batching cost a later optimization introduces will show up as either a
per-frame allocation or a raised peak — the exact hooks OPT-11-01-02
evaluates.

## Structural assertions

- every end presents all 64 frames, in order, on one device object;
  no device leaks (`leaked() == 0`), the status stays `Ok`;
- the three ends' digest ledgers agree frame for frame, and their
  allocation ledgers agree too — the comparability the screenshot face
  established, per frame;
- the saved baseline pins the digests at indices 1..=8 and 64 plus the
  run memory ledger `(2 allocations, 1,228,848 bytes, peak =
  1,228,848)` — any raster, scene or device-memory change trips it;
  OPT-11-01-04's visual-regression rerun walks the same pins.

## Gates

- `cargo test -p client`: 861 lib passed, 4 + 2 + 3 + 2 integration
  passed, 0 failed (2 new: the three-end baseline record, the rerun
  ledger).
- `cargo fmt -p client --check`: clean.
- `cargo clippy -p client --all-targets -- -D warnings`: no new lint.
