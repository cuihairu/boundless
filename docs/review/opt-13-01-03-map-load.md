# OPT-13-01-03 — Map load: editor wait comparison

The acceptance: **compare editor wait times** (`比较编辑等待时间`).
The harness builds a real 4×4 space — a real `space.settings`, 16
zipped `.cdata` chunk archives, each holding a real `TerrainHeightMap2`
quantised PNG — then measures the editor's wait on four shapes.

| Shape | Wait | Ledger |
| --- | --- | --- |
| Settings parse (`SpaceHelper::init`) | **101.8 µs** | bounds 0..3, chunk size 100 |
| One-shot load (16 chunks: archive read + height decode) | **12.34 ms** | 16/16 decoded |
| Reopen, fresh helper, no cache | **11.65 ms** | 16/16 identical maps |
| Reused decode (kept image cache) | **0.067 ms** | 16/16 served, 0.54% of a load |
| Parallel chunks (4 workers, disjoint quarters) | **4.55 ms** | 16/16 identical maps |

## What the comparison says

- **The per-chunk decode is the wait**: ~0.77 ms per 36×36 chunk in a
  debug build, against a one-off 102 µs settings parse. Optimising
  the settings path would not move editor load time.
- **Keeping the decoded images wins outright**: a reopen served from
  a cache is ~185× cheaper than re-reading and re-decoding the
  archives (0.54% of the full load). This is the shape the JIT
  editor should hold between reloads — the cache cost is one clone
  of the image vector, not a PNG decode.
- **Chunk parallelism pays 2.71×** on four workers: the archive read
  and the PNG decode are entirely per-chunk independent. That is the
  same data-parallel shape the AoI migration work measured in
  OPT-08-01-03; no shared state between workers.
- **The reopen ratio is 0.94–1.28× across captures** (page capture:
  0.94×) — the archives stay in the OS page cache, so reopening a
  just-loaded space costs the same decode work with warm file data.
  The honest comparison is decode vs cache, not first vs second open.
- **Correctness is pinned, not just timed**: the decoded 36×36 pole
  grid is compared value-for-value against `unquantise(quantise(...))`
  of the source heights, and both the reopen and the parallel legs
  must produce pixels identical to the one-shot load.

## Harness

`crates/asset-pipeline/tests/map_load_baseline.rs` — one temp root,
fixed pole function, deterministic chunk archives. The archive
builder uses the port's own `ZipFileSystem` plus `TerrainHeightMap2`
(the same fixture shape as `world_machine_import`'s unit tests).

```sh
MAP_LOAD_OUT=<abs path> cargo test -p asset-pipeline \
    --test map_load_baseline -- --nocapture
```

## Fixtures

`docs/review/fixtures/map-load/map-load.tsv` — 11 rows, captured with
the command above.
