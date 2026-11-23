# BW-00-05-06 台账完整性复核

> 证据面（复核验收：每个启用目标有运行证据或显式缺口）。

复核把 BW-00-05-01 的台账逐条对回 BW-00-01-01 的组件清册 TSV（`name`+`dir`+`type` 键），并核对启用计数与索引状态。

## 检查项

| 检查 | 结果 | 明细 |
| --- | --- | --- |
| 清册与台账一一对应 | ✅ PASS | tsv 79 行 / ledger 79 行 / 去重后 tsv 79、ledger 79 |
| 启用计数一致 | ✅ PASS | tsv in_build 77 / ledger in_build 77 |
| 启用目标均有证据或显式缺口 | ✅ PASS | 启用 77 / 有证据 44 / 显式缺口 33 / 未索引 0 |
| CMake 组全部有索引 | ✅ PASS | cmake 11 组 / 未索引 0 |

整体判定 `all_ok=true`。

## 汇总

| 项 | 数 |
| --- | --- |
| 清册组件 | 79 |
| 台账行 | 79 |
| 启用目标（in_build=1） | 77 |
| 其中有运行证据（已覆盖/部分+夹具） | 44 |
| 其中显式缺口（带来源） | 33 |
| 未索引 | 0 |
| CMake 组 | 11 |

启用 77 = 有证据 44 + 显式缺口 33，未索引 0——验收满足。

## 复核范围与前序证据

- 台账字段与行基：`bw00-05-01-coverage-ledger.md`。
- 引擎库与服务端索引：`bw00-05-02-engine-server-index.md`。
- 客户端与工具索引：`bw00-05-03-client-tools-index.md`。
- 脚本与资源入口：`bw00-05-04-script-resource-index.md`。
- 发行物、SDK 与缺失资源：`bw00-05-05-artifact-sdk-check.md`。

## 可追溯性

- 复核：`reference/bw00_05_06_ledger_review.json`。
- 台账：`reference/bw00_05_01_coverage_ledger.json`；清册：`reference/bw00_01_make_components.tsv`。

生成：`ledger_review.py` + `ledger_review_doc.py`。
