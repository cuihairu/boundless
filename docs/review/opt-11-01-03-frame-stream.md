# OPT-11-01-03 — The resource-streaming and frame-scheduling candidates

The acceptance: **the texture swaps and the long frames reported**
(`报告切图和长帧`). The streaming face is the `TextureManager`'s lazy
decode — a texture's first `get` decodes it — so the legs measure what
a frame pays when a new texture enters mid-play against the scheduling
alternative of paying everything up front.

## The inline stream (切图)

One new texture per frame for the scene's whole content cycle (16
textures), then a second warm pass:

| Face | Value |
| --- | --- |
| Swap frames (decode inline) | 16, avg 14.51 ms |
| Quiet frames (warm pass) | 16, avg 12.98 ms |
| Decode cost per texture | 389 µs — 6.23 ms total swap overhead |
| Worst frame | 19.04 ms |

The hitches are real but bounded: each texture entry adds ~0.4 ms to
its frame — a 13%-shaped bump, not a stall — and the warm pass proves
the cost is paid exactly once per texture.

## The prefetch burst (帧调度)

All 16 textures decode before the loop, then play:

| Face | Value |
| --- | --- |
| Load burst | 5.17 ms, one-off, 0 misses |
| Play frames | 16, avg 13.44 ms, max 15.14 ms |
| Long frames during play | **0** |

**Verdict: prefetch wins the latency shape.** The worst play frame
drops from 19.0 ms to 15.1 ms (1.26× tighter) and no play frame carries
a decode; the price is a 5.2 ms loading burst before frame one —
1.2 ms cheaper than paying the same decodes inline
(6.23 ms), because the burst skips 16 per-frame round trips. The
streamed trade is honest for a bounded set; per-frame streaming is
viable exactly when the texture count per frame stays low, because the
decode is once-per-texture and the warm swap after it is microseconds.

## The warm swap (切图成本下限)

A frame reading all 16 cached textures per present pays **43 µs** of
cache hits (16 lookups, 0 misses) — once streamed, the swap cost is the
lookup, not the decode.

## Harness

`crates/client/tests/frame_stream_candidates.rs` — the three legs on
the live `TextureManager` and device faces, one deterministic run.

```sh
FRAME_STREAM_OUT=<abs path> cargo test -p client \
    --test frame_stream_candidates -- --nocapture
```

## Fixtures

`docs/review/fixtures/frame-stream/frame-stream.tsv` — 22 rows,
captured with the command above.
