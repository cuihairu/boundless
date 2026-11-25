# BW-17-02-03 登录洪峰场景 — 复核证据（陈旧行补账）

- **任务编号**: BW-17-02-03（todo.md BW-17-02 节）
- **验收**: 可重复发送指定速率
- **Boundless 模块**: `crates/load/src/hurricane.rs`
- **交付**: `c71d7b2`（feat: port login hurricane scenario，2026-11-24）+ 后续
  `db8dcec`（login aggregate metrics 面）；代码在树、在 main 祖先链，本席只补
  复核文档与台账勾账，不改代码。

## 形状

- `run_login_hurricane(scenario, fd_limit, seed) -> HurricaneLedger`：按
  BW-17-02-02 执行器激活曲线逐秒放行 bot 进队（`Fleet::add_bot`），每秒一次
  mini-tick（顺带派发重试重登录），逐秒记录发送数与在线数。
- **发送速率即放行速率**：参照 Bots 客户端在构造函数发登录
  （`server/tools/bots/client_app.cpp:405`），每秒发送数=该秒放行数 + 队内
  重试重登录（`main_app.cpp:636-657`）——`per_second` 逐桶记录即速率曲线。
- **可重复**：固定种子 → `Fleet::with_seed` 的 xorshift 流固定名字计数与
  构造速度抽样，两次运行 `HurricaneLedger`（per_second/names/speeds/refused/
  online）逐位相等；换种子保持调度与名字、只重抽速度。
- FD 预算界速：`fd_open + FD_PER_BOT > fd_limit` 拒绝放行并计 `refused`。

## 验收对应（单测 4 例）

| 用例 | 验收点 |
|---|---|
| the_hurricane_sends_at_the_specified_rate | 逐桶速率 = 指定 ramp_rate 到 concurrency 封顶、warmup 为 0 |
| the_hurricane_is_repeatable_from_the_fixed_seed | **可重复**：同种子两次运行全 ledger 相等；异种子调度不变 |
| the_ledger_names_follow_the_fleet_counter | 名字序=放行序（`Bot_<n>_hurricane`） |
| the_fd_budget_bounds_the_send_rate | FD 预算拒放行计 refused |

## 证据

- `cargo test -p load`：**150 passed / 0 failed**。
- llvm-cov（fresh `target/llvm-cov-target`，按 SF max 合并）：
  **hurricane.rs 87/87 = 100.00%**（dense/steady 同跑 100%）。
- `cargo clippy -p load --lib --tests` 0 告警；fmt 只作用叶子文件全路径。
- 卡点勘误：原行卡点「无 Bot 运行时」已过时——`crates/load` 库面
  （bot/fleet/beeline/patrol/executor/report）即 BW-17-01-01..06 交付的运行时，
  本验收是库面场景定义与重放，不需要活服务端；活服务端联调归 BW-17-02-06..08。
