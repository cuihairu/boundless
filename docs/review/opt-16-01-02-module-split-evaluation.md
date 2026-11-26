# OPT-16-01-02 — Module split evaluation

The acceptance: **compare the recompile cost of the same change before
and after a split** (`同修改前后重编耗时可比`). The evaluation ran in a
disposable copy of the workspace; the main tree is unchanged — this is
an assessment, not the split itself.

## The candidate

`foundation::time_queue` was chosen from the 16-01-01 consumption
survey (math 166 references, stream 97, ids 34, profiler 14,
time_queue 12, time 4). It is a narrow seam: only `mercury` and
`server-runtime` name it outside `foundation`, and the module uses one
internal seam (`crate::time::stamp_now()`), rewritten to
`foundation::time::stamp_now()` in the extracted crate.

The split in the copy:

- `crates/time-queue/` with the module body as `lib.rs` and
  `foundation = { workspace = true }`.
- `crates/foundation/src/lib.rs` drops `pub mod time_queue;`.
- `mercury` and `server-runtime` switch to the `time-queue` path
  dependency (`server-runtime` keeps `pub use time_queue as timers;`).

## Same change, two shapes

Warm target on the same filesystem (ext4 disk, not tmpfs), same
command `cargo build --workspace --offline`, one source timestamp
touched with no content change (mtime restored after each run), Cargo's
`Compiling` lines counted:

| Shape | Touched | Recompiled | Wall seconds |
| --- | --- | --- | --- |
| Before | `foundation/src/time_queue.rs` | **25** | **8.30** |
| After, run 1 | `time-queue/src/lib.rs` | **7** | **6.79** |
| After, run 2 | `time-queue/src/lib.rs` | **7** | **10.65** |

The two graphs are captured as fixtures and the harness derives the
counts from them:

- Before: `foundation`'s downstream fan-out is **24** → 25 crates.
- After: `time-queue`'s downstream fan-out is **6**
  (`mercury`, `db`, `server-runtime`, `db-postgres`, `test-support`,
  `boundless-server`) → 7 crates.
- **The split removes 18 crates from the rebuild set.**

The second after-run is slower than the first even though the crate
set is identical — the repeat is recorded to show that the ~57% wall
swing is machine noise, not a structural change. **The crate count is
the stable comparison; the best observed wall ratio 1.22× is a
captured observation, not a threshold.** An earlier before-run on a
tmpfs target measured 11.26 s / 25 crates; it is not used in the
comparison because the storage differs.

## Verdict

The split is **effective in rebuild scope**: a change to the same
timer-queue logic stops rebuilding `foundation`'s 24 downstream
crates, including `resources`, `scene`, `client`, `bots` and
`asset-pipeline`. The remaining seven crates are real consumers of
timers. The cost is one more workspace crate plus two path edges; the
public re-export (`server_runtime::timers`) keeps the downstream API
shape intact. The wall-time win on this small module is modest because
the surviving consumers include `db-postgres` and `test-support` —
this supports splitting *heavier* foundation modules by the same
pattern, not prioritizing `time_queue` for wall-clock reasons.

## Harness and fixtures

`crates/asset-pipeline/tests/split_evaluation.rs` reads:

- `docs/review/fixtures/dep-graph/crates-graph.tsv` — the before graph
  (29 crates).
- `docs/review/fixtures/dep-graph/split-graph.tsv` / `.json` — the
  experiment's graph (30 crates, raw capture trimmed to the same
  shape as `crates-graph.json`).
- `docs/review/fixtures/dep-graph/split-samples.tsv` — the three
  measured runs.

It gates the graph shape, both fan-outs, the 18-crate reduction and
measured-vs-predicted counts; timings are recorded.

```sh
SPLIT_EVAL_OUT=<abs path> cargo test -p asset-pipeline \
    --test split_evaluation -- --nocapture
```

`docs/review/fixtures/dep-graph/split-evaluation.tsv` — 15 rows,
captured with the command above.
