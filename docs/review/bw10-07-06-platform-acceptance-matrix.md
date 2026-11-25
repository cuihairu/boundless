# BW-10-07-06: map end-to-end platform acceptance matrix

This is the acceptance matrix for the BW-10-07 fixed map (the two-chunk world
with height variation, a hole, an obstacle and a Portal). The section's
contract is "bind every applicable reference target, per platform, with a
result and evidence". This document is the evidence: every row names the
target, the platform it applies to, what was actually run here, and the
result. Anything that could not be run is marked *not runnable here* with
the reason and the owning item, not silently omitted.

Evidence was collected on 2026-11-25 on this host:

* host: `Linux-7.0.0-44-generic-x86_64-with-glibc2.43`, `x86_64`
* reference tree: `/home/cui/workspaces/BigWorld`
* reference binaries: `/home/cui/workspaces/BigWorld/game/bin/server/el7/`
* Boundless checkout: this worktree (`lane/bw19-docs`)

## Reference target inventory (Linux EL7, x86-64)

These are the original binaries that exist on this machine for the map's
acceptance surfaces (terrain, physics/navigation, cell, scene and the
process machinery). Each row records the SHA-256 of the actual binary and
the result of running it where the binary is a unit-test executable.

| Reference target | SHA-256 | Result |
| --- | --- | --- |
| `unit_tests/terrain_test` | `1e5ed57ddef0a128c7bca83dc3df5e34104430144f6837cdd679c1a8a52a8abb` | rc=0, 8 tests run, no failures |
| `unit_tests/physics2_test` | `32448a05519ce47892c2b14ba5767285e8d2c5398f6e560f7b9a452f57b2d05a` | rc=0, 44 tests run, no failures |
| `unit_tests/cellapp_test` | `5fa8de9cd2e61b17b238e3714d75bbd463008a76d0e9faeefb69a1e3ae877cc9` | rc=0, 62 tests run, no failures |
| `unit_tests/server_test` | `ac426590a7e4dce4295a7ce459cf5df9c9a506670b6bbaa3cb8bfdc47bad939f` | rc=0, 5 tests run, no failures |
| `unit_tests/scene_test` | `474a4a26608901e0bd84e64ad151f73a1b07c14ff625127a4396d8225b4e7da1` | rc=0, 5 tests run, no failures |
| `tools/bwmachined2` | `3bdb7f2bda77d6c4fcf1e2eadd2868c54b84eed2d46a433fc1a5a7dcb036d022` | EL7 x86-64 executable; presence verified (not launched — it needs the server process group) |
| `tools/res_packer` | `19cfec7a828e28432dce0f351cd7fc9184038627148d0884f80af5ab503bc044` | EL7 x86-64 executable; presence verified |

The original navgen graphical tool (`tools/navgen`) is **Windows-only MFC**
(`BW-10-05-01`: CMake target `navgen` is WIN32/MFC). There is no EL7 `navgen`
binary in the install tree. The original Linux route for generated map data
is the library-level generator plus the reader harnesses recorded by
BW-10-05-04, BW-10-07-02 and BW-10-07-03, not an installed executable.

## Boundless native targets

| Platform | Command | Result |
| --- | --- | --- |
| Linux x86-64 (native) | `cargo test -p waypoint -p terrain -p physics -p chunk` | all suites green: chunk 52 lib + 11 integration, terrain 80 + 9 + 1, physics 44 + 2 + 4 + 3, waypoint 50 + 8; 0 failures |
| Windows x86-64 (cross check) | `cargo check -p platform --target x86_64-pc-windows-gnu` | 0 errors (3 warnings, all pre-existing workspace/manifest warnings) |
| macOS arm64 (cross check) | `cargo check -p platform --target aarch64-apple-darwin` | **21 errors**: `libc` lacks the Linux-only names the crate uses un-gated — `epoll_event`, `EPOLL_CTL_ADD`/`_DEL`/`_MOD`, `EPOLLERR`/`_HUP`/`_IN`/`_OUT` (in `crates/platform/src/poller.rs`) and `Ioctl`, `IP_RECVERR`, `MSG_ERRQUEUE`, `sock_extended_err` (in `crates/platform/src/socket.rs`); owned by BW-15-04 |

The cross checks compile, they do not execute: there is no Windows runner and
no macOS runner on this host. BW-15-03 and BW-15-04 own real execution on
those platforms; this matrix does not claim a green result there.

## Map-route coverage by item

| BW-10-07 item | Route | Evidence | Platform |
| --- | --- | --- | --- |
| 07-01 fixed map and expected values | original EL7 generator, 139 truth lines replayed by `crates/waypoint/tests/fixed_map.rs` | terrain lane `059b0a8`/`8e074d7`, re-verified on this host (9/9) | Linux EL7 / Rust Linux |
| 07-02 editing and generation | original `navgen` generator + original reader, 34 probes in `genmap.rs` | terrain lane `c3b005a`/`2a665d2`, re-verified (3/3) | Linux EL7 / Rust Linux |
| 07-03 server-side load | original bind harness, 183 truth lines in `genmap_load.rs` | terrain lane `d6d8701`/`3cfe5e5`, re-verified (2/2) | Linux EL7 / Rust Linux |
| 07-04 client query | original client query harness, Phase 1 | terrain lane in flight (`9e9f12f`, `413bcee`); not present on this lane | Linux EL7 / Rust Linux |
| 07-05 bot traversal | `load::Bot` + two cell regions + two AoI worlds | this lane `dddfd21`, `tests/bot_cross_cell.rs` 2/2 | Rust Linux (no live server) |
| 07-06 this matrix | reference target hashes + run results, in-tree tests + cross checks | this document | see rows above |

The map fixtures for 07-01..03 live on `lane/bw10-terrain` and are not yet in
`main`; the re-verification used a temporary worktree on that lane (removed
after the run). This lane's own `crates/waypoint` carries the reader half
(navmesh decode: 50 lib + 8 integration green).

## Gaps and ownership

* **Windows / macOS execution** — no runner here. Owned by BW-15-03/04; the
  macOS cross check above is a concrete blocker for BW-15-04 (Linux-only
  `epoll`/socket names in `platform::poller` and `platform::socket` are not
  `cfg`-gated for `aarch64-apple-darwin`).
* **Original `navgen` graphical entry** — Windows-only MFC, not installed in
  the EL7 tree; no Linux binary to bind. Its Linux-equivalent route is the
  generator libraries and reader harnesses above.
* **Live two-CellApp hand-off** — no live server process group was started;
  07-05 uses an in-process two-cell harness. Real process hand-off is a
  platform run item, not claimed here.
* **Reference binary digests** — the hashes above pin the exact binaries
  used. A rerun on a different install must re-hash before comparing results.

## Verdict

For the platforms that can be executed here (Linux x86-64, both the original
EL7 binaries and the Rust workspace), every applicable target has a recorded
result and a reproducible evidence trail. Windows and macOS are reported as
not executable here with their owning items, rather than reported green.
