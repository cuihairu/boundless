# OPT-15-01-02 — 采集各图形后端性能 (collect per-graphics-backend performance)

验收: **记录设备、驱动与帧时** (record the device, the driver and the frame
times).

`crates/client/tests/render_backend_baseline.rs` collects all three per
shipped graphics backend. The reference client's device is an
`IDirect3DDevice9` (`render_context.cpp:1164`); its four reference-side
backends (D3D9, navgen OpenGL, cocos2d, Awesomium) are recorded as facts in
`docs/review/bw15-06-01-graphics-backend-list.md` and are **not ports**.
This tree ships exactly one pixel-producing backend — the deterministic
software rasterizer behind `client::device::Device` — so the collection is
built as a registry keyed by backend: every entry in `BACKENDS` answers the
same three questions in one report row, and a backend BW-11 lands later
joins the table instead of needing a second harness.

## What a row records

- **Device** (structural, asserted): backend id and device type
  (`SoftwareDevice`), the presentation parameters (`DeviceConfig::default()`
  — 640×480, adapter 0, 307,200 px RGBA8), the frame order (indices
  1..=64 on one device object), the operational status (`Ok` throughout)
  and the leak conservation (`leaked() == 0`).
- **Driver** (probe data, never asserted): a software rasterizer has no
  GPU driver to query, so its "driver" is the execution environment —
  CPU model, logical cores, OS, kernel — recorded as text. That text is
  what makes two hosts' frame times interpretable against each other.
- **Frame time** (probe data, never asserted): per-frame wall clock around
  `check_device` → `step` → `present` over the fixed camera, 64 frames,
  printed one `FRAME-TIME` line per frame plus a `FRAME-TAIL` summary and
  written min/median/p99/max into the report.

## The Linux x86_64 record (committed at `tests/baseline/render-backend-baseline.tsv`)

| field | value |
| --- | --- |
| device | SoftwareDevice 640×480 adapter=0 (307,200 px RGBA8) |
| driver | 14 logical cores, Intel i9-10880H, kernel 7.0.0-44-generic, linux x86_64 |
| frames | 64 |
| frame time | min 12.72 ms / median 18.75 ms / p99 = max 24.21 ms |
| backend | `software` |

Frame times move with the host and the debug build; the doc records the
numbers of the run rather than a contract. What is asserted instead is
that the walk is the same fixed camera the end baseline pins: every
consecutive frame digest differs (the scene animates), frames 1..=8 and 64
reproduce the saved OPT-11-01-01 digests, and frames 1/7/31/64 reproduce
from the frame index alone — so the backend record and the end baseline
answer to one raster and cannot drift apart silently.

## The pending rows

`macos-arm64` and `windows-x86_64` carry `-` in every measurement field and
name their blocker (`BW-15-04`: the file-system and windowing slices; and
`BW-15-03`: `server-runtime/src/signals.rs` still uses Linux-only signal
APIs, so the workspace does not build there). The grammar check rejects
measurements on a pending row, an unknown backend id, duplicate
(platform, backend) pairs, non-numeric frame-time fields, and a measuring
platform missing a row for any shipped backend. Running the workload on a
compiling host regenerates that host's rows via `RENDER_BACKEND_OUT`;
nothing is asserted against another host's numbers.

## Gates

- `cargo test -p client` — 861 lib + all integration suites green
  (including the two new `render_backend_baseline` tests), 0 failed.
- `cargo fmt -p client -- --check`: clean.
- `cargo clippy -p client --all-targets -- -D warnings`: no crate lint
  (the pre-existing workspace-manifest `blowfish` unused-dependency warn
  is not from this change).