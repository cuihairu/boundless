# OPT-13-01-02 — Evaluate the conversion cache and the concurrency faces

Acceptance: **评估缓存与并发转换** (评估: compare cold/hot cost).

`crates/asset-pipeline/tests/cache_concurrency_evaluation.rs` drives the
shipped content cache through a full miss/hit/restore cycle over the
fixed asset set (the OPT-13-01-01 fixtures plus a shared
`palettes/common.palette` secondary dependency), drives the concurrency
faces, and drives the `ContentAddressableCache` directly. Wall-clocks
are probe data printed on `CACHE-EVAL` lines and are never asserted;
the structural contracts are.

## The pipeline cache leg — six walks, one shared cache

Every walk registers a **fresh pipeline session** over persistent
trees (see the finding below on why the session must be fresh). The
converter declares the palette as a critical secondary source
dependency, so each task's full dependency list carries it.

- **Cold** — 12 conversions; the cache ends with three entries per
  asset (36): the pre-conversion dependency list under the
  primary-only input hash, the post-conversion full list under the
  secondary input hash, and the output under its content hash.
- **Flip (miss)** — the palette changes; the fresh session re-hashes
  it from disk, the stored hash misses, the cache has no full list
  under the new input hash, all 12 re-convert, and the new full lists
  land next to the old ones (48 entries — one new per asset; the
  primary list and output hashes are unchanged, so their entries
  overwrite in place).
- **Revert (hit)** — the palette flips back; the cached full list
  under the v1 input hash restores and validates against the current
  hashes — zero conversions, the outputs untouched, nothing written.
- **Hot restore** — the intermediate and output trees are wiped but
  their directory skeleton and the cache are kept. The walk rebuilds
  nothing: the cached primary list restores first, the cached full
  list answers the secondary check, and the cached outputs restore
  under their stored hashes — byte-identical digests, zero
  conversions, the cache neither grows nor shrinks.
- **Bare-root restore** — the same wipe *without* the output
  directories. The restore stage copies the cached file to a temporary
  file **inside the target file's directory** and renames it into
  place, so it cannot create a missing directory: exactly one task per
  missing output subdirectory (3) falls back to conversion — which
  recreates the tree — and the remaining nine tasks restore from the
  cache. Digests still reproduce; the fallback conversions overwrite
  their entries in place (still 48).
- **Blind contrast** — the same wiped state with cache reads
  disabled: all 12 re-convert.

Probe sample from one run (printed only): cold 12.09 ms, flip miss
11.60 ms, revert hit 8.28 ms, hot restore 13.26 ms, bare restore
14.27 ms, blind 8.81 ms. At this 12-file scale the cache costs more
than it saves — the restore legs validate every hash and copy through
temporaries, while blind conversion of copy-sized assets is cheap; the
contract is the hit/miss structure, not the wall-clock.

## The concurrency faces

The shipped `process_tasks` is `processTasksOnSingleThread` by
design; the multi-threaded `BgTaskManager` variant is a dropped
Windows concern. `set_thread_count`/`set_force_rebuild` are stored
observables that do not change the loop: a session storing four
threads runs the same single-threaded walk and produces artifacts
byte-identical to a fresh one-thread walk over the same tree (probe:
10.91 ms vs 11.18 ms, printed only). The stored counts round-trip.

## The direct cache face

`ContentAddressableCache` is driven without the pipeline: the bucket
layout `{cachePath}{low byte of hash %hhX}/{basename}.{hash %X}`, the
cache root must pre-exist (only bucket subdirectories are
auto-created), the write lands a bucket file and fires the write
event, a same-basename path restores byte-identical (read event), a
different basename and an unknown hash miss (read-miss events), and
the disabled gates (`set_cache_read(false)` / `set_cache_write(false)`)
answer false with no callbacks. Two independent walks land identical
bucket paths and identical event ledgers (read=1, miss=2, write=1,
write_miss=0 each).

## Findings

- **`queue_task` only queues `status == New` tasks** — a second
  `build` on the same session re-queues nothing ("Found: 0 tasks").
  Every honest cache comparison here is fresh-session over persistent
  trees. (Same finding as OPT-13-01-01, where it first surfaced.)
- **The cache restore stage cannot create directories** — the temp
  copy + rename into the target directory fails when the directory is
  absent, so a bare output root re-converts exactly one asset per
  missing output subdirectory. Deterministic and structural: the count
  is the number of distinct output directories, not an ordering
  accident.
- **Cache entries per asset are three, not one** — dependency-list
  entries are written twice (pre-conversion primary list, post-
  conversion full list) and the output once; a palette flip adds one
  new entry per asset and overwrites the other two in place.

## Gates

- `cargo test -p asset-pipeline`: 384 lib + 2 + 2 integration passed,
  0 failed (2 new in `cache_concurrency_evaluation`).
- `cargo fmt -p asset-pipeline -- --check`: clean.
- `cargo clippy -p asset-pipeline --all-targets -- -D warnings`: no
  lint.
