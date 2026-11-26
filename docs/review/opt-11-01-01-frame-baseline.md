# OPT-11-01-01 — The three-end fixed-camera baseline

The acceptance: **frame time and video memory recorded** (`保存帧时间
和显存`). The reference client optimises across three target machines;
this port's pixel-producing device is the deterministic software
rasterizer (`render::software`) — the same binary on every platform, so
the per-platform split is BW-15-04's matrix — and the three ends here
are the three presentation profiles the device models: the back-buffer
sizes a small windowed (320×240), a standard windowed (640×480) and a
fullscreen-size (1024×768) present take.

## The fixed-camera frame time

The scene frozen at a fixed phase and step, 120 presents per end —
every frame draws the identical pixels, and the harness pins all 120
digests equal. That digest is the visual anchor each later row replays
against:

| End | Frame avg | Frame max | Anchor digest |
| --- | --- | --- | --- |
| 320×240 | 4.90 ms | 8.35 ms | `7b728a36f4509325` |
| 640×480 | 21.49 ms | 39.35 ms | `d643ec40a231e325` |
| 1024×768 | 65.88 ms | 134.28 ms | `d46e431949522325` |

The frame time is pixel-proportional and flat against content: the
animated walk (the scene stepping, its full 16-frame content cycle
covered) lands within noise of the frozen leg at every end — the draw
cost is `width × height` pixel writes, so the load lever is the
presentation profile, not what is on screen.

## The memory (显存账本)

| Face | Value |
| --- | --- |
| Back buffer | width × height × 4 (`D3DFMT_A8R8G8B8`) — 307 200 / 1 228 800 / 3 145 728 bytes |
| Texture fixtures | 4 BMPs, 113 880 file bytes → 151 552 level-0 pixel bytes cached |
| End totals (back buffer + textures) | 458 752 / 1 380 352 / 3 297 280 bytes |

The textures load through the `TextureManager`'s res tree and cache —
the decode cost is paid once, the pixel bytes are what the device holds
for the rest of the run. The back buffer dominates at every profile and
scales with the same pixel ratio as the frame time.

## Harness

`crates/client/tests/client_frame_baseline.rs` — the three ends, the
texture ledger, one deterministic run.

```sh
CLIENT_FRAME_OUT=<abs path> cargo test -p client \
    --test client_frame_baseline -- --nocapture
```

## Fixtures

`docs/review/fixtures/frame-baseline/frame-baseline.tsv` — 31 rows,
captured with the command above.
