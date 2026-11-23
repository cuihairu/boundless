# BW-00-04-06 — 实现差异报告

验收：注入字段、超时和顺序差异时分别可定位。

本任务为对照 lane 新增**差异报告**：把 fullstack lane（BW-00-04-04）写下的
两份证据——原版栈的证据与新版栈的证据——逐字段比较，把每一处差异定位到
字段路径并说明类型（`injected` / `omitted` / `timeout` / `order` / `value`），
而不是笼统地说"两者不同"。

- 引擎：`ci/diff_report.py`（`json <ref> <new>` / `trace <ref> <new>`；退出码
  0 一致 / 1 有差异 / 2 一侧缺失）。
- 驱动：`ci/diff-report.sh`（读 `$CI_OUT/fullstack/{ref,new}/*.json`）。
- 接线：`ci/changes.sh` fullstack 触发 + `ci.yml` fullstack job 同跑、
  harness job 同跑 `self-test`。

## 1. 差异类型与定位

| kind | 含义 | 定位示例 |
|------|------|----------|
| `injected` | 新证据独有的字段/事件 | `$.entityTypes[0].allProperties[0].injectedFlag` |
| `omitted` | 原版证据独有的字段/事件（镜像，防静默消失） | `$.events[int-arg].args` |
| `timeout` | 一侧没有任何证据（该次运行没产出，不是值不符） | `new evidence missing` |
| `order` | 同元素不同次序 | `$.events` / `$.entityTypes[0].allProperties` |
| `value` | 同字段不同内容 | `$.events[int-arg].args[0]` |

事件按**名字**匹配而非位置，因此注入一个订阅者会被点名，而不会让整列
事件集体"偏移"。`-IGNORABLE` 事件（BW-00-04-02）内的数值与 `<float>`
占位符互相匹配（运行时值不参与比较）。

## 2. 对照 lane 上实测（live）

fullstack lane 对三棵 fixture res 树产出原版/新版证据（逐字节等于捕获的
oracle，见 BW-00-04-04/05），差异报告对同一对证据逐字段比较：

| fixture | 报告 | 判定 |
|---------|------|------|
| full_def | 原版==新版（字段一致） | PASS |
| service_def | 原版==新版（字段一致） | PASS |
| entity_index | 原版==新版（字段一致） | PASS |

协议与回调场景只有原版证据（新半生产者未就绪），如实 SKIP 并点名缺失
生产者，不冒充比较成功：

- `diffreport-protocol` — mercury wire 生产者不存在（crates/mercury 无
  可运行字节入口）。
- `diffreport-callbacks` — python-host 回调生产者不存在（crates/python-host
  无 ScriptEventList）。

## 3. 自证（self-test，验收逐条）

`ci/diff-report.sh self-test` 逐条植入三种差异并断言被定位：

1. **注入字段**：新证据在 `allProperties[0]` 加 `injectedFlag`，报告
   `injected $.entityTypes[0].allProperties[0].injectedFlag`，rc=1。
2. **超时**：新证据文件缺失，报告 `timeout`，rc=2（不是值不符）。
3. **顺序**：回调 trace 中两个事件块互换，报告 `order $.events`，rc=1。

三条自证全过；自证用植入文件于 sandbox，植入的失败不写进共享 manifest。

## 4. 顺带修复

fullstack-compare.sh 结尾 `[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"` 在
LANE_RC 为 0 时短路，返回被测试的失败状态 1——健康 lane 反而以 1 退出、
拖红任务。改为显式 `if` 分支（diff-report.sh 同型同改）。

## 5. 观察（非本任务引入）

本改动零 Rust。本机全工作区 `cargo clippy --all-targets -- -D warnings`
在 `crates/resources`（既往任务完成的 crate，本任务未触碰）报 37 条预存
lint 发现（`manual_is_multiple_of`、`chunks_exact_to_as_chunks`、
可折叠 `if` 等）。CI 的 rust job 对**非 Rust 改动**以空 scope 跳过
fmt/lint/build/test（check-rust.sh：显式空 scope=无 Rust 变更），故本任务
的 clippy 门禁按 CI 语义为 SKIP，`cargo test --workspace --offline` 本机
全绿（rc=0）。resources 的预存 lint 属其所在任务的验收面，不在本任务范围。
