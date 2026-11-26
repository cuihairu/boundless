# OPT-13-01-01 — Record the cold-build, incremental and editor-load baseline

Acceptance: **形成固定资产基线** (form a fixed-asset baseline).

`crates/asset-pipeline/tests/build_baselines.rs` drives the shipped batch
pipeline over a committed fixed asset set (`crates/asset-pipeline/fixtures/baseline`:
12 nested `.txt` sources, 48,293 bytes) through all three baseline legs.
Wall-clocks are probe data printed on `BASELINE` lines and are never
asserted; the structural contracts are.

## The fixed asset set

`fixtures/baseline/{models,textures,ui}` holds 12 deterministic files
(4/5/3 respectively). The test copies them into a fresh temp source tree
per walk, so every leg starts from the same bytes and no committed
fixture is mutated. The fixture root is resolved through
`env!("CARGO_MANIFEST_DIR")`, not the process cwd.

## Cold build

The pipeline is registered with the shipped seams: `BwResource::init_paths`
+ `enable_editor_default_root` for the source root,
`BatchCompiler::new`, the compiler's `set_intermediate_path`/`set_output_path`
setters (what `BatchCompilerOptions::apply` pushes through), a
`.txt`-claiming `ConversionRule` and a counting copy `Converter` via
`ConverterInfo::DEFAULT_FLAGS`. Asserted: zero failures; exactly 12
converter invocations (one per asset); 12 outputs whose bytes equal the
sources; and one `{source}.deps` sidecar per source in the intermediate
tree. The build's own summary prints `12 succeeded, 0 failed,
12 converted` — the converter-call counter, not that label, is the
observable for "converted" (see the finding below).

## Incremental build

A fresh pipeline session is registered over the same (unchanged) tree
and rebuilt — every task answers up-to-date through the shipped
dependency checks (the on-disk `.deps` list, the source hash, the
stored output hashes), so zero conversions happen and each output is
byte-identical to the cold build's. The leg deliberately uses a new
session per walk: the shipped `queue_task` only queues tasks whose
status is `New`, so a second `build` on the same session re-queues
nothing ("Found: 0 tasks") and exercises no mechanism at all. Probe
sample from one run (printed only): cold 5.83 ms → incremental 3.99
ms — the fresh session re-walks and re-hashes every source, so at this
12-file scale the saving is small; the counter and the digests are the
contract.

## Editor load

A fresh `BwResource` opened over the output tree plus
`enable_editor_default_root` plays the editor's load face: every
artifact answers `file_exists`, resolves as `FileType::File`, and reads
back through `MultiFileSystem::read_file` with bytes equal to the
source. The loaded digests equal the cold build's digests. Probe sample:
0.76 ms for the 12 artifacts (printed only).

## Reproducibility

A second independent cold build (fresh temp tree, same fixtures, plain
copy converter) produces byte-identical outputs for all 12 assets and
identical digests — `BASELINE-RERUN files=12 digests_equal=true`.

## Findings

**The batch driver's task summary mislabels a no-op rebuild.** The
`task_up_to_date_count` hook is driven off `TaskRecord::up_to_date`
(default `true`), but `on_pre_convert` clears it before the
`process_conversion` early-out returns, so the summary line can report
`12 converted, 0 up-to-date` on a no-op rebuild. The real skip is
observable at the converter seam (zero additional calls), which is
what the test asserts. This is a reporting-label quirk in the shipped
driver, not a rebuild that re-ran conversion work.

**A second `build` on the same session is a no-op.** `queue_task`
(asset_compiler.rs) only queues tasks whose status is still `New`;
after the first build every task is `Done`, so the same session's
second walk reports `Found: 0 tasks` and nothing else happens. An
honest incremental measurement must register a fresh session over the
persistent trees — which is what this test (and OPT-13-01-02's cache
walks) do.

## Gates

- `cargo test -p asset-pipeline`: 384 lib + 2 + 2 integration passed,
  0 failed (the incremental-leg correction reruns `build_baselines`
  alongside the OPT-13-01-02 target).
- `cargo fmt -p asset-pipeline -- --check`: clean.
- `cargo clippy -p asset-pipeline --all-targets -- -D warnings`: no lint.
