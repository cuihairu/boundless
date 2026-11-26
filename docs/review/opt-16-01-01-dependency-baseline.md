# OPT-16-01-01 — Dependency and incremental-build baseline

The acceptance: **locate the rebuild sources and their cost**
(`定位重编源及耗时`). The workspace dependency graph is captured
with `cargo metadata --no-deps`; the harness derives each crate's
downstream *fan-out* — the set of crates cargo must recompile when
that crate changes — and checks it against real incremental builds
(touching one source file's timestamp, content unchanged).

## The graph

29 workspace crates. The five biggest rebuild sources:

| Crate | Fan-out | What it touches |
| --- | --- | --- |
| `foundation` | **24** | nearly the whole workspace, including `db-postgres`, `server-runtime`, `bots` |
| `resources` | 14 | file-system and archive consumers |
| `physics` | 8 | scene, chunk, client, server side |
| `platform` | 7 | runtime and app crates |
| `terrain` | 6 | map/nav/space consumers |

A leaf (`bots`) has an empty fan-out — no workspace crate depends on
it. The graph is a DAG and the fan-out is strictly downstream.

## The measured incremental rebuilds

`cargo build --workspace --offline`, warm target directory, one
source timestamp touched per run (mtime restored afterwards; no
content change), wall seconds from `/usr/bin/time`:

| Touched | Recompiled | Seconds | Graph prediction |
| --- | --- | --- | --- |
| `bots` (leaf) | **1** | **0.47** | 0 downstream + itself = 1 |
| `foundation` run 1 | **25** | **10.70** | 24 + itself = 25 |
| `foundation` run 2 | **25** | **9.94** | 24 + itself = 25 |
| `resources` | **15** | **14.83** | 14 + itself = 15 |

- **The graph predicts the rebuild exactly**: every measured count
  equals fan-out + 1, and the actual `Compiling` names for
  `foundation` matched the computed 24-crate set. Fan-out is the
  actionable metric; the seconds are a captured observation with
  machine-specific noise (foundation's two runs differ by 7%).
- **A hub touch costs ~22.8× a leaf touch** (10.70/0.47 s) and
  recompiles 25× as many crates. That is the argument for the next
  item: splitting or decoupling `foundation` (and then `resources`).
- **`resources` is slower than `foundation` despite a smaller
  fan-out** (14.83 s for 15 crates vs ~10 s for 25): the rebuilt
  crates themselves are heavier (db/postgres, compiled-space), so
  "crate count" alone is not the cost model. Any split must weight
  the consumers, not just count them.

## Harness and fixtures

`crates/asset-pipeline/tests/dependency_rebuild_baseline.rs` reads:

- `docs/review/fixtures/dep-graph/crates-graph.tsv` — 29 crate
  rows, captured by `cargo metadata --no-deps --format-version 1`;
  the raw JSON is kept alongside as `crates-graph.json`.
- `docs/review/fixtures/dep-graph/rebuild-samples.tsv` — the four
  measured runs above.

It asserts the fan-out sizes and the measured-vs-predicted counts;
the timings are recorded, not gated.

```sh
DEP_GRAPH_OUT=<abs path> cargo test -p asset-pipeline \
    --test dependency_rebuild_baseline -- --nocapture
```

`docs/review/fixtures/dep-graph/dependency-baseline.tsv` — 20 rows,
captured with the command above.
