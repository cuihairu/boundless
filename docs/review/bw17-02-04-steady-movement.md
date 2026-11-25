# BW-17-02-04 稳态移动场景 — 复核证据（陈旧行补账）

- **任务编号**: BW-17-02-04（todo.md BW-17-02 节）
- **验收**: 固定种子生成相同轨迹
- **Boundless 模块**: `crates/load/src/steady.rs`
- **交付**: `9e03e4c`（feat: port steady movement scenario，2026-11-24）；代码在树、
  在 main 祖先链，本席只补复核文档与台账勾账，不改代码。

## 形状

- `run_steady_movement(scenario, fd_limit, seed) -> SteadyLedger`：满并发进队
  → 每bot 喂登录结果与实体/space → 走完 warmup（不采样）→ 负载窗逐 mini-tick
  采样 (x, z, yaw)，每 bot 一条 `SteadyTrack`，队序即账序。
- **参照面**：已登录无控制器的 Bots 客户端在半径 10 的闲逛圆上自转
  （`server/tools/bots/client_app.cpp:1106-1134`），步长=每 mini-tick 一步
  （`TICK_PERIOD`），圆周期由构造速度抽样决定（`:385-387`）；速度来自
  BW-17-01-03 的种子 xorshift 流——固定种子固定每条周期，圆律是纯浮点
  客户端时钟函数，轨迹逐位可复现（θ = clientTime·2π/period）。
- **首样本位**：warmup 走但不采——第一点=负载窗第 1 个 mini-tick 的圆位
  （单测用 θ 律逐值复算比对），yaw 朝行进方向（θ + π/2）。

## 验收对应（单测 5 例）

| 用例 | 验收点 |
|---|---|
| the_fixed_seed_generates_the_same_trajectory | **固定种子相同轨迹**：两次运行 SteadyLedger 全等 |
| another_seed_redraws_the_trajectories | 异种子重抽速度→周期变→轨迹散（形状不变、速度仍在 [4,8]） |
| the_ledger_shape_follows_the_scenario | 每 bot 一轨、20 采样/2s、每点在 r=10 圆上 |
| the_fd_budget_bounds_the_fleet | 拒放行的 bot 不入轨 |
| the_first_sample_lands_one_tick_into_the_load_window | θ 律逐值：x=sin、z=cos、yaw=θ+π/2 |

## 证据

- `cargo test -p load`：**150 passed / 0 failed**。
- llvm-cov（fresh target，SF max 合并）：**steady.rs 101/101 = 100.00%**。
- clippy 0 告警；fmt 只作用叶子文件全路径。
- 卡点勘误：原行卡点「轨迹由 bot 在服务端世界移动产生——依赖活服务端」过时：
  轨迹由参照圆律的种子化纯函数产生（bot.rs 的 idle circle 即参照 `:1106-1134`
  的移植），库面即可验收；活服务端验证归 BW-17-02-06..08。
