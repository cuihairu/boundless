# OPT-11-01-02 — Uploads and render batches, evaluated

Acceptance: **按端对比帧时间** (compare the frame time per end).

`crates/client/tests/render_batch_opt.rs` evaluates the frame's two
halves — the upload (draw) and the digest — through both faces over the
same fixed camera (640×480 RGBA8, 64 frames, scene state the only
variable):

- **shipped face**: `TestScene::draw`, one scalar `set_pixel` upload per
  pixel — 307,200 write calls a frame;
- **batched candidate**: the same pixel math through the shipped batch
  primitives — one whole-buffer `fill` for the background, then one
  slice fill per row over the marker cell's column block — **481 write
  calls a frame** (1 + 480).

Both faces time their draw and their digest separately; the frame
times are compared per end (linux / windows / macos, the BW-11-04-07
comparability set) and printed — probe data, never asserted.

## The per-end comparison (this tree, this runner, debug profile)

| End | shipped draw p50 | batched draw p50 | digest p50 (both faces) |
|---|---|---|---|
| linux | 15.50 ms | **2.10 ms** (7.4×) | 4.48 / 4.52 ms |
| windows | 15.05 ms | **1.97 ms** (7.6×) | 4.51 / 4.48 ms |
| macos | 17.36 ms | **2.08 ms** (8.3×) | 5.10 / 5.07 ms |

The batched upload cuts the draw half 7–8× on every end; the frame
time as a whole drops from ~20 ms to ~6.6 ms. The digest half is
face-independent (the same FNV over the same pixels) — its agreement
across the two faces within each end is itself a comparability check.
Wall-clock is this machine's data; the contract is the frames.

## Structural assertions

- **every batched frame is pixel-identical to the shipped frame** — the
  full `pixels` vector compared every frame, all 64 frames × 3 ends,
  plus digest equality;
- the shipped device face (`Device::present`, draw + digest through the
  real present path) produces the same digest walk the harness shipped
  face timed — the comparison rides the device face, not a stand-in;
- neither face allocates on any frame (0 allocations both), and the
  digest ledger reruns identically;
- the scene animates under the fixed camera (consecutive digests
  differ) and the three ends' ledgers agree frame for frame.

## The evaluation

The 307,200→481 write-shape change is where the whole draw-half win
lives; nothing else in the frame path moves. The batch face is a
candidate draw shape the scene could adopt — it changes no contract
(pixels identical), costs no memory (still zero allocation), and the
saved digests of OPT-11-01-01 remain the pin either face must answer.

## Gates

- `cargo test -p client`: 861 lib passed, 4 + 2 + 3 + 2 + 2 integration
  passed, 0 failed (2 new: the per-end batch comparison, the rerun
  ledger).
- `cargo fmt -p client --check`: clean.
- `cargo clippy -p client --all-targets -- -D warnings`: no new lint.
