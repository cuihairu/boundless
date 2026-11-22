# BW-00-05-05 发行物、SDK 与缺失资源核查

> 证据面（核查验收：缺口有来源与阻塞原因）。

逐行列出参考树中的发行物与 SDK，并核查缺失资源。每行给出 `present`（路径是否在树内，实算）、`来源`（哪份证据确立该状态）与缺口时的阻塞原因——验收要求每个缺口都有 `来源` + `阻塞原因`。

## 汇总

| 项 | 数 |
| --- | --- |
| 核查行 | 13 |
| 树内存在（present） | 8 |
| 已覆盖（存在+证据基线） | 1 |
| 缺口 | 12 |
| 缺口带来源+阻塞 | 12 |
| 类别 | SDK 4、发行物 4、缺失资源 5 |

缺口 12 项全部带 `来源` + `阻塞原因`（12/12），验收满足。

## 核查明细

| 类别 | 项 | 路径 | present | 任务号 | 夹具 | 状态 | 来源 | 阻塞原因 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 发行物 | Linux 服务端二进制树 | `game/bin/server/el7` | 是 | BW-00-02-01 | bw00-02-01-runtime-env | 已覆盖 | bw00-02-01-runtime-env（59 安装文件 sha256）+ bw00-01-release-scripts | — |
| 发行物 | Windows 服务端包 | `game/bin/win32|win64` | 否 | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | bw00-02-05-windows-baseline | 阻塞：本机无 MSVC/msbuild/powershell 与 Windows 产物；由 windows-latest CI 的 root-build-probe 承载 |
| 发行物 | 客户端 SDK 三件套 | `client_sdk` | 是 | BW-00-01-03 | bw00-01-release-scripts | 缺口 | bw00-01-release-scripts | 阻塞：SDK 版本仅 0.0.1、产物仅 workflow artifact（client-sdk job，retention 14d）；新版无对应 SDK 发布面 |
| 发行物 | products-manifest.txt | `(stage 打包期)` | 否 | BW-00-01-03 | bw00-01-release-scripts | 缺口 | bw00-01-release-scripts | 阻塞：打包期产物不在树内（随 stage 逐文件 SHA256 生成）；已记录未构建项 cellapp_extension |
| SDK | typescript | `client_sdk/typescript` | 是 | BW-00-01-03 | bw00-01-release-scripts | 缺口 | bw00-01-release-scripts | 阻塞：版本 0.0.1；新版无对应 TypeScript SDK |
| SDK | unity | `client_sdk/unity` | 是 | BW-00-01-03 | bw00-01-release-scripts | 缺口 | bw00-01-release-scripts | 阻塞：含 PROTOCOL.md 协议约定；新版无对应 Unity SDK |
| SDK | unreal | `client_sdk/unreal` | 是 | BW-00-01-03 | bw00-01-release-scripts | 缺口 | bw00-01-release-scripts | 阻塞：仅 REVIEW.md 占位；新版无对应 Unreal SDK |
| SDK | generator (bwgen) | `client_sdk/generator` | 是 | BW-00-01-03 | bw00-01-release-scripts | 缺口 | bw00-01-release-scripts | 阻塞：Python 代码生成器；新版无对应 |
| 缺失资源 | game/bin/win32 | `game/bin/win32` | 否 | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | bw00-02-05-windows-baseline | 阻塞：Windows 构建面缺失（同 Windows 服务端包） |
| 缺失资源 | game/bin/win64 | `game/bin/win64` | 否 | BW-00-02-05 | bw00-02-05-windows-baseline | 缺口 | bw00-02-05-windows-baseline | 阻塞：Windows 构建面缺失（同 Windows 服务端包） |
| 缺失资源 | snapshot_helper 安装 | `game/bin/server/el7/server/commands/snapshot_helper` | 否 | BW-00-01-01 | bw00-01-build-targets | 缺口 | bw00-01-build-targets | 阻塞：该检出已声明该目标但未安装（el7/server/commands 无此文件）；仅清册有声明，无产物 |
| 缺失资源 | third_party/png | `programming/bigworld/third_party/png` | 是 | BW-00-01-01 | bw00-01-build-targets | 缺口 | bw00-01-build-targets | 阻塞：vcpkg 迁移后残留空壳组件，不再由 Make 构建 |
| 缺失资源 | third_party/zip | `programming/bigworld/third_party/zip` | 是 | BW-00-01-01 | bw00-01-build-targets | 缺口 | bw00-01-build-targets | 阻塞：vcpkg 迁移后残留空壳组件，不再由 Make 构建 |

## 关键事实

- **Linux 服务端二进制** `game/bin/server/el7` 存在（含 server/commands/ 的 clear_auto_load、consolidate_dbs、remove_db、sync_db、transfer_db），有 BW-00-02-01 安装文件 sha256 基线——是唯一的已覆盖发行物。
- **Windows 侧缺失**：`game/bin/win32` 与 `win64` 均不存在（BW-00-02-05 探测确认），阻塞在本机无 MSVC/msbuild 工具链，由 windows-latest CI 承载。
- **snapshot_helper 声明未装**：Make 清册声明该目标，但 `el7/server/commands/` 无该文件（BW-00-01-build-targets 已记）。
- **third_party/png|zip** 为 vcpkg 迁移后残留空壳，目录在但不再由 Make 构建。
- **客户端 SDK 四件套**（typescript 0.0.1、unity、unreal、generator）树内存在，但仅作 workflow artifact（client-sdk job，retention 14d）、版本仅 0.0.1，新版无对应发布面——按缺口记。

## 可追溯性

- 核查：`reference/bw00_05_05_artifact_sdk_check.json`。
- 既有发行面证据：`bw00-01-release-scripts.md`（BW-00-01-03）、`bw00-02-05-windows-baseline.md`、`bw00-01-build-targets.md`。

生成：`artifact_sdk_check.py` + `artifact_sdk_check_doc.py`。
