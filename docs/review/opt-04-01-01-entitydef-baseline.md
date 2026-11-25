# OPT-04-01 — EntityDef startup and codec baseline

The section measures what the entity definitions cost at startup and on
the encode/decode path, then evaluates caching and pre-generated codecs
against that baseline. This row (OPT-04-01-01) establishes the
measurement harness; it does not change any hot path.

## Harness

`crates/entitydef/tests/entitydef_baseline.rs` runs three legs against
the tracked definition fixtures, through the same load path the engine
uses (`EntityDescriptionMapFactory::create`: `FsResolver` over the res
root, aliases initialized, `scripts/entities.xml` opened, every
definition parsed into an `EntityDescriptionMap` — the
`bwentity_load.rs` contract):

1. **Startup** — a cold factory create of `full_def` (3 entities) and
   `fantasydemo_def` (5 entities), file reads included. The parse must
   succeed with clean diagnostics and the definition digest must equal
   the captured oracle (`full_entitydef.json` /
   `fantasydemo_entitydef.json`), so a broken load cannot produce a
   timing row; the digest string is recorded alongside.
2. **Codec** — every property of every entity in both fixtures,
   round-tripped 20,000 times through `DataType::addToStream` /
   `createFromStream` from its parsed default, one clocked sample per
   round-trip, identity asserted on every round. Eleven families appear
   across the two fixtures (scalars, STRING, ARRAY of STRING/UINT8,
   FIXED_DICT of 2 and 3 fields) — this is the interpretive per-`Kind`
   dispatch any pre-generated codec must match byte for byte.
3. **Warm start** — the fantasydemo tree loaded a second time in the
   same process; the digest must equal the cold load's, and the number
   measures the XML parse rather than the file reads.

Wall-clock numbers are **recorded, never asserted tight** (machine
dependent — the network and Python-bridge baseline discipline). The
record sink writes `metric<TAB>value` rows to stdout and, when
`DEF_BASELINE_OUT` is set, appends to that path:

```sh
DEF_BASELINE_OUT=<abs path> cargo test -p entitydef \
    --test entitydef_baseline the_entitydef_baseline_runs_all_legs -- --exact
```

## OPT-04-01-02 — cached definition parsing, cold/warm

`FsResolver::open_section` used to read and re-parse the XML on every
open. The reference resource manager keeps parsed sections for the
process lifetime — `DataSectionCache`
(`resmgr/data_section_cache.hpp`: `find`/`add` by path, `clear` as the
invalidation face, bounded by `setSize`) — and the port now mirrors it:
a thread-local cache keyed by the joined full path, bounded by entry
count (1024; on overflow it clears rather than tracking an LRU chain —
the definition trees are a few dozen small files), with
`clear_section_cache()` as the `clear` face. `MemoryResolver` is
untouched; the only `FsResolver` consumers in the repo load read-only
fixtures.

The baseline harness is the comparison instrument: the runner clears
the cache first, so `startup_*_ms` measures a cache-cold load and
`startup_warm_fantasydemo_ms` measures exactly the cached reload (tree
clones plus map assembly — no file reads, no XML parse).

| Face | Before (`a041efd`) | After (`9881c37`) |
|---|---:|---:|
| cold `fantasydemo_def` | 0.615 ms | 0.916 ms |
| warm `fantasydemo_def` | 0.448 ms | **0.297 ms** |
| same-run cold/warm gap | 0.167 ms | **0.619 ms** |

Reading: the cross-record cold numbers are not comparable (fleet load
~40 then ~21), so the honest face is the **same-run gap**: before the
cache, a warm reload saved 0.167 ms (OS page cache only); after, it
saves 0.619 ms — the read+parse is gone from the reload, leaving tree
clones and map assembly. The digests are identical before and after
(`18D00DC6…`, pinned against the oracle on every run).

## Record

`docs/review/fixtures/entitydef-baseline/entitydef-baseline.tsv` (63
rows) is a rolling canonical record: the OPT-04-01-01 before record
lives at commit `a041efd`, the OPT-04-01-02 after record at `9881c37`
(2026-11-26 toolchain):

| Face | Value |
|---|---:|
| cold startup, `full_def` (3 entities) | 0.637 ms |
| cold startup, `fantasydemo_def` (5 entities) | 0.615 ms |
| warm startup, `fantasydemo_def` | 0.448 ms |
| codec scalar round-trip (INT32/INT16/INT8/UINT8/UINT32) p50 | 457–474 ns |
| codec FLOAT64 p50 | 268 ns |
| codec STRING p50 | 340 ns |
| codec ARRAY (STRING / UINT8) p50 | 238 / 247 ns |
| codec FIXED_DICT (2 / 3 fields) p50 | 817 / 1,565 ns |

Reading: the interpretive scalar round-trip costs ≈460 ns; composites
scale with field count (FIXED_DICT 3 fields ≈ 3.4× the scalar). Startup
is sub-millisecond for both fixtures — the interesting startup face for
OPT-04-01-02 (caching) is therefore the parse share, which the warm
number bounds from below (file reads ≈ 0.17 ms of the cold 0.615 ms
under this load).

Two consecutive runs of the runner produced identical metric names,
identical structural results and identical digests; the timing values
are a snapshot, not a contract. OPT-04-01-02/03 comparisons use this
same harness; the other side of each comparison lives in the commit
cited in its tick.
