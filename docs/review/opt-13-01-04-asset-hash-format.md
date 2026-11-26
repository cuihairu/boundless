# OPT-13-01-04 — 复核资产哈希和格式 (recheck asset hashes and formats)

验收: **可复现且原版可读** (reproducible and readable by the original
engine's format contracts).

The delivery (`b981ae7`) drives three shipped faces against the
reference's format contracts:

- **Hashes** — `hash64` against golden vectors computed from the
  reference `bw_hash64.cpp` FNV-1a algorithm, including the LE-integer and
  `combine` contracts the dependency hashes rely on (5 vectors, one
  combined digest `0x85562B3F62384861`).
- **Compiled space** — `SpaceConverter` over a real space (chunk + cdata
  + settings) producing a `space.bin` that the compiled-space readers
  open; two independent convert passes compare **byte-for-byte and by
  digest** — 213 bytes, 3 sections, `digests_equal=true`, section names
  `models/box.model|models/spikes.model` readable through the original
  reader's tree.
- **Dependencies** — `DependencyList` round-trips with exact hashes: 4
  inputs → 1 output, primary `0x3A31A0D27AFBDAAB`, full
  `0x39F4CDF311678C81`.
- **Packed format** — `res_packer` packs the committed `bw.xml` fixture
  twice into identical bytes (both legs 177 B) and **unpacks the
  reference EL7-produced `bw.packed`** through the same shipped packer
  reader — 177 B, `tree_equal=true`: the original artifact round-trips
  through the port's faces.

Wall-clocks print on `ASSET-FMT` lines as probe data only. The golden
vectors, digests and byte counts the contracts pin live in the test
itself and every run re-derives them — no separate fixture record is
carried.

## Gates

- `cargo test -p asset-pipeline --test asset_hash_format` — 4 passed, 0
  failed (the delivery's own run, re-verified in-tree).