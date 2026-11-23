# BW-17-02-01 定义场景输入

- **任务编号**: BW-17-02-01（lane BW-17 Bots 压测，todo.md 行 1919）
- **验收**: 固定种子、账号池和并发配置可加载。
- **结论**: 满足并勾行。新 `crates/load` 域库（`Scenario` 输入定义 + XML 加载器）随本项交付：
  `tests/load/scenarios/` 下 6 个场景文档（登录洪峰/稳态移动/密集 AOI/跨 Cell/断线重连/长稳）
  均可经 `Scenario::load_xml` 加载；固定种子、账号池（generated/explicit）与并发量逐字段断言通过。

**参考锚点**: 原版 Bots 入口 `programming/bigworld/server/tools/bots/`（`bots_config.cpp` 经
`BWConfig`/DataSection 读 `bots/` 前缀选项）。本场景层是**压测新设计**（任务书 BW-17 头：
「在 `tests/load/scenarios/` 保存固定种子、账号池、并发量、爬升速率、预热及持续时间」），
仅沿用参考的配置读取风格（XML DataSection），字段语义按任务书定义。

## 1. 场景输入模型

`crates/load/src/scenario.rs`：

| 字段 | XML 路径 | 语义 | 必需 |
|---|---|---|---|
| `name` | `name` | 场景名（如 `login-hurricane`） | 是 |
| `fixed_seed` | `seed` | 固定种子（可复现运行的 RNG 种子） | 是 |
| `account_pool` | `accountPool` | 账号池（generated/explicit 两形态） | 是 |
| `concurrency` | `concurrency` | 峰值并发 bot 数 | 是 |
| `ramp_rate` | `rampRate` | 斜坡期每秒新增 bot（缺省 0 = 瞬时） | 否 |
| `warmup` | `warmup` | 预热秒数（缺省 0） | 否 |
| `duration` | `duration` | 持续秒数 | 是 |

`AccountPool` 两形态（`mode` 属性，缺省 `generated`）：

- `generated`（缺省）：`prefix`+`count` 生成 `<prefix><n>`（n ∈ 0..count），
  `password` 缺省 `""`（对齐参考 `bots_config` 的 `password` 默认值）。
- `explicit`：`accounts` 逗号分隔用户名表（非空必填），`password` 缺省 `""`。

`Scenario::count()` 返回池内账户数（generated=count；explicit=列表长度）。

## 2. 加载器与错误可定位

`Scenario::load_xml(raw) -> Result<Scenario, ScenarioError>`：`resources::datasection::parse`
（XML DataSection 解析，参考配置同款）→ `from_section` 逐字段抽取。`ScenarioError { path, message }`
携带**可定位路径**：顶层字段用自身名（`seed`/`duration`/…），池内字段用限定路径
（`accountPool/prefix`/`accountPool/count`/`accountPool/accounts`/`accountPool/mode`）；
数值解析失败与缺字段分别报「expected an unsigned integer」/「expected a value」；空必填串报
「expected a non-empty value」；未知 `mode` 报 `unknown mode`；malformed XML 归 `xml` 路径。

## 3. 实测

```
$ cargo test -p load
test result: ok. 16 passed; 0 failed   (lib: scenario::tests::*)
test result: ok. 1 passed; 0 failed    (tests/scenarios.rs::every_saved_scenario_loads)
```

- 16 个单元测试逐错误臂覆盖（缺字段/坏数字/空串/空列表/未知 mode/malformed XML/缺池…）+
  generated 与 explicit 两形态全字段断言 + 验收三字段（seed/账号池/concurrency）专项断言。
- 夹具遍历测试把 `tests/load/scenarios/` 六个文档逐个喂给加载器，任一不可加载即失败。
- **行覆盖 98.58%**（`cargo llvm-cov -p load`，282/286 行，函数 39/39 = 100%，门禁 ≥98%）。
- `cargo fmt --check` 净；`cargo clippy -p load --all-targets` 本 crate 0 警告。
- workspace 全绿（`cargo test --workspace` exit 0）；`test-support` 层门禁
  `workspace_rules_hold` 绿——`load` 登记 `LAYER_DOMAIN`，仅依赖 `resources`（同层域库）。

## 4. 边界

本项只交付**场景输入定义**与加载（验收即「可加载」）。执行器（爬升/预热/各阶段计时，
BW-17-02-02）与 Bot 运行时（BW-17-01）不在本项：前者消费本 `Scenario`，后者连接服务端。
本项不触碰服务端/客户端/mercury，也不生成任何压测结果（结果聚合属 BW-17-03）。
