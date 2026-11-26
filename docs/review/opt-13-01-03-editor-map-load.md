# OPT-13-01-03 — Optimize the map load

Acceptance: **比较编辑等待时间** (compare the editor's wait time).

`crates/chunk/tests/editor_map_load_opt.rs` drives the shipped editor-side
load gesture — enumerate a space, `ChunkSpace::load_and_bind` every cell,
capture the structural ledger, focus/query every cell — over the same
synthetic space through two resolvers: the raw read+parse resolver
(OPT-10-01-01) and the shipped `BwResource::open_section` LRU section
cache (OPT-04-01-02 / OPT-10-01-02). Wall-clocks and allocation counts
are printed on `MAP-EDIT` lines as probe data and are never asserted;
the structural equalities and the cache ledger are the contracts.

## The space and the two channels

The synthetic space is a 12×12 grid (144 chunk files) carved from the
committed fixture's XML shape (`<model>`/`<light>`/`<sound>` per cell),
with a `settings.xml` declaring `chunkSize=100` and bounds 0..11. The
serialized sections total 65,112 bytes — inside the shipped 100 KiB
`DEFAULT_CACHE_SIZE`, so the whole working set stays resident.

- **Raw** — every `open_section` equivalent reads the file and parses
  it. Two consecutive open-walk-close legs bind all 144 cells and
  re-parse all 144 files each time.
- **Cached** — one `BwResource` is shared across the cold open and the
  reopen leg. The first pass misses every file and fills the LRU; the
  second pass hits every file. `section_cache_stats()` asserts exactly
  144 misses and 144 hits (one miss per file, one hit per reopened
  file), and that the cache stays within the shipped budget.

Probe sample from one run (printed only): raw reopens 28.91 ms and
16.69 ms (24,319 allocations each); cached cold 28.07 ms (34,844
allocations) and cached reopen 11.02 ms (19,854 allocations). The
reopen wait drops by roughly a third once the sections are resident;
the cold pass pays for the cache's bookkeeping.

## Structural identity

Both channels run the exact same `editor_open` gesture and must answer
identically:

- bind order and the owning-chunk identifiers from the focus/query pass
  (144 per leg, asserted equal across raw legs, cached legs, and
  the two channels);
- the per-chunk item ledger — identifier, item tags, item resources —
  for every cell, asserted equal between raw and cached;
- every one of the 144 cached parses serialises byte-for-byte to the
  raw parse. `BwResource` roots the tree at the filename
  (`00000000o.chunk`) while `datasection::parse("", …)` roots it at the
  XML tag (`00000000o`), so the test normalises the root name before
  comparing `write_to_string()`; the children are identical.

A second test reruns the whole cached gesture twice over one LRU and
asserts the bind/owner ledger repeats exactly (`MAP-EDIT-RERUN`).

## Finding: the cache budget is a hard boundary

At 16×16 (256 files, ~102.3 KB serialized) the working set exceeds the
100 KiB LRU. A sequential bind scan then thrashes — each freshly
parsed section is inserted at the LRU head and evicts the oldest entry,
which is the one the scan reaches next; the measured reopen legs made
**zero** cache hits despite 256 files being "in the cache" at some
point. The test therefore uses a grid whose working set fits the
shipped budget and asserts the exact hit/miss structure rather than a
wall-clock inequality. A production space larger than the budget will
not benefit from this cache on a full sequential scan.

## Gates

- `cargo test -p chunk`: 175 lib + 2 + 2 + 2 + 2 + 2 + 9 integration
  passed (including the two new `editor_map_load_opt` tests), 0 failed.
- `cargo fmt -p chunk -- --check`: clean.
- `cargo clippy -p chunk --all-targets -- -D warnings`: no lint.
