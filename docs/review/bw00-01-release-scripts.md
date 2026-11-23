# BW-00-01-03 发行脚本与 RPM 目标清册

> 证据面（BW-00-01-03 验收：标出独有交付物及版本）。

扫描自参考 `cuihairu/BigWorld` 的发行面：`scripts/package-daily.sh`、
`scripts/package-daily-client-sdk.sh`、`scripts/package-daily-windows.ps1`、
`scripts/build-windows-server.ps1`，以及驱动它们的 `.github/workflows/daily-build.yml`
（三个 job：`linux` / `windows` / `client-sdk`）。

源码树里**没有任何 `*.spec` 文件**（`find -name '*.spec'` 为空）：RPM 的 spec 由
`package-daily.sh` 在打包时于 `$STAGE/rpmbuild/SPECS/bigworld.spec` 现场生成，
再交给 `rpmbuild`。

## 版本源（single source of truth）

| 交付面 | 版本源 | 当前值 |
| --- | --- | --- |
| Linux 包 / Windows 包 | `programming/bigworld/vcpkg.json` 的 `"version"` | `14.4.1` |
| 客户端 SDK 三件套 | `client_sdk/typescript/package.json` 的 `version` | `0.0.1` |

服务端两条 lane 共用同一版本号；build tag 统一为 `daily<UTC YYYYMMDD>.<short-sha>`：

- Linux：`DEB_VER = <base>+daily<date>.<sha>`，`RPM_REL = 1.daily<date>.<sha>`
- Windows：`ver = <base>+daily<date>.<sha>`（与 Linux 对齐，便于两条 lane 的产物名并排比对）
- SDK：`TAG = daily<date>.<sha>`（不带 base 版本）

`short-sha` 取自 `git rev-parse --short HEAD`，取不到时回退 `norev`。

## 三条 lane 与独有交付物

三条 lane 都挂在同一个 48 小时新鲜度门上（`git rev-list --count --since='48 hours ago'`；
`workflow_dispatch` 无条件构建），产物全部只作 workflow artifact，`retention-days: 14`、
`if-no-files-found: error`。

### Linux：`scripts/package-daily.sh <out-dir>`

阶段目录 `pkg/opt/bigworld/{bin,res}`（`game/bin` + `game/res` 整树拷入），
先 `find ... -name '*.log' -delete` 剔掉单测断言转储。一份 staging 喂三种格式：

| 交付物 | 命名 | 版本字段 | 独有说明 |
| --- | --- | --- | --- |
| tar.gz | `bigworld-<DEB_VER>-linux-x86_64.tar.gz` | DEB_VER | 打包 `opt`，安装姿势 `tar -C / -xzf ...` |
| `.deb` | `bigworld_<DEB_VER>_amd64.deb` | `Version: <DEB_VER>` | `Architecture: amd64`；`Depends: libc6 (>= 2.17), libstdc++6, libmysqlclient21 \| libmariadb3`；`dpkg-deb --root-owner-group` |
| `.rpm` | `bigworld-<BASE_VER>-<RPM_REL>%{dist}.x86_64.rpm` | `Version: <BASE_VER>` / `Release: <RPM_REL>%{dist}` | `BuildArch: x86_64`、`License: BigWorld Open-Source Edition`、`AutoReqProv: no`（不自动探测依赖，与 .deb 显式声明一致）；`%files /opt/bigworld` |
| `pkg-version.txt` | — | DEB_VER | 供 workflow 取 `steps.pkg.outputs.ver` 拼 artifact 名 |

RPM 目标即 `rpmbuild -bb --define "_topdir $RPM_TOP" --define "dist %{nil}"
--define "bwsrc $PKG_STAGE" SPECS/bigworld.spec`：`dist %{nil}` 把 `%{dist}`
后缀置空，故产物名里没有平台后缀；spec 的 `%install` 用宏 `%{bwsrc}` 指回 staging 树。

artifact 名：`bigworld-daily-<DEB_VER>-linux-x86_64`。

该 lane 的 gate 是 `make -s -j"$(nproc)" -rR` 之后 `make -s -rR bw-run-all-unit-tests`
（21 个模块）——单测不绿不打包。

### Windows：`build-windows-server.ps1` → `package-daily-windows.ps1`

`build-windows-server.ps1` 按目标逐个 MSBuild 出 Hybrid 配置（根 `CMakeLists.txt:22`
把配置类型强制成 `Debug;Hybrid`，没有 Release，故 `--config Release` 会全目标
MSB8013）；需要 `$env:DXSDK_DIR`（D3DX 9.29.952.8，`lib/math` 的 Windows 向量类型
是 D3DX 别名）。

`package-daily-windows.ps1` 产出**扁平**布局（不分 `bin/` + `python/`：嵌入解释器
不设 `Py_SetPythonHome`/`Py_SetPath`，从 `python313.dll` 自身目录找 home，
`Lib/` 必须与它同级，即 PCbuild 原生布局）：

| 交付物 | 命名 | 内容 |
| --- | --- | --- |
| zip | `bigworld-windows-server-<ver>.zip` | 11 个 Hybrid 可执行 + `python313.dll` + PCbuild 扩展模块 + BigWorld 的 Python 模块（`_bwlog.dll`、`bwengine_xml.dll`、`_bw_profile.dll`）+ `Lib/`（CPython 标准库，仓库内 `third_party/python/Lib`，2338 个文件，拷入使包自足） |
| `products-manifest.txt` | — | 逐文件 SHA256 + 字节数 + 名称；另记录构成事实：`MSVC 19.51 / Visual Studio 18 2026, x64`、`configuration: Hybrid`、`runtime crt: UCRT`（随 Windows 10+ 提供，不打包）、标准库文件数、**未构建项**（`examples/cellapp_extension`——Windows DLL 链接必须解析 `Entity::*`/`Controller::*` 等 cellapp 可执行持有的符号，需要导出决策） |
| `pkg-version.txt` | — | `ver` |

BigWorld 核心库（`cstdmf`、`network`、`server`…）在此轨道为静态链接，
**不产出也不存在** `cstdmf.dll` / `network.dll` 这类文件——这一点写进 manifest
以便审计。artifact 名：`bigworld-daily-<ver>-windows-x64`。

### 客户端 SDK：`scripts/package-daily-client-sdk.sh <out-dir>`

| 交付物 | 生成方式 | 版本 |
| --- | --- | --- |
| `.nupkg`（C# 协议核心） | `dotnet pack client_sdk/unity/src/BigWorld.ClientSdk/BigWorld.ClientSdk.csproj -c Release -p:Version=<SDK_VER>` | `SDK_VER = 0.0.1` |
| npm tarball（TypeScript 插件） | `pnpm pack`（`client_sdk/typescript`，需 `pnpm build` 先产出 `dist`） | `SDK_VER` |
| wheel（`bwgen`） | `pip wheel --no-deps client_sdk/generator` | `bwgen 0.0.1`（`pyproject.toml`） |
| `MANIFEST.txt` | 脚本自拼 | 含 version / build / commit / built / toolchain 头 + 逐文件 SHA256 与字节数 |
| zip | `bigworld-client-sdk-<TAG>.zip` | `TAG = daily<date>.<sha>` |
| `pkg-version.txt` | — | `TAG` |

artifact 名：`bigworld-client-sdk-<TAG>`。gate 在打包前依次跑三门：`dotnet test`、
`pnpm install --frozen-lockfile && pnpm build && pnpm test && pnpm typecheck`、
`python3 -m unittest discover -s client_sdk/generator/tests`——SDK 坏了不出包。

`.github/workflows/client-sdk.yml` 只是 push/PR 的纯 CI 门（`client_sdk/**` 触发），
不含打包腿。

## 红线与约定

- **无版本号发布**：源码与脚本里都注明「无 git tag、无 GitHub Release」。产物一律是
  workflow artifact，带保留窗口；`daily` 后缀即滚动构建，不是正式版本。
- **未过验收测试的产物不出包**：Linux lane 以 `bw-run-all-unit-tests` 为门，
  SDK lane 以三门测试为门。
- **两条服务端 lane 的版本号同源**（同一 `vcpkg.json`），产物名 `bigworld-daily-<ver>-<os>-<arch>`
  可并排比对。
- **RPM 的 `%{dist}` 被 `--define "dist %{nil}"` 置空**，所以 rpm 产物名里看不到
  `.el7` 之类的发行版后缀；dist 归属由 CI 环境而非包名承载。