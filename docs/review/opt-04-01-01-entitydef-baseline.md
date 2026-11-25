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

## Record

`docs/review/fixtures/entitydef-baseline/entitydef-baseline.tsv` (63
rows, commit `a041efd`, 2026-11-26 toolchain, machine under fleet load
~40):

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
