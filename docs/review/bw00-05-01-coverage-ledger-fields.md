# BW-00-05-01 建立覆盖台账字段 — 记录

- **任务编号**: BW-00-05-01（lane BW-00-05 功能覆盖台账，todo.md 行 144）
- **验收**: 每行含原版目标、启用条件、任务号、夹具及状态。

## 交付

1. `docs/review/data/bw00-05-coverage-ledger.tsv` — 覆盖台账，79 行（与
   `make_inventory.py` 的 79 个构建组件一一对应，行序=清单顺序）。
2. `docs/review/tools/check_coverage_ledger.py` — 台账校验器（字段值域、
   blocked/excluded 必填 note、夹具路径存在性、`origin=make` 与清单双向
   1:1 交叉核对、`--scaffold` 列出未登记组件、`--schema-only` 跳过清单交叉核对）。
3. 本记录。

## 字段契约（9 列，验收五字段 + 4 项账本列）

| 列 | 含义 | 取值来源 |
|---|---|---|
| `target` | 原版目标名（Makefile.rules `name`） | `make_inventory.py` 清单 |
| `type` | 组件类别 | 清单 `type`（空→`oneoff`，即 remove_db） |
| `dir` | 原版源目录，相对 `programming/` | 清单 `dir` |
| `origin` | 行来源：`make`/`cmake`/`release`/`resource`/`gap` | `make`=01-01 清单；`cmake`=01-02 CMake/Windows 面（03 登记）；`release`=01-03 发行物（05 登记）；`resource`=资源/脚本入口（04 登记）；`gap`=缺失资源/SDK 缺口（05 登记） |
| `enable` | **启用条件**：`-`（无条件）或逗号连接的特性令牌（`usePython`/`useOpenSSL`/… 或 `BW_*` CMake 宏） | 清单 `use_flags`（make 行）；CMake 宏族 per 01-02 |
| `task` | **任务号**：`BW-xx[-yy[-zz]]`（lane/小节/条目级均可，逗号连接）、`pending`（未映射，待 02-04）、`n/a`（刻意无任务，note 说明） | todo.md 条目 id；`pending`=本项骨架默认 |
| `fixture` | **夹具**：仓库相对路径（逗号连接），可运行验收或可追踪记录；`-`（尚无） | 测试路径 / `docs/review/*` 记录 / `docs/review/data/*` |
| `status` | **状态**：`covered`/`partial`/`pending`/`blocked`/`excluded` | 见下表语义 |
| `note` | 备注；`blocked`/`excluded` **必填** | 阻塞原因（许可证/真机）或决策引用（如 BW-00-01-04 替代表） |

## 状态语义

| 状态 | 含义 | 约束 |
|---|---|---|
| `covered` | 有可运行验收且对应 todo 条目已勾 | fixture 必填 |
| `partial` | 部分覆盖，余量已登记或任务在途 | fixture 必填 |
| `pending` | 已登记未映射（本项骨架默认，待 02-04） | fixture=`-` |
| `blocked` | 外部条件阻塞（许可密钥/Windows 真机/上游等待） | note 必填 |
| `excluded` | 决策不复现该原版目标（第三方被替代/不用） | note 必填，引用决策记录 |

## 骨架与种子

`make_inventory.py` 扫 93 条记录、79 个组件，本台账为每个组件建一行；
行序与清单一致（target, dir）唯一。79 行全以 `enable`=清单 use_flags、
`task=pending`、`fixture=-`、`status=pending`、`note=-` 起步——即**字段
契约先行**，映射（任务号+夹具+状态）是 02-04 的填入面。

已勾 todo 条目有证据的目标先落真实值（20 行），不空转：

- **基础库**：`cstdmf`→BW-01-01-01..03+BW-01-05-02..05（partial，
  allocator/basictypes/memhook 面待映射）；`math`→BW-01-01-04/05（covered）；
  `resmgr`→BW-01-03-01..06+BW-01-04-01（partial，access_monitor/auto_config/bdiff
  面待映射）；`server`→BW-01-04-02..05+BW-01-05-01（partial，time_keeper/bwservice
  余量待 BW-02/BW-04）；`unit_test_lib`→BW-00-02-02（partial，宿主库经复跑间接验证）。
- **原版单测**（BW-00-02-02 复跑 919 测试 0 失败）：`cstdmf`/`resmgr`/`entitydef`
  三个 `unit_test` 行=covered（451/88/380）；余 18 个 unit_test 行保持 pending
  （声明未构建，映射时补 note）。
- **服务端进程**（BW-00-02-03 部分证据）：`baseappmgr`/`cellappmgr`/`dbappmgr`/
  `bwmachined2`=partial（达 `Version 14.4.1` 持续运行 / `bwmachined2 -f` 启动）；
  `baseapp`/`cellapp`/`dbapp`/`loginapp`/`serviceapp`=blocked（缺
  `server/bigworld.key` 商业许可 / `server/loginapp.privkey`），note 引 00-02-03
  启动日志。
- **第三方**：`CppUnitLite2`/`png`/`zip`=excluded，note 引 BW-00-01-04 决策
  （Rust 测试框架 / image crate / flate2 替代；zip 读写面经 BW-01-03-03 覆盖）。

## 校验与门禁

- `python3 docs/review/tools/check_coverage_ledger.py`：**79 行全字段合法、
  与清单 79 组件双向 1:1、类型逐一吻合**，rc=0。
- `--scaffold`：空（无未登记组件）；`--schema-only`：rc=0（字段面独立可验）。
- `python3 -m py_compile` 两脚本通过；`make_inventory.py` 未改动（本项只新增
  校验器、台账与记录）。
- workspace 级 `cargo test` 未触及（本项纯文档/工具）；同前，`crates/db`
  未跟踪成员 + `platform` 在途测试为兄弟会话债务，非本项。

## 边界声明

本项交付**字段契约 + 全量骨架 + 有证据的 20 行**；不做系统性映射——
`BW-00-05-02` 完成 79 个 Make 组件的任务号/夹具/状态填入，`03` 补 CMake/
Windows 面（`origin=cmake` 新行），`04` 补 common/examples/FantasyDemo
（`origin=resource` 新行），`05` 补发行物/SDK/缺失资源（`origin=release`/`gap`
新行），`06` 复核。每填一批跑一次校验器（缺行由 `--scaffold` 列出）。

## 状态

- [x] 台账字段契约（9 列、值域、状态语义）
- [x] 全量骨架（79 行 = 清单 79 组件 1:1）
- [x] 有证据种子（20 行，仅勾行已证）
- [x] 校验器（字段+交叉核对+scaffold）
- [x] 校验绿（rc=0，双向 1:1，类型吻合）
