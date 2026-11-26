# BW-10-07-06 — the platform acceptance matrix

Acceptance: **每个适用原版目标有结果及证据** — every applicable
reference target has a result and evidence. The cluster ran on the
enabled reference platform(s); each target in the lane's domain was
executed on this machine and its output captured.

## The platform axis

The reference tree ships exactly one enabled binary platform:
`game/bin/server/` contains only `el7/`. There are no Windows binaries
in the tree, so the per-platform matrix is:

| platform | state | result |
|----------|-------|--------|
| Linux el7 | enabled — binaries present and runnable | every applicable target green (below) |
| Windows | not enabled — no reference binaries in the tree | no applicable target; nothing to run |

This matches the cluster gate 「绑定**已启用**的原版目标逐平台验收」:
the enabled platform is fully exercised; the absent platform has no
targets to bind.

## The matrix — BW-10 domain targets on Linux el7

All eight targets ran to completion on this machine (2026-11-25),
exit code 0, no test failures. Raw logs:
`docs/review/fixtures/platform/<target>.log`.

| reference target | BW-10 surface it backs | result |
|------------------|------------------------|--------|
| `terrain_test` | 地形/高度带（05 簇 flood、07-01 固定图坡带） | 8 tests, 0 failures |
| `physics2_test` | chunk 碰撞与 obstacle 扫掠（07-04 collide 面） | 44 tests, 0 failures |
| `scene_test` | chunk/scene 装载（07-03 服务端加载） | 5 tests, 0 failures |
| `math_test` | 几何基础（全 lane 共用的向量/平面/包围盒） | 44 tests, 0 failures |
| `resmgr_test` | chunk 资源加载与依赖（07-03 的 res 面） | 88 tests, 0 failures |
| `cstdmf_test` | foundation（debug/stream/memory 全 lane 地基） | 453 tests, 0 failures |
| `cellapp_test` | cell 移动/ghost 语义（07-05 Bot 穿越） | 62 tests, 0 failures |
| `unit_test_lib_test` | 测试框架自身（结果可信度的元证据） | 1 test, 0 failures |

**703 test cases total, 0 failures, every exit code 0.**

## Targets outside the lane, recorded as not applicable

`baseapp_test`, `baseappmgr_test`, `cellappmgr_test`, `db_test`,
`dbapp_test`, `dbappmgr_test`, `entitydef_test`, `loginapp_test`,
`network_test`, `pyscript_test`, `reviver_test`, `script_test`,
`server_test` validate other lanes' domains (entitydef → BW-04,
db → BW-09, network → BW-02, baseapp → BW-08, …). They are not part
of the 地图与地形 acceptance and are not claimed here. The cell side
of 07-05 is covered by `cellapp_test` above.

## Cross-links to the cluster's own reference-driven evidence

The unit-test binaries are the platform baseline; the cluster's
end-to-end acceptance additionally drove the reference **libraries**
directly through harnesses replaying recorded truth:

| cluster item | harness | truth log |
|--------------|---------|-----------|
| 07-01 固定测试地图 | `gen_fixed_map.cpp` | `gen_fixed_map.log` |
| 07-02 地图编辑和生成 | `gen_navgen_map.cpp` / `gen_navgen_paths.cpp` | `gen_navgen_map.log` / `gen_navgen_paths.log` |
| 07-03 服务端加载 | `gen_mapload.cpp` | `gen_mapload.log` |
| 07-04 客户端查询 | `gen_clientquery.cpp` | `gen_clientquery.log` |
| 07-05 Bot 穿越 | 四回调序引证 `cell.cpp:212/225`, `entity.cpp:4693/4791` | recorder trace in `bot_crossing.rs` tests |
| 05-04/05-05/05-06 生成与读取 | `gen_generate.cpp` / `gen_navpaths.cpp` | `gen_generate.log` / `gen_navpaths.log` |

Each ported surface was accepted against these recorded truths
(LOG-IDENTICAL replays where marked), so the matrix's green targets
and the cluster's item-level evidence back the same engine behaviours.

## Verification

- Evidence fixture: `docs/review/fixtures/platform/` — one log per
  target, header naming the binary and UTC run time, tail line
  recording the exit code.
- No product code changed in this item; the four gates stand as run
  for the cluster at 07-05 (server-runtime 1169 green, workspace
  0 failures, fmt/clippy clean).
