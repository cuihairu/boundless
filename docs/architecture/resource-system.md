# Resource system

Two crates cover resources: `crates/resources` is the BWResource port —
search paths, section trees, archives and caches — and `crates/asset-pipeline`
is the BW-13 conversion infrastructure. This page records how a path becomes
data today, and how much of the asset pipeline actually converts anything
(answer: the infrastructure landed, the converters did not).

## The path layer

`BwResource` (`crates/resources/src/bwresource.rs`, port of `bwresource.cpp`)
holds an ordered search-path list, a `MultiFileSystem` built from it, and an
LRU section cache. Paths are added with `add_paths` / `add_paths_with_source`
and only live roots survive — a non-existent path is reported and dropped.
`init_from_sources` reproduces the reference source order: `--res`/`-r`
arguments first, then the deprecated `BW_RES_PATH` environment variable, then
the first usable line of `~/.bwmachined.conf` for the uid lookup
(`crates/resources/src/bwresource.rs` documents the order and the
`-uid` override).

`refresh_paths` classifies each root: a `*.zip` file or a `<path>/res.zip`
package becomes a zip archive root; a directory becomes a native root; the
editor build additionally appends an empty-base root
(`editor_default_root`). Lookup goes through
`crates/resources/src/multi_file_system.rs`, whose `read_directory` is a
**union** across all roots — the same name under two res paths is listed
twice, exactly as the reference behaves.

Reference quirks are preserved deliberately (the module doc enumerates them):

- The res-path separator is `:` on unix; `format_path` ensures a trailing
  slash; `format_search_path` normalises backslashes.
- `path_is_relative("")` is **false** — an empty path counts as absolute.
- `get_file_path` is POSIX `dirname()`; extension helpers ignore dots inside
  directory components, and `.hidden` yields extension `"hidden"`.
- Lookups are case-sensitive everywhere except `dissolve_filename`, which
  matches the search-path prefix case-insensitively (the reference's
  `mf_pathcmp`).
- `resolve_filename` / `search_for_file` walk the roots depth-first and
  rewrite the query to the hit; `resolve_to_relative_path` matches on a `/`
  boundary.

`file_exists` consults the res-path view while `file_absolutely_exists`
probes the native filesystem directly — both exist because the reference has
both (`crates/resources/src/bwresource.rs`).

## Sections and loading

`open_section` does **not** use the file extension: the port of
`createAppropriateSection` sniffs content, and a first non-whitespace byte of
`<` means XML (`crates/resources/src/bwresource.rs`). Everything else returns
`None` at this level — the `BinSection`, `ZipSection` and `DirSection`
branches, `DataSectionCensus` bookkeeping and undotted `makeNewSection` are
flagged in the code as later tasks.

The XML tree is `crates/resources/src/datasection.rs` (port of
`resmgr/xml_section.cpp`): attributes are children ordered before element
children, text is trimmed at parse time, CDATA stays raw, and any parse error
fails the whole document. The save path is pinned byte-exact against the
reference `XMLSection::writeToStream` layout by a test in
`crates/resources/src/bwresource.rs`.

Loaded sections sit in an LRU (`crates/resources/src/data_section_cache.rs`,
default 100 KiB, tunable via `BW_CACHE_SIZE`): zero-byte entries are never
cached, oversized sections never displace the chain, and a re-added entry
replaces its payload *without* moving to the head — all reference behaviours.

Other formats, all direct ports:

| Format | File | Cited origin |
| --- | --- | --- |
| Zip archives | `crates/resources/src/zip.rs` | `resmgr/zip_file_system.cpp` (nested zip nodes absent) |
| Packed sections | `crates/resources/src/packed.rs` | `resmgr/packed_section.cpp` (ships the reference `BW_ENCRYPTION_KEY = 0xc9`) |
| Zip stream compression | `crates/resources/src/zip_stream.rs` | `lib/network/zip_stream.cpp` (tags 1..=9, expansion guard) |
| XML character escaping | `crates/resources/src/special_chars.rs` | `resmgr/xml_special_chars.cpp` |
| Base64 | `crates/resources/src/base64.rs` | `cstdmf/base64.cpp` |
| Binary blocks | `crates/resources/src/binary.rs` | `resmgr/bin_section.cpp` |

Cross-implementation agreement is pinned by a fixture:
`crates/resources/fixtures/zlib_ref.log.bin` is a deflate stream produced by
Python's `zlib`, and one test in `crates/resources/src/zip.rs` decodes it
byte for byte.

## Script face and consumers

The Python `ResMgr` module (`crates/python-host/src/resmgr.rs`) exposes
`openSection`, `save`, `purge`, `resolveToAbsolutePath`, `isDir`, `isFile`,
`localise` and the `DataSection`/`ResourceTable` types over this crate — its
deltas are recorded on the [Entity pipeline](/architecture/entity-pipeline)
page. The server configuration loader reads its XML through `BwResource`
(`crates/server-runtime/src/config.rs`), as do `crates/server-runtime/src/server_app.rs`,
`apps/server/src/main.rs` and the whole of `crates/asset-pipeline`.

## Asset conversion (BW-13): infrastructure only

`crates/asset-pipeline` ports the reference `tools/asset_pipeline` frame —
17 modules listed in `crates/asset-pipeline/src/lib.rs`. What landed:

- **Dependency model** (`dependency.rs`, `dependency_list.rs`, `hash64.rs`):
  the six dependency types (`SourceFile`…`Directory`), the critical flag,
  DataSection serialisation, and the FNV-1a-based `Hash64` with the
  golden-ratio fold (BW-13-01-01).
- **Incremental cache** (`content_addressable_cache.rs`): the
  `{cachePath}/{hash byte}/{name}.{hash}` layout, temp-file-then-move
  staging, event callbacks (BW-13-01-02).
- **Batch compiler** (`batch_compiler.rs`, `asset_compiler.rs`,
  `asset_compiler_options.rs`): the `parseCommandLine → initCompiler → build
  → outputReport` flow, exit code = failed count, and the `CompilerHook`
  trait seam that replaces the reference's subclassing (BW-13-01-03).
- **Rules and discovery** (`hierarchical_config.rs`,
  `generic_conversion_rule.rs`, `conversion_rule.rs`, `task_finder.rs`): the
  `asset_rules.xml` merge semantics (deeper overrides shallower, `import`
  chains, a rule naming an unknown converter silently yields no task) and the
  `sourcePattern` replace.
- **Task processing** (`task_processor.rs`): the
  `PROCESSING → NEEDS_PRIMARY_DEPS → … → DONE/FAILED` ladder, `.deps` files,
  hash-checked skips — deliberately single-threaded (sub-dependencies run
  inline), which the module doc records as an interleaving-only change.

What does **not** exist: there is no `Converter` implementation for any real
asset class — the only impls in the crate are test doubles
(`TextConverter`, `NoopConverter`, …). No JIT/on-demand compiler, no
model/texture/space/BSP/effect converters, no `assetprocessor`,
`offline_processor`, `res_packer` or `resourcechecker` workflows, no NavGen
entry, and no WorldEditor/ModelEditor/DCC-export work. The interface-mapping
rows BW-13-06-01..03 are recorded decisions (empty placeholders stay
unmigrated; `crates/plugin-host` was created for the plugin system), and the
natvis debug-field view (BW-13-06-04) is blocked with the reasoning archived
in `docs/review/bw13-06-04-debug-field-view.md`.

## State

| Piece | State |
| --- | --- |
| Search paths, init sources, root classification | Implemented (`bwresource.rs`) |
| Path resolution, resolve/dissolve, quirks | Implemented (`bwresource.rs`) |
| Multi-root union enumeration | Implemented (`multi_file_system.rs`) |
| XML section tree, byte-exact save | Implemented (`datasection.rs`) |
| Section cache (LRU, reference quirks) | Implemented (`data_section_cache.rs`) |
| Zip / packed / zip-stream / base64 / binary | Implemented (`zip.rs`, `packed.rs`, `zip_stream.rs`, `base64.rs`, `binary.rs`) |
| `BinSection`/`ZipSection`/`DirSection`, section census | **Not implemented** (flagged "later task" in `bwresource.rs`) |
| Asset dependency graph, incremental cache, batch compiler | Implemented (BW-13-01-01..03, `crates/asset-pipeline/`) |
| Real asset converters, JIT compiler | **Not implemented** (BW-13-01-04..06) |
| assetprocessor / offline_processor / res_packer / resourcechecker / NavGen | **Not implemented** (BW-13-02) |
| WorldEditor / ModelEditor / DCC export | **Not implemented** (BW-13-03..05) |
