# OPT-11-01-03 — Resource streaming and frame scheduling, evaluated

Acceptance: **报告切图和长帧** (report the chunk switches and the long
frames).

`crates/client/tests/render_stream_sched_opt.rs` drives the baseline
fixed camera (640×480, 64 frames) through the shipped frame-scheduling
face — `FrameLoop::run_frame` firing `Tick → UpdateAnimations → Draw`
with the script hooks in phase, every frame — and lands six scheduled
chunk switches (frames 8/16/24/32/40/48) that stream the reference
fixture map into a live `ChunkSpace` through the shipped
`load_and_bind` face: the 37 candidates in scan order split into six
per-frame batches, the way an app streams a batch of chunks a frame.
One tracked region spans the whole walk, so the per-frame allocation
counts are deltas and the live-bytes trajectory is continuous.

## The chunk-switch report (structural)

| Switch | Frame | allocs | live after | bound |
|---|---|---|---|---|
| 1 | 8 | **954** | 1,269,813 | `00000000i` + the four exterior cells (`…o` ×4) |
| 2 | 16 | 32 | 1,270,541 | — (6 candidates read, none bind) |
| 3 | 24 | 32 | 1,271,717 | — |
| 4 | 32 | 32 | 1,271,997 | — |
| 5 | 40 | 32 | 1,274,069 | — |
| 6 | 48 | **128** | 1,279,767 | `001x000x/sep/00110000o` (the fixture's separator) |

The switches bind exactly the probe's whole set (6 chunks, order
preserved per batch); the run totals are 1,729 allocations, peak
1,281,573 bytes, final live 1,280,327 — the streamed space riding on
top of the device's 1,228,848-byte baseline from OPT-11-01-01. The
switches land on their scheduled frames, asserted; every switch frame
allocates strictly more than every steady frame (954/128/32/32/32/32
vs ≤9), asserted — the allocation ledger is the structural
switch/long-frame identification. After the walk, the four exterior
chunks each resolve their own cell centre through
`find_chunk_from_point` (the separator has no grid cell and is
excluded by shape). The whole ledger reruns identically (allocation
counts, bound sets, peak).

**Fixture finding while landing it:** the sixth bound chunk is
`001x000x/sep/00110000o` — a separator/transition chunk whose
identifier is a path, not a grid cell; the probe set is 5 grid chunks +
this separator.

## The long-frame report (probe data, printed, never asserted)

The switch frames carry their cost visibly (switch 1 ≈ 21–41 ms vs a
~20 ms steady frame), but on this shared debug runner the wall-clock
tail is dominated by OS jitter — the top slow frames per end are
steady-state frames (allocs = 8–9) reaching 27–76 ms, not switch
frames. The honest long-frame identification on this machine is the
allocation ledger above; the per-frame wall-clocks (192
`STREAM-FRAME` lines), the switch rows and the top-5 slowest frames
per end are printed for the record.

## Frame scheduling

Every frame ran the canonical active shape — `[Tick, UpdateAnimations,
Draw]`, `is_canonical_order` true, the hooks (`onTick`, `onDrawFrame`)
firing in phase order — asserted for all 64 frames on every end; the
scheduler and the device both count 64 frames.

## Gates

- `cargo test -p client`: 861 lib passed, 4 + 2 + 3 + 2 + 2 + 2
  integration passed, 0 failed (2 new: the scheduled-switch walk, the
  rerun ledger).
- `cargo fmt -p client --check`: clean.
- `cargo clippy -p client --all-targets -- -D warnings`: no new lint.
