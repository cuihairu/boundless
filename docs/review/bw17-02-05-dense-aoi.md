# BW-17-02-05 密集 AOI 场景 — 复核证据（陈旧行补账）

- **任务编号**: BW-17-02-05（todo.md BW-17-02 节）
- **验收**: 记录目标密度及可见数
- **Boundless 模块**: `crates/load/src/dense.rs`
- **交付**: `7ce12df`（feat: port dense aoi scenario，2026-11-24）；代码在树、
  在 main 祖先链，本席只补复核文档与台账勾账，不改代码。

## 形状

- `run_dense_aoi(scenario, spacing, aoi_radius) -> DenseAoiLedger`：`concurrency`
  个目标按 ⌈√n⌉ 见方网格密排（列主序 (x,z)），记录：
  - `density` = 目标数 / 网格延展面积（extent²）；
  - `positions` = 密排出生点；
  - `visible_counts` = 每 bot 半径内其他目标数（平方距离 ≤ r²，自排除）。
- **可见性=记录的期望值**：AOI 仲裁本身归 cell 席（BW-08）；本场景记录的是
  活跑时 enter/leave 喂给服务端该答出的期望可见数——布局是 (数量, 间距) 的
  纯函数，期望逐位可复现。
- 3×3 单位格 r=1.5 的期望可见数 `[3,5,3,5,8,5,3,5,3]`（角 3/边 5/心 8）。

## 验收对应（单测 4 例）

| 用例 | 验收点 |
|---|---|
| the_grid_packs_the_requested_density | 9 目标/9 平方单位=密度 1.0，格点坐标逐值 |
| the_visible_counts_follow_the_aoi_radius | **可见数**逐值（角/边/心） |
| a_sparse_packing_empties_the_view | 稀疏化（间距×2）→ 可见数全 0、密度四分 |
| the_layout_is_repeatable_for_any_count | **密度+布局可复现**；非平方数 5 目标落到同网格（各见 4） |

## 证据

- `cargo test -p load`：**150 passed / 0 failed**。
- llvm-cov（fresh target，SF max 合并）：**dense.rs 75/75 = 100.00%**。
- clippy 0 告警；fmt 只作用叶子文件全路径。
- 卡点勘误：原行卡点归入「无 Bot 运行时」簇——记录密度与期望可见数是库面
  纯计算（BW-17-03-01..04 报告层同口径），活服务端对照归 BW-17-02-06..08。
