# BW-00-02-02 复跑原版单测 — 记录

- **任务编号**: BW-00-02-02（lane BW-00-02 原版运行基准，todo.md 行 101）
- **参考树**: `/home/cui/workspaces/BigWorld`（运行采集，未改动）；构建产物在 `programming/bigworld/build/cmake-{cstdmf,resmgr,entitydef}/`。
- **Boundless 模块**: 无（原版单测基准，供 BW-00-02 差分验收与 BW-00-05 覆盖台账引用）。
- **执行方式**: 按参考树 CTest 记录的规范命令（`CTestTestfile.cmake`）逐一复跑，捕获完整输出。

## 参编列表（实际已构建且可运行的单测可执行）

参考树声明 21 个 unit_test 目标（见 BW-00-01-01），但**仅 3 个已构建出可执行**（其余为源级目标未构建或仅库产物）：

| 测试可执行 | 构建目录 | 大小 |
|---|---|---|
| `cstdmf_test` | `build/cmake-cstdmf/` | 72 MB |
| `resmgr_test` | `build/cmake-resmgr/` | 55 MB |
| `entitydef_test` | `build/cmake-entitydef/` | 190 MB |

已声明未构建（CMake `add_test` 存在但无二进制）：`network_test`（cmake-network）、`bw_lsp_test`（cmake-entitydef）。

## 执行命令（规范，来自 CTestTestfile + 工作目录）

| 测试 | 工作目录 | 命令 |
|---|---|---|
| `cstdmf_test` | `lib/cstdmf/unit_test` | `build/cmake-cstdmf/cstdmf_test --root /home/cui/workspaces/BigWorld` |
| `resmgr_test` | `lib/resmgr/unit_test` | `build/cmake-resmgr/resmgr_test --root /home/cui/workspaces/BigWorld` |
| `entitydef_test` | `lib/entitydef/unit_test` | `build/cmake-entitydef/entitydef_test --res <unit_test/res>:<build/cmake-entitydef/bwpython-sample/python-res/game/res/bigworld>` |

（`--root` 供测试定位参考树根；`entitydef_test` 的 `--res` 需同时给本测试资源与 python-res 脚本树——否则 `could not initialise BWResource` / Python 相关失败。）

## 结果（全绿）

| 测试 | 运行 | 失败 | 时间 |
|---|---|---|---|
| `cstdmf_test` | 451 | 0 | 1.05 s |
| `resmgr_test` | 88 | 0 | 2.74 s |
| `entitydef_test` | 380 | 0 | 0.373 s |
| **合计** | **919** | **0** | — |

**完整输出**: `docs/review/data/bw00-02-02-original-unit-test-results.txt`（每次运行末尾的 `N tests run / no failures / Test time` 摘要，含 CTest 规范命令）。

## 边界与已知登记项

1. **仅 3/21 单测可运行**：参考树构建止于库阶段 + 部分单测（cstdmf/resmgr/entitydef）；`network_test`/`bw_lsp_test` 已声明未构建，其余 16 个 unit_test 目标（baseapp/cellapp/…/server 等）无对应可执行。复跑面以**实际构建出的 3 个**为准。
2. **entitydef_test 依赖 python-res**：缺 `--res` 的 python-res 树会导致 Python 相关失败（`PYTHON_6`、`UserDataObjectType::init()`、`Script_BinaryStream_basictypes_persistentOnlyTest` 等 78 项）——规范命令已含完整 `--res`。
3. **结果可复现**：本机（Linux x86_64）复跑，三次均 0 失败；二进制哈希见 BW-00-02-01 清单。

## 状态

原版单测复跑完成：参编列表 3 个可执行（cstdmf/resmgr/entitydef）、规范执行命令 3 条（CTest 记录）、结果全绿（451+88+380=919 测试，0 失败）；`network_test`/`bw_lsp_test` 已声明未构建，登记为缺口。
