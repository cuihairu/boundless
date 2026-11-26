# OPT-11-01-04 — The visual-regression rerun

The acceptance: **the fixed camera meets the tolerance** (`固定镜头
满足容差`). The rerun replays the OPT-11-01-01 baseline's recorded
anchors and proves the cluster's optimisations changed transport and
scheduling only — never pixels.

## What the rerun pins

| Face | Tolerance | Result |
| --- | --- | --- |
| Frozen-camera digest @320×240 | bit-exact vs `7b728a36f4509325` | **match** |
| Frozen-camera digest @640×480 | bit-exact vs `d643ec40a231e325` | **match** |
| Frozen-camera digest @1024×768 | bit-exact vs `d46e431949522325` | **match** |
| Back-buffer bytes per end | exact (`w × h × 4`) | **match** |
| Texture file bytes | exact vs `113 880` | **match** |
| Texture level-0 pixel bytes | exact vs `151 552` | **match** |
| Row upload (OPT-11-01-02 A) | same anchor digest per end | **match** |
| Band upload (OPT-11-01-02 B) | same anchor digest per end | **match** |
| Animated content-cycle digest | FNV-1a over the 16 cycle frames, pinned `f13e8f8151d4e5a8` | **match** |

The digests are bit-exact because the software rasterizer is
deterministic — "meets the tolerance" for pixels means *equal*, and the
assertions hold for all three ends plus both candidate transports plus
the animated cycle.

## The frame-time envelope

Wall-clock timings move with the machine, so the frame averages carry a
two-sided 3× envelope against the baseline's recorded values:

| End | Baseline avg | Rerun avg | Ratio |
| --- | --- | --- | --- |
| 320×240 | 4.90 ms | 5.20 ms | 1.06× |
| 640×480 | 21.49 ms | 15.99 ms | 0.74× |
| 1024×768 | 65.88 ms | 43.69 ms | 0.66× |

The two-sided bound is doing real work: the rerun lands *up to 1.5×
faster* than the recorded session — machine load at baseline capture
time moved those numbers, and a one-sided "not slower" check would
falsely flag a faster machine. The envelope holds in both directions on
repeated runs.

## Harness

`crates/client/tests/frame_visual_rerun.rs` — one deterministic run:
the ledger legs first, the animated cycle (pinned digest), then the
three ends (frozen camera timed and digest-checked, both candidate
forms digest-checked).

```sh
FRAME_RERUN_OUT=<abs path> cargo test -p client \
    --test frame_visual_rerun -- --nocapture
```

## Fixtures

`docs/review/fixtures/frame-rerun/frame-rerun.tsv` — 25 rows, captured
with the command above.
