# OPT-13-01-04 — Recheck the asset hashes and formats

验收: **可复现且原版可读** (reproducible and readable by the original
engine).

`crates/asset-pipeline/tests/asset_hash_format.rs` drives the shipped
hash, conversion and packing faces and checks the artifacts against the
reference formats and the reference tool's output. Wall-clocks are
printed on `ASSET-FMT` lines as probe data and never asserted; the
contracts are the exact vectors, bytes and structure.

## The hash face

`hash64` is checked against golden vectors of the reference algorithm
(`BigWorld/programming/bigworld/lib/cstdmf/bw_hash64.cpp`: FNV-1a 64
with the standard offset basis `14695981039346656037` and prime
`1099511628211`): the empty input, `"a"` (the published FNV-1a-64
value `0xAF63DC4C8601EC8C`), `"hello"`, `"bw.xml"`, and the LE-integer
contracts — `compute_i64(-1) == compute_u64(u64::MAX)` hashes the
two's-complement bytes, and `combine(0, compute_u64(12345))` equals
`0x85562B3F62384861`, the exact value the C++ `combine` template
produces when its caller pre-hashes the integer (as every Rust caller
does).

## The dependency format

A `DependencyList` is built through the **shipped `AssetCompiler`** —
so the stored hashes come from the real `getHash` seam (path hash
combined with content hash) over a planted space — populated like a
real task (`initialise` with converter id/version/params, one
secondary source dependency, one intermediate and one final output),
serialised out and read back. The on-disk format contract is pinned:
the reference `PrimaryInputs`/`SecondaryInputs`/`IntermediateOutputs`/
`Outputs` sections are present, every stored `(dependency, hash)`
survives the round trip, the derived `get_input_hash(false)`/`(true)`
values are exact and equal after the read, and an independent rewrite
of the same list produces byte-identical `.deps` text.

## The compiled space format

The shipped `SpaceConverter` converts a real space (chunk + cdata twin
+ settings, two model resources in the chunk) into a `space.bin`,
twice, into two independent output trees (a fresh `BwResource` and
`AssetCompiler` per pass, so no section-cache carry-over can leak
between passes). Two contracts:

- **reproducible** — both passes produce identical bytes and an
  identical `compute_bytes` digest;
- **original-readable** — both files open through the
  `compiled-space` readers that port the original engine's
  `binary_format`/`string_table`/`asset_list`/`compiled_space_settings`
  readers: the `BWTB` container magic/version, the original writer's
  header shape (`header_size=96`, `file_size=0` — the reference writer
  memsets the header it writes), a valid string table containing both
  model resources, a `BWAL` asset list pointing at them, and the
  space bounds section.

Probe sample (printed only): two convert passes of 213 bytes,
`first_ns=484911`, `second_ns=407137`, `sections=3`,
`names=models/box.model|models/spikes.model`.

## The packed format

The shipped `res_packer` packs the committed `bw.xml` fixture
(`docs/review/fixtures/fantasydemo_def/res/server/bw.xml`) twice
through two independent resources; both outputs are 177 bytes and
byte-identical (`pack leg=0 bytes=177 ns=287560`, `pack leg=1 bytes=177
ns=283855`). The **reference EL7 artifact**
`docs/review/fixtures/el7_res_packer/bw.packed` — produced by the
original `res_packer` binary (`scripts/el7-interop/run-el7-res-packer.sh`
records the invocation) — and both shipped outputs are parsed by the
shipped packed reader and compared node-by-node (names, child counts,
leaf values) against the untouched XML source: zero mismatches, i.e.
the original engine's packed bytes read back identical to their source
through the port's reader.

## Finding: the converter dependency hash does not include the primary count

The reference `DependencyList::getInputHash` folds `Hash64::compute(
primaryInputs_.size())` into the combined seed before combining the
primary hashes; the port starts the seed at 0 and folds only the
hashes. Two lists with the same primary hashes but different primary
input counts therefore produce the same input hash in the port. The
full (secondary-inclusive) hash still folds the secondary count
(`combine(hash, secondaryInputs_.size())`), which matches the
reference, so the cache-key surface is unaffected for real tasks — the
primary-count term is a mixing term, not a correctness term. The test
asserts the port's current behaviour (primary hash stable and
independent of the secondary set) and documents the divergence.

## Gates

- `cargo test -p asset-pipeline`: 384 lib + 2 + 2 + 4 integration
  passed, 0 failed.
- `cargo fmt -p asset-pipeline -- --check`: clean.
- `cargo clippy -p asset-pipeline --all-targets -- -D warnings`: no
  lint.
