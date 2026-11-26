# OPT-10-01-01 — The map load and query baseline

Acceptance: **保存冷启动、内存及尾延迟** (save the cold start, the
memory and the tail latency).

`crates/chunk/tests/map_load_baseline.rs` measures the shipped map
faces end to end over the original fixture space
(`docs/review/fixtures/chunk_space`, the 18×2 grid the reference
`settings.xml` describes). The cold start is the whole streaming
sequence as one measured region — parse the mapping, enumerate the
space directory (37 candidates: 36 cells plus the inside chunk),
`load_and_bind` the six `.chunk` files on disk, focus the loaded
chunks, and register the terrain slabs (two half-cell plates at 10 m
per queried cell) through the shipped `add_obstacle` focussed gate.

## The saved baseline (this tree, this runner)

| Face | Value |
|---|---|
| cold start (parse → scan enum → load → bind → focus → obstacles) | **1.6–2.5 ms**, 6 chunks bound |
| cold-start allocations | **2025 allocs / 163 790 bytes** (counting allocator, the OPT-01-01-01 pattern) |
| cold-start RSS delta | **~428 kB** (`VmRSS` around the region, probe data) |
| `findChunkFromPoint` (100 probes, 5×5 per loaded cell) | p50 **0.53 µs**, p99/max **1.7 µs** |
| `ChunkSpace::collide` (100 findDropPoint rays, 50 m → −10 m) | p50 **1.8 µs**, p99/max **18.7 µs** |

The structural ledger is asserted and reruns identically: the bound
set is exactly the six files (all bound), every sample point resolves
through `find_chunk_from_point` to its owning cell's chunk, every ray
stops at the slab 40 m down the ray (±1e-3), and the cold start
reproduces the same bind order, find answers, collide distances and
allocation count (2025/163790 both runs). The wall-clock, RSS and
tail numbers are probe data, printed, never asserted — the collide
p99 tail is one grid-walk column crossing several column boundaries,
not a queue.

## Gates

- `cargo test -p chunk`: 188 passed, 0 failed (2 new: the saved
  baseline, the rerun identity).
- `cargo fmt -p chunk --check`: clean.
- `cargo clippy -p chunk --all-targets -- -D warnings`: no new lint.
