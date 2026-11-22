# BW-00-01-04 登记第三方依赖与资源来源 — 清册

- **任务编号**: BW-00-01-04（lane BW-00 构建目标清册，todo.md 行 90）
- **原版构建入口/源码**: `programming/bigworld/third_party/`（22 个子目录）；`game/res/bigworld/scripts/`（服务端脚本与内嵌 Python 标准库）；repo 根 `THIRD-PARTY-NOTICES`（2292 行，16 个第三方许可通知节）。
- **Boundless 模块**: 本条目给出第三方依赖/资源的**复用决策**（对照 `crates/` 现状与 `Cargo.toml` 依赖），供 BW-00-05 覆盖台账与 BW-16 依赖审计引用。
- **平台**: 第三方依赖横跨 Linux 服务端与 Windows 客户端/编辑器。
- **输入**: `third_party/*` 许可证/版本标记文件；`THIRD-PARTY-NOTICES`；`game/res`；boundless `crates/`+`Cargo.toml`。

## 结果：第三方依赖清单（22 个子目录）

`THIRD-PARTY-NOTICES` 是**主 NOTICE 文件**（repo 根），含 16 个通知节：CppUnitLite2、Curl、JsonCpp、MongoDB C++ Driver、NedAlloc、NvMeshrender、Nvtt、Openautomate SDK、OpenSSL、pnglib、Python、RE2、Recast Navigation、SQLite3、WTL、Zlib。其余目录为二进制或由父节覆盖（详见各表）。

### 库/框架（有独立许可证，NOTICE 有节）

| 目录 | 组件 | 许可证 | NOTICE 节 | 版本 | 复用决策 |
|---|---|---|---|---|---|
| `curl` | libcurl | curl 许可（Daniel Stenberg 1996-2013，MIT 类） | Curl | 参考树自带 | **替代**：Rust 侧用 `ureq`/`reqwest`（HTTP），server-runtime 已含 urlrequest 概念 |
| `png` | libpng | libpng 许可 | pnglib | 参考树自带 | **替代**：Rust `image`/`png` crate |
| `python` | CPython 源码 | PSF（Python Software Foundation License） | Python | **3.13**（`BW_PYTHON_VERSION := 3.13`，third_party_python.mak:7；README.rst 亦 3.13） | **复用**：boundless `crates/python-host`（PyO3 系）；内嵌 Python 标准库资源（game/res）随 PSF 复用 |
| `re2` | RE2 正则库 | BSD-3 | RE2 | 参考树自带 | **已决**：Rust `regex` crate（`Cargo.toml` 注释明言 "reference uses RE2"；asset 规则/目录依赖模式用 Rust regex） |
| `sqlite` | SQLite 库 | Public domain | SQLite3 | amalgamation | **替代**：Rust `rusqlite`/`sqlx`；boundless `crates/db` 已有 SQLite 迁移面 |
| `zip` | zlib | zlib 许可 | Zlib | **1.2.8**（ChangeLog 头 "Changes in 1.2.8 (28 Apr 2013)"） | **替代**：Rust `flate2` |
| `nedalloc` | nedmalloc 分配器 | Boost Software License 1.0 | NedAlloc | 参考树自带 | **替代**：Rust 默认分配器（无需 C 分配器） |
| `recastnavigation` | Recast/Detour 寻路 | zlib 许可（Mikko Mononen 2009） | Recast Navigation | 参考树自带 | **待定**：寻路在服务端范围，Rust 侧可 `recastdetour-rs` 或自实现；纳入 BW-15 平台矩阵时拍板 |
| `mongodb` | MongoDB C++ Driver（boost+mongo_cxx_driver） | Apache 2.0 | MongoDB C++ Driver | 参考树自带（仅 message_logger/_bwlog 用） | **替代**：Rust `mongodb` crate；仅消息日志面用到，可降级为不依赖 |
| `nvtt` | NVIDIA Texture Tools | MIT（NVIDIA 2007） | Nvtt | **2.0.8**（VERSION 文件） | **不用**：渲染/贴图压缩，非服务端范围 |
| `nvmeshmender` | NVIDIA Mesh Mender | NVIDIA 许可（MIT 类） | NvMeshrender | 参考树自带 | **不用**：网格工具，非服务端范围 |
| `openautomate` | OpenAutomate SDK | **NVIDIA 专有**（Software License Agreement） | Openautomate SDK | 参考树自带 | **不用**：性能测试 SDK，专有许可，服务端不用 |
| `CppUnitLite2` | 单测框架 | MIT + **军事用途限制**（gamesfromwithin.com） | CppUnitLite2 | **1.1** | **替代**：Rust 内建测试框架（无需 C++ 单测库） |
| `WTL` | Windows Template Library | Microsoft 共享源（Shared Source，ReadMe.html "version 10.0.10320 Release (2020-11-15)"） | WTL | **10.0.10320** | **不用**：Windows UI 库，服务端不用 |
| `JsonCpp` | JSON 库（在 NOTICE 有节，无独立 third_party 目录——随源码树/其他交付） | Public Domain / MIT 双许可 | JsonCpp | 参考树自带 | **替代**：Rust `serde_json` |

### 二进制/工具（无独立库许可，不设 NOTICE 节）

| 目录 | 内容 | 说明 | 复用决策 |
|---|---|---|---|
| `openssl-vs2010/2012/2013/2015` | 预编译 OpenSSL 库（**1.0.1p**，Cygwin/MSVC） | Windows 侧预编译产物 | **不用**：Windows 构建产物；服务端用系统/自建 OpenSSL 或 Rust `openssl`/`rustls` |
| `openssl_android` | Android 构建 OpenSSL（**0.9.8h**，含 fips 变体） | Android 平台构建 | **不用**：Android 不在 Boundless 范围 |
| `putty` | `pageant.exe`/`plink.exe`/`puttygen.exe` 预编译 | Windows 远程/密钥工具 | **不用**：Windows 工具产物 |
| `cmake-win32-x86` | CMake 二进制发行（bin/doc/man/share） | Windows 构建工具链 | **不用**：boundless 用自身 CI 工具链 |
| `python_modules` | Python 模块（README.txt） | Python 依赖模块 | **复用**：随 python-host/PyO3 面评估（与 game/res 的 Python 标准库一并） |

## 资源来源

- **资源树**: `game/res/bigworld/scripts/`（4557 文件）——服务端脚本：`common/`（含 `Lib/`、`site-packages/`、`bw_site.py`）+ `server_common/`（含 `site-packages/`、`lib-dynload-el7/`）。这是内嵌 **Python 3.13 标准库 + BigWorld 服务端脚本**。
- **许可证**: 资源自身为 BigWorld 自有（BigWorld 许可证）；内嵌 Python 标准库为 **PSF**（`third_party/python/LICENSE` 对应）。
- **NOTICE**: 无独立资源 NOTICE——资源许可经 `THIRD-PARTY-NOTICES` 的 **Python** 节覆盖（内嵌标准库），BigWorld 自有部分随主 LICENSE。

## 复用决策总表

- **复用（vendored/原生）**：Python（PSF，python-host 系）、RE2→Rust `regex`（已决，`Cargo.toml`）。
- **替代为 Rust crate**：curl→ureq/reqwest、png→image、sqlite→rusqlite/sqlx、zlib→flate2、mongodb→mongodb crate、JsonCpp→serde_json、MD5→`md-5`（已决，entitydef 摘要）、CppUnitLite2→Rust 测试框架、nedalloc→Rust 分配器。
- **不用（非服务端范围/专有/Windows 产物）**：nvtt、nvmeshmender、openautomate（NVIDIA 专有）、WTL、openssl-vs*/openssl_android、putty、cmake-win32-x86。
- **待定**：recastnavigation（寻路，随 BW-15 平台矩阵拍板）。

## 状态

清册完整：22 个 `third_party` 子目录（16 库/框架 + 6 二进制/工具）、16 个 NOTICE 通知节（主文件 `THIRD-PARTY-NOTICES`）、资源来源（`game/res/bigworld/scripts` = Python 3.13 标准库 + BigWorld 服务端脚本，4557 文件），每项均有许可证、NOTICE 归属、版本与复用决策（复用/替代/不用/待定）。
