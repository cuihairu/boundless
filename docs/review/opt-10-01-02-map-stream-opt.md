# OPT-10-01-02 — The streaming reload through the shipped section cache

Acceptance: **报告峰值内存及加载速度** (report the peak memory and the
load speed).

`crates/chunk/tests/map_stream_opt.rs` runs the streaming cycle the
server actually runs — load, unload two cells behind the camera,
reload them through the shipped `scan`, query the whole probe grid
again — through two resolvers on the same `load_and_bind` face:

- **shipped baseline**: the OPT-10-01-01 resolver, every call reads the
  `.chunk` file and parses it;
- **candidate**: the shipped section cache (`BwResource::open_section`,
  the OPT-04-01-02 LRU). The first open fills the cache, every reload
  of the same file is a hit.

Both maps must come back identical — same bound sets, same reloaded
pairs, same find answers, same collide distances (asserted via
`DataSection` equality on the cached parse and per-probe comparisons
on the scripts). A fully warm second space over the same geometry
measures the steady state.

## The saved report (this tree, this runner)

| Face | wall-clock | allocs | peak bytes |
|---|---|---|---|
| cold start, raw (OPT-10-01-01 baseline) | **2.97 ms** | 2025 | 63 389 |
| cold start, cached (fills the cache) | **5.34 ms** | 2494 | 83 155 |
| reload two cells, raw | **10.46 ms** | 931 | 41 059 |
| reload two cells, cached (LRU hits) | **3.77 ms** | 759 | 41 059 |
| full space, warm cache (second space id) | **1.50 ms** | 1132 | 43 807 |

Peak memory is a live-bytes high-water in the counting allocator (the
OPT-01-01-01 pattern extended with a `PEAK` cell; blocks allocated
outside the region overstay, so the peak stays an upper bound).

The honest trade: the cache **pays on fill** — each section is sized by
a full `write_to_string()` serialize, so the cold start adds ~469
allocs and is slower on this runner — and **wins on every later load**:
the reload drops to 3.77 ms / 759 allocs from the raw 10.46 ms / 931
(read + parse + 28 failed candidate reads), and a fully warm space
loads in 1.50 ms / 1132 allocs versus the cold 2.97 ms / 2025 (the
clone-on-hit is the `load_and_bind` resolver face's owned-`DataSection`
contract). For a server that streams cells in and out continuously,
the fill cost is paid once per file per process and the reload win
compounds; for a one-shot cold load the raw resolver stays cheaper.

Wall-clock is probe data, printed, never asserted — it drifted between
runs on this box (cold-cached 3.3 → 5.3 ms, reload-raw 3.8 → 10.5 ms);
the allocation counts and peak bytes reproduced exactly.

## Structural assertions

- the cached section equals the raw parse (`DataSection: PartialEq`);
- both scripts bind the same six files, reload the same two cells, and
  answer the 100-probe grid identically (find ids, collide distances
  within 1e-5);
- the cycle reruns to the same map through the cached resolver
  (bound, reloaded, finds, dists all equal across two full scripts).

## Gates

- `cargo test -p chunk`: 188 passed, 0 failed (2 new: the measured
  streaming reload, the cycle rerun identity).
- `cargo fmt -p chunk --check`: clean.
- `cargo clippy -p chunk --all-targets -- -D warnings`: no new lint.
