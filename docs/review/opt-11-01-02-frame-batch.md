# OPT-11-01-02 — The upload and render-batching candidates

The acceptance: **frame time compared per end** (`按端对比帧时间`). The
baseline's draw walks every pixel through `Framebuffer::set_pixel` —
one bounds-check and index computation per pixel, `width × height`
upload calls per frame. The frame itself is at most two vertical bands
(the background, the marker cell's column band), so the candidates
restructure only how the pixels reach the buffer, and every form must
reproduce the frozen camera's anchor digest byte for byte.

## The candidates

- **A — the row-contiguous upload.** Per row: fill the row's contiguous
  slice with the background, then the band's slice with the marker
  colour — `2 × height` slice uploads.
- **B — the band upload.** One whole-buffer background fill, then only
  the marker band uploads row by row — `1 + height` uploads.

## Per-end frame time

| End | Baseline | A row upload | B band upload | A speedup | B speedup |
| --- | --- | --- | --- | --- | --- |
| 320×240 | 2.87 ms | 0.31 ms | 0.32 ms | 9.14× | 9.10× |
| 640×480 | 12.65 ms | 1.19 ms | 1.31 ms | 10.59× | 9.63× |
| 1024×768 | 37.68 ms | 3.24 ms | 3.37 ms | 11.64× | 11.19× |

Upload calls per frame drop from `width × height` (76 800 / 307 200 /
786 432) to 480 / 960 / 1 536 (A) or 241 / 481 / 769 (B). **The speedup
grows with resolution** (9.1× → 11.6×): the baseline's per-pixel
bounds-check overhead scales with pixels while the candidates' slice
fills hit `memset` speed, so the candidate's edge widens exactly where
the frame budget is tightest. A and B land within noise of each other —
once the writes are contiguous, the extra whole-buffer fill of B buys
nothing.

**Verdict: take the row-contiguous upload.** All three ends confirm the
same frame, the same pixels, byte-identical digests
(`digests_equal` = 1 per end), an order-of-magnitude frame-time cut,
and the draw's own shading math is untouched — the candidates only
batch the transport.

## Harness

`crates/client/tests/frame_batch_candidates.rs` — the two candidates
emulated on the live `Framebuffer` faces, one deterministic run.

```sh
FRAME_BATCH_OUT=<abs path> cargo test -p client \
    --test frame_batch_candidates -- --nocapture
```

## Fixtures

`docs/review/fixtures/frame-batch/frame-batch.tsv` — 34 rows, captured
with the command above.
