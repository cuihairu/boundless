# OPT-06-01-04 复跑重复登录、重连及旧客户端

Acceptance: **行为一致**。本行是复跑行（零代码改动）：把三条已交付面的测试族全量复跑，核对每条结果与记录的参考契约一致。

## 复跑面与结果（2026-11-26 本机）

### 重复登录（duplicate login）

`crates/server-runtime/tests/duplicate_auth.rs` — **5 passed / 0 failed**：

| 测 | 钉住的行为 |
|---|---|
| `repeated_authentication_rewrites_one_mapping_in_place` | 同账号二次登录原位改写唯一映射（不新增记录） |
| `a_concurrent_request_is_refused_without_polluting_the_record` | 并发请求被拒且不污染已有记录 |
| `an_in_progress_checkout_refuses_without_an_attempt` | checkout 进行中拒绝且不计一次尝试 |
| `the_logoff_of_a_relogoned_entity_leaves_one_record` | 重登实体的 logoff 后仍只剩一条记录 |
| `the_custom_extension_is_repeatable_for_one_account` | 自定义扩展对同账号可重复 |

### 重连（relogon / reconnect）

`cargo test -p server-runtime --lib -- relogon relogin` — **28 passed / 0 failed**，覆盖五族：

- `relogon.rs`（9 测）：活动 base 发起 attempt、第二次同实体拒绝、被拒回复拒绝重登、wait-for-destroy 等待后超时、baseapp channel 异常超时、logoff 期间等待改为重试登录、take-control 回复放行新会话、并发登录同账号单记录、非法回复码无回复收尾。
- `baseapp_relogon.rs`（13 测）：web 登录零 login key 免 pending、web 重登复用 proxy 不 logoff、完成 attempt 复用 proxy 并重新武装 pending、slot 清空收尾、单 slot 单 attempt、destroyed/missing base 等旧实体、送出 proxy 推迟 attempt、非 proxy base 拒绝、脚本结果缺失拒绝。
- `log_on_records.rs`（2 测）：attempt 追踪与完成、logoff 抹记录并旗标等待重登。
- `login_entity.rs`（2 测）：带活 base 的已加载实体路由到 relogon、checkout 中实体无 base 也路由到 relogon。
- `headless_client.rs`（1 测）：lifecycle 记录 login/enter/quit/relogin。

`cargo test -p load` 全绿（156 lib + 1 scenarios + 1 空 target），其中重连链三测：`drop_connection_cuts_without_telling_the_server`（断连不告知服务端、仅在线会话可断）、`a_retry_scheduled_bot_relogs_on_when_due`（到期重登）、`a_zero_retry_failure_drops_the_bot`（retry time 0 = 不排程直接丢弃）。`reconnect.xml` 保存场景经 `every_saved_scenario_loads` 加载（断连-重登语义在 lib 测面；该 kind 的离线 run face 尚未交付，`SmokeRun` 三通道为 hurricane/steady/dense）。

### 旧客户端（old client）

`crates/server-runtime/src/loginapp.rs` 版本面 — `cargo test -p server-runtime --lib -- version` **9 passed / 0 failed**：

- `a_different_version_is_rejected_before_the_params`：三个旧版本（2.9.0.1 / 2.8.0.0 / 0.0.0.0）全部 `BadProtocolVersion`，且在触碰凭据参数之前拒绝（版本先行读取，坏版本不解析 credentials）。
- `supports_is_exact_equality`：`supports` 是严格相等（版本是 wire tag 非区间）。
- `a_truncated_version_is_malformed`：截断版本流 = `MalformedRequest`。
- `current_version_roundtrips_in_wire_order`：四字节线上序 subpatch, patch, minor, major（反自然序）。
- 回放面双测（`a_version_mismatch_is_rejected` / `a_header_from_another_version_is_rejected`）与 watcher v1 协议拒绝同窗全绿。

## 行为一致判读

全部复跑结果与各面交付时记录的参考契约逐条一致（`relogon.rs`/`baseapp_relogon.rs` 的回复码-状态机、`duplicate_auth.rs` 的单记录不变量、`loginapp.rs` 的 `LOGIN_BAD_PROTOCOL_VERSION=65` 与版本先读序），**零代码改动、零回归**——本行无新交付面，验收「行为一致」即由这组复跑全绿给出。

## 门禁

- `cargo test -p server-runtime --lib`：**1701 passed / 0 failed / 1 ignored**。
- `cargo test -p server-runtime --test duplicate_auth`：5 passed；`--test fantasydemo_login`：5 passed（登录验收不回归）。
- `cargo test -p load`：156 + 1 + 1 全绿。
- `ci/load.sh`（`CI_LOAD_DURATION=15 CI_LOAD_CLIENTS=4`）：**3/3 场景出结果全 PASS**（harness 通道）。
