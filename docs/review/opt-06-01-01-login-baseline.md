# OPT-06-01-01 记录登录与资源下载基线

Acceptance: **保存峰值、拒绝率和延迟**。本行记录基线，不改调度或下载策略。

## 面

`crates/load/src/login_baseline.rs` 组合已有面，记录三族数字：

- **峰值** — 登录 hurricane 的 online 序列最大值（`online_peak`），固定 seed 可复现。
- **拒绝率** — FD 预算拒绝的 bots 占 ramp 试图接纳的比例（`login_refusal_rate = refused / (admitted + refused)`）。沿用 `LoginAggregate` 已有的 `login_refused` 计数；`login_success_rate` 仍是参考的 admitted/attempts 语义（离线面每个 admit 都发出一次 logon，故恒 1），拒绝率单列以免把「发出后失败」和「预算拒绝」混为一谈。
- **延迟** — 资源下载的传输时间（`download_ticks`）：参考 `ServerDownloads::add_to_bundle` 每 mini-tick 至多写一个 bundle 的载荷（`BUNDLE_BYTES`），故 `ceil(payload / BUNDLE_BYTES)` 个 mini-tick 是分块强加的确定性地板，与 server-download 面所钉的块序同出处。队列驻留字节（`download_queue_bytes`）一同记录。

**读法警告**：离线面是模拟时间，**没有墙钟登录往返**。这里记录的延迟是下载分块的 tick 地板，不是网络 RTT；报告如此注明，避免把确定性地板误读成实测往返。与 OPT-05-01-01 一样，机器相关的墙钟数字不设带。

## 记录的运行（2026-11-26，本树 tip `af9041f`）

固定输入：保存的 `tests/load/scenarios/login-hurricane.xml`（seed `20261123`，保存并发 2000，ramp 200，harness 信封），FD 预算 `500 * FD_PER_BOT + FD_PER_BOT`，下载载荷 4096 字节。

| 面 | 值 |
|---|---:|
| `login_attempts` | 500 |
| `login_admitted` | 500 |
| `login_refused` | 177 |
| `login_success_rate` | 1 |
| `online_peak` / `online_final` | 500 / 500 |
| `login_refusal_rate` | 0.2614475627769572 |
| `download_payload_bytes` | 4096 |
| `download_ticks` | 4 |
| `download_queue_bytes` | 4096 |

保存夹具：`docs/review/fixtures/load/login-baseline.report.json`。5 个新单测钉住 ceil 分块、队列字节、峰值/拒绝记录、固定 seed 可复现和预算拒绝臂。

## 门禁

- `cargo test -p load --lib login_baseline`：5/5 绿。
- `cargo test -p load`：全绿。
- clippy `-D warnings` 与 rustfmt `--check`：净（提交前跑）。
