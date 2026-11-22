# BW-00-05-03 客户端和工具目标索引

> 证据面（映射验收：编辑器、导出器及离线工具均有索引）。

客户端与工具目标大多是 Windows 构建面产品，登记在 BW-00-01-02 的 CMake 目标组里（`client`、`tools`、`exporters`、`assetprocessor`、`navgen`、`extras`、`bwentity` 及其 `_blob` 变体；`server` 组在 BW-00-05-02 已索引）。本索引逐产品列出：

- **类别**——按用途分 `客户端` / `编辑器` / `导出器` / `离线工具` / `资产管线` / `资产转换器` 等；
- **索引条目**——`任务号` + `夹具`，或 Windows 缺口来源（本机无 MSVC 产物，由 windows-latest CI 承载）。

## 汇总

| 项 | 数 |
| --- | --- |
| 索引行（产品 41 + 组说明 4） | 45 |
| 已映射（有任务号+夹具） | 2 |
| 缺口（Windows 面 / 未实现，带来源） | 43 |
| 类别 | 客户端 1、编辑器 5、导出器 10、离线工具 5、资产管线 8、资产转换器 8、资源打包 1、运行时环境 1、示例 1、工具基座 1、组（无独立产物） 4 |

编辑器 5 项，均有索引条目 5 项。
导出器 10 项，均有索引条目 10 项。
离线工具 5 项，均有索引条目 5 项。

未索引 = 0（每行均有 `任务号`+`夹具` 或具体缺口来源）。

## 客户端（1）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `bwclient` | `client` | client | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |

## 编辑器（5）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `editor_shared` | `tools/editor_shared` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `modeleditor_core` | `tools/modeleditor_core` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `worldeditor` | `tools/worldeditor` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `modeleditor` | `tools/modeleditor` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `particle_editor` | `tools/particle_editor` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |

## 导出器（10）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `animationexporter_max2010` | `tools/animationexporter/max2010_exporter` | exporters | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `animationexporter_max2011` | `tools/animationexporter/max2011_exporter` | exporters | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `animationexporter_max2012` | `tools/animationexporter/max2012_exporter` | exporters | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `mayavisualexporter_maya2010` | `tools/mayavisualexporter/maya2010_exporter` | exporters | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `mayavisualexporter_maya2011` | `tools/mayavisualexporter/maya2011_exporter` | exporters | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `mayavisualexporter_maya2012` | `tools/mayavisualexporter/maya2012_exporter` | exporters | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `mayavisualexporter_maya2013` | `tools/mayavisualexporter/maya2013_exporter` | exporters | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `visualexporter_max2010` | `tools/visualexporter/max2010_exporter` | exporters | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `visualexporter_max2011` | `tools/visualexporter/max2011_exporter` | exporters | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `visualexporter_max2012` | `tools/visualexporter/max2012_exporter` | exporters | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |

## 离线工具（5）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `assetprocessor` | `tools/assetprocessor` | assetprocessor | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `navgen` | `tools/navgen` | navgen | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `offline_processor` | `tools/offline_processor` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `batch_compiler` | `tools/batch_compiler` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `jit_compiler` | `tools/jit_compiler` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |

## 资产管线（8）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `compiler` | `tools/asset_pipeline/compiler` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `conversion` | `tools/asset_pipeline/conversion` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `dependency` | `tools/asset_pipeline/dependency` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `discovery` | `tools/asset_pipeline/discovery` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `test_compiler` | `tools/asset_pipeline/compiler/test_compiler` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `test_converter` | `tools/asset_pipeline/compiler/test_converter` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `compiler_unit_test` | `tools/asset_pipeline/compiler/unit_test` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `dependency_unit_test` | `tools/asset_pipeline/dependency/unit_test` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |

## 资产转换器（8）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `bsp_converter` | `tools/asset_pipeline/converters/bsp_converter` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `effect_converter` | `tools/asset_pipeline/converters/effect_converter` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `texformat_converter` | `tools/asset_pipeline/converters/texformat_converter` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `texture_converter` | `tools/asset_pipeline/converters/texture_converter` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `hierarchical_config_converter` | `tools/asset_pipeline/converters/hierarchical_config_converter` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `space_converter` | `tools/asset_pipeline/converters/space_converter` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `visual_processor` | `tools/asset_pipeline/converters/visual_processor` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |
| `primitive_processor` | `tools/asset_pipeline/converters/primitive_processor` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |

## 资源打包（1）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `res_packer` | `tools/res_packer` | extras | BW-00-01-01 | bw00-01-build-targets | 缺口 | 资源打包工具；无对应实现与证据 |

## 运行时环境（1）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `process_defs` | `tools/process_defs` | extras | BW-00-02-01 | bw00-02-01-runtime-env | 已覆盖 | 运行时环境与进程定义（Make 侧已映射） |

## 示例（1）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `simple_python_client` | `examples/client_integration/python/simple` | extras | BW-00-02-04 | bw00-02-04-client-login | 已覆盖 | 示例登录驱动与实体脚本（跨 BW-00-05-04 示例索引） |

## 工具基座（1）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `tools_common` | `tools/common` | tools | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 windows-latest CI 承载（BW-00-02-05 复测） |

## 组（无独立产物）（4）

| 目标 | 目录 | 目标组 | 任务号 | 夹具 | 状态 | 索引条目 |
| --- | --- | --- | --- | --- | --- | --- |
| `bwentity` | `build/cmake/BWConfiguration_bwentity.cmake` | bwentity | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | 组内仅库依赖（cstdmf、math、connection、connection_model、entitydef、network），无独立产物；本组索引随其承载面 |
| `client_blob` | `build/cmake/BWConfiguration_client.cmake` | client_blob | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | 打包 blob 变体（BW_BLOB_CONFIG=ON 后 INCLUDE 基座配置，本身不登记项目），随基座组索引 |
| `exporters_blob` | `build/cmake/BWConfiguration_exporters.cmake` | exporters_blob | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | 打包 blob 变体（BW_BLOB_CONFIG=ON 后 INCLUDE 基座配置，本身不登记项目），随基座组索引 |
| `tools_blob` | `build/cmake/BWConfiguration_tools.cmake` | tools_blob | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | 打包 blob 变体（BW_BLOB_CONFIG=ON 后 INCLUDE 基座配置，本身不登记项目），随基座组索引 |

## 可追溯性

- 行基：`reference/bw00_01_cmake_targets.json`（CMake 组）。
- 索引：`reference/bw00_05_03_client_tools_index.json`。

生成：`client_tools_index.py` + `client_tools_index_doc.py`。
