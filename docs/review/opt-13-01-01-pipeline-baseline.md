# OPT-13-01-01 — The build-pipeline baseline

The acceptance: **a fixed-asset baseline forms** (`形成固定资产基线`).
Three legs measure the pipeline on a real tree — 48 real 4 KiB files,
a real content-addressable cache, real dependency hashing:

| Leg | What it pays | Result |
| --- | --- | --- |
| Content hash (`hash64` FNV-1a, pre-read bytes) | the hash face alone | **28.3 µs**/asset |
| Cold build | hash → cache miss → write output → store into cache | **88.0 µs**/asset, 48 misses, 48 stores |
| Incremental build | hash → cache hit → copy → read-back verify | **84.6 µs**/asset, 48 hits, 0 store misses |
| Editor scan | `TaskStore` info records + requested set + scanning callbacks | **4.3 µs**/asset, callbacks fire once |
| Editor convert | `Converting` state → current → complete | **3.4 µs**/asset, 48 records |
| Editor deps | `DependencyList::initialise` per asset (3 inputs hashed) | **39.9 µs**/asset, 3 inputs each |

## What the numbers say

- **Cold ≈ warm at this asset size (1.04×, 1.04–1.22× across
  captures).** The fixture's "conversion" is one 4 KiB output write,
  so the cold leg's extra work (a cache store) roughly balances the
  warm leg's (a copy plus read-back verify). The honest reading: the
  content-addressable cache's worth scales with conversion cost — at
  4 KiB of passthrough there is little to save. Real converters
  (mesh, texture, navmesh) multiply the cold side and widen the gap;
  this baseline is the floor they will be measured against.
- **The hash face is the pipeline's floor**: ~28 µs per 4 KiB asset in
  a debug build, byte-at-a-time FNV-1a. Both build legs re-hash the
  source, so the content hash bounds the incremental build's cost from
  below — it cannot get cheaper than "hash the file you were handed".
- **The dependency leg dominates editor load**: 39.9 µs per asset
  against 4.3 µs of scan and 3.4 µs of convert bookkeeping. Its three
  inputs each pay a hash, and the source input pays a real file read
  inside `get_hash` — the editor's wait-to-ready tax is I/O, not
  bookkeeping.
- **The cache's event seam accounts every asset**: 48 misses + 48
  stores cold, 48 hits warm, no double counts — the counters are the
  contract the concurrency work (13-01-02) will lean on.

## Harness

`crates/asset-pipeline/tests/build_pipeline_baseline.rs` — one
deterministic run, one temp root (`src/`, `out-cold/`, `out-warm/`,
`cache/`), fixed position-valued asset bytes, the cache root created
before any store. The legs share pre-read source bytes so cold and
warm are structurally symmetric; correctness asserts ride every leg
(miss/hit polarity, byte-exact restore, task-record round-trip, 3
primary inputs per dependency list).

```sh
PIPELINE_BASELINE_OUT=<abs path> cargo test -p asset-pipeline \
    --test build_pipeline_baseline -- --nocapture
```

## Fixtures

`docs/review/fixtures/pipeline-baseline/pipeline-baseline.tsv` — 15
rows, captured with the command above.
