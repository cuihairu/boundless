# BW-00-05-06 复核台账完整性 — 记录

- **任务编号**: BW-00-05-06（lane BW-00-05 功能覆盖台账，todo.md 行 149）
- **验收**: 每个启用目标有运行证据或显式缺口。

## 结论

台账 **174 行逐行对账，0 未对账**。每行落入三类之一：

| 类 | 行数 | 判据 |
|---|---|---|
| 运行证据 | 13 | covered 5 + partial 8，fixture 全部存在且可解析 |
| 显式缺口 | 86 | blocked 83 + excluded 3，note 全部非空 |
| 排期（未启用） | 75 | pending，task 全部为具体任务号（`task=pending` 0 行） |

复核命令：`python3 docs/review/tools/check_coverage_ledger.py --audit`，
输出 `audit OK: 174 rows = 13 evidence + 86 explicit gap +
75 scheduled (all indexed)`，rc=0。

## 复核方法

`--audit` 在 `validate()`（字段词汇、夹具存在性、(target,dir) 唯一、
`origin=make` 79 项与 `make_inventory` 双向 1:1）之后，**按行**重述验收
口径，不依赖人工记忆：

- `covered`/`partial` → 必须有 fixture 且每个路径在仓内可解析（运行证据）；
- `blocked`/`excluded` → 必须有 note（显式缺口）；
- `pending` → task 必须已索引（不得仍是 `pending`，否则该行是悬空缺口）。

任何一行落入不了三类，审计即报错返回 1。

## 运行证据面（13 行）

| 目标 | 状态 | 证据（fixture） |
|---|---|---|
| cstdmf | covered | `docs/review/data/bw00-02-02-original-unit-test-results.txt` |
| cstdmf | partial | `crates/foundation/src/{ids,time,time_queue}.rs`、`crates/server-runtime/src/task_manager.rs` |
| entitydef | covered | `bw00-02-02-original-unit-test-results.txt` |
| math | covered | `crates/foundation/src/math` |
| resmgr | covered | `bw00-02-02-original-unit-test-results.txt` |
| resmgr | partial | `crates/resources/src/{datasection,bwresource,multi_file_system}.rs` |
| server | partial | `crates/server-runtime/src/{config,server_app,signals,shutdown,tick}.rs` |
| unit_test_lib | partial | `bw00-02-02-original-unit-test-results.txt` |
| baseappmgr | partial | `bw00-02-03-baseappmgr-start.log` |
| cellappmgr | partial | `bw00-02-03-cellappmgr-start.log` |
| dbappmgr | partial | `bw00-02-03-dbappmgr-start.log` |
| bwmachined2 | partial | `bw00-02-03-baseappmgr-start.log` |
| package-daily.sh | covered | `.github/workflows/package-daily.yml` |

说明：`covered` 且 fixture 指向原版单测记录的 5 行是 BW-00-05-01 的种子口径
（原版单测通过＝该目标的参考行为有证据）；`partial` 行指向 Boundless 侧已
落地的代码或启动日志。全部 13 行的 fixture 路径由校验器逐路径 `exists()`
检查。

## 显式缺口面（86 行，全部有 note）

| origin | 行数 | 缺口原因（登记处） |
|---|---|---|
| cmake | 58 | CMake/Windows-only（BW-00-01-02），Linux 无原生 CMake 路径，待 Windows 真机（BW-00-02-05/BW-15-03） |
| release | 12 | 参考 Windows 发行脚本 + 无定义 rpm 目标（BW-00-05-05） |
| make | 5 | `bigworld.key`/`loginapp.privkey` 缺失（BW-00-02-03/05）、twisted 模块、serviceapp 随 baseapp 阻塞 |
| gap | 8 | 参考树/依赖侧缺口，逐项来源+阻塞原因（BW-00-05-05） |
| excluded | 3 | BW-00-01-04 决策：CppUnitLite2 不复现、png 由 image crate 替代、zip 由 flate2 替代 |

`excluded` 三行的 note 均引 BW-00-01-04 决策，属**有意不做**而非遗漏。

## 排期面（75 行，未启用）

`pending` 75 行（make 59 + release 2 + resource 14）**全部带任务号**，是
「尚未启用」的未来清单，不是悬空缺口：Boundless 侧对应 lane 尚未实施，
启用时该行按验收转 covered/partial 并补 fixture。审计拒绝 `task=pending`
的 pending 行，故该面 0 悬空。

## 完整性判据

- **Make 面**：79 项与 `make_inventory` 现跑双向 1:1，类型逐一吻合（校验器
  逐条报错，本次 0 报）。
- **CMake 面**：111 个数组工程名 + 4 个拼接单测工程名逐名覆盖，0 未覆盖
  （make 同名者共用 make 行、纯第三方工程不建行、16 个导出器版本变体经 3
  个族行 note 枚举）——见 BW-00-05-03。
- **唯一性**：(target, dir) 无重复（校验器强制）。
- **缺口可追溯**：每个 blocked/excluded 行有 note；8 个 `origin=gap` 行
  逐项有来源与阻塞原因——见 BW-00-05-05。

## 校验与门禁

- `python3 docs/review/tools/check_coverage_ledger.py --audit`：
  `ledger OK: 174 rows` + `audit OK: 174 rows = 13 evidence + 86 explicit
  gap + 75 scheduled (all indexed)`，rc=0。
- 默认校验（含 `origin=make` 79 项双向 1:1）rc=0；`--schema-only` rc=0；
  `--scaffold` 空；`py_compile` 过。
- 本项纯台账/工具；未触任何 crate。

## 边界声明

lane BW-00-05 六项至此全清（01 字段契约 → 05 发行物/SDK/缺口 → 06 复核）。
台账此后是**活文档**：任一 target 被启用/阻塞，改对应行的 status/fixture/note，
重跑 `--audit` 即可；新增行经 `--scaffold` 提示。

## 状态

- [x] 174 行逐行对账，0 未对账
- [x] 证据面 13 行全部有可解析 fixture
- [x] 缺口面 86 行全部有 note（含 3 行有意排除决策）
- [x] 排期面 75 行全部已索引（0 悬空）
- [x] 校验绿（`--audit` rc=0）