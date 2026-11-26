# OPT-11-01-04 — Rerun the visual regression

Acceptance: **固定镜头满足容差** (the fixed camera satisfies the
tolerance).

`crates/client/tests/render_visual_rerun.rs` re-drives the OPT-11-01-01
fixed camera (640×480 RGBA8, 64 frames, three ends) through the shipped
faces and holds the rerun against the saved baseline record — the
frame digests at indices 1..=8 and 64, and the memory ledger (two
allocations, 1,228,848 bytes, peak equal).

## The tolerance

The measured per-pixel tolerance between walks is **zero**: at every
pinned frame, a fresh walk and its rerun differ in no pixel
(`differing_pixels = 0`, asserted), and the digest ledgers are equal —
so the deterministic face's tolerance floor is the bit and the rerun is
held bit-exact rather than within a budget. The same holds between the
shipped draw face and the OPT-11-01-02 batched candidate: every one of
the 64 batched frames answers the walked digest, and at the pinned
frames the batched buffer differs from the shipped device's back buffer
in no pixel.

## What the rerun covers

- **digests**: every pinned frame matches the saved record on all three
  ends, on the first walk and on its rerun;
- **memory**: the run ledger reruns to the saved numbers (2 allocations
  during the walk, peak 1,228,848 bytes — a frame still never raises
  it);
- **screenshots**: the `ScreenCopy::capture` → TGA channel (the
  BW-11-04-07 face) captures the pinned frames 1, 8 and 64; each TGA is
  the 18-byte header plus the 1,228,800-byte frame, the three ends'
  captures are byte-equal frame for frame, and a second pass reproduces
  them byte-identically — the visual-regression artifact itself reruns;
- **the candidate face**: the OPT-11-01-02 batched draw satisfies the
  same baseline, so the section's optimization changes no visual
  contract.

## Gates

- `cargo test -p client`: 861 lib passed, 4 + 2 + 3 + 2 + 2 + 2 + 2
  integration passed, 0 failed (2 new: the saved-baseline rerun with
  the screenshot face, the batched-face visual rerun).
- `cargo fmt -p client --check`: clean.
- `cargo clippy -p client --all-targets -- -D warnings`: no new lint.
