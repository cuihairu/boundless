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
are a snapshot, not a contract. OPT-04-01-02 uses this same harness;
the other side of the comparison lives in the commit cited in its
tick.

## OPT-04-01-03 — pre-generated codec evaluation

The interpretive codec walks the `Kind` tree on every call: the
per-`Kind` match, then the payload match, recursing through the
generic entry point for every composite. A pre-generated codec
compiles that tree once — at definition load, the way a build-time
codegen pass would emit specialized functions — and executes the
result without the per-call dispatch.

The evaluation lives in the test tree next to its harness
(`crates/entitydef/tests/entitydef_codec_gen.rs`): the value
vocabulary stays untouched (the closures take and return
`PropertyValue` exactly like the shipped path), and the three scalar
helpers the crate keeps private (`write_int`, `truncate_integer`,
`check_sequence_capacity`) are faithful local copies, pinned to the
originals by the byte-identity face below. The prototype compiles a
closure tree whose children are shared through `Rc<GeneratedCodec>`
(captured by both directions, off the hot path).

**Byte identity** (structural): for every property of every entity in
both fixtures (21 properties), the generated encode produces the same
bytes as `DataType::add_to_stream`, the generated decoder returns the
parsed default from the interpretive encoding, and the interpretive
decoder returns it from the generated encoding. A generated codec that
changed the wire format could not produce any timing row. The kinds
the interpretive stream path itself rejects (`CLASS`, `UDO_REF`,
`PYTHON`, `USER_TYPE`) are counted and asserted absent — the same
fixture assumption the baseline's `.expect("addToStream")` makes.

**Cost report** (`docs/review/fixtures/entitydef-baseline/codec-gen.tsv`,
92 rows; encode-only and decode-only nanoseconds, 20,000 rounds per
family per face, both paths, sink construction outside the clock
window, decode over a pre-encoded buffer with `rewind` between
windows). p50, three consecutive runs, ns per call:

| family | interp | gen | Δ |
|---|---:|---:|---:|
| scalars encode (INT8/16/32, UINT8/32, FLOAT64) | 182/184/181/191/201/118 | 175/174/176/175/174/112 | −3..−9% |
| scalars decode | 286/290/297/283/269/130 | 268/270/282/272/258/122 | −4..−6% |
| STRING encode / decode | 140 / 237 | 133 / 224 | −5% / −6% |
| ARRAY (STRING) encode / decode | 81 / 152 | 73 / 139 | −10% / −9% |
| ARRAY (UINT8) encode / decode | 81 / 156 | 73 / 143 | −10% / −8% |
| FIXED_DICT (x, y FLOAT32) encode / decode | 283 / 528 | 313 / 577 | **+11% / +9%** |
| FIXED_DICT (r, g, b UINT8) encode / decode | 600 / 1027 | 623 / 1031 | **+4% / +0.4%** |

(One intermediate run carried a fleet-load spike — UINT8 encode
191→329 ns — and the direction of every pair was stable across all
runs; the p50s above are the calm runs.)

**Verdict: not worth adopting as the shipped codec.** The compiled
closure tree wins 3–12% on flat families, where it removes the
per-call `Kind` and payload matches at the cost of one indirect call
per property — and loses 4–11% on `FIXED_DICT`, where the per-field
boxed-closure indirection costs more than the match it removes. The
one-off generation cost (7 µs for 21 properties) is irrelevant
either way. The remaining lever for a build-time codegen — straight-
line monomorphized functions with no boxed calls — cannot apply here:
entity definitions are runtime data loaded through
`EntityDescriptionMapFactory::create`, not build inputs, and the
reference engine itself ships the interpretive dispatch this port
mirrors. The ~10 ns scalar win also sits inside the record
discipline's noise band, so even the flat-family gain should not be
read as a contract.

One record incident: the first post-`rustfmt` solo run failed
transiently under fleet load with no panic text retained; 16
subsequent runs of the same binary passed.
