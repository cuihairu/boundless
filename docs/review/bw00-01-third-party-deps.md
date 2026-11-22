# BW-00-01-04 第三方依赖与资源来源清册

> 证据面（BW-00-01-04 验收：每项有许可证、NOTICE 及复用决策）。

数据来源（参考 `cuihairu/BigWorld`）：

- `build/make/third_party_*.mak`（`vcpkg` / `curl` / `json` / `openssl` / `mongodb` /
  `mysql` / `sdl` / `python` / `python_modules` / `python_symbols`）
- `programming/bigworld/vcpkg.json`（其注释指向 `docs/vcpkg-migration.md`）
- 根 `THIRD-PARTY-NOTICES`（16 段，2292 行）
- `programming/bigworld/third_party/` 目录清单

## 一、NOTICE 段与许可证对照

`THIRD-PARTY-NOTICES` 用 `| <名称> |` 定界，共 16 段（行号为起始行）：

| NOTICE 段 | 行 | 许可证（该段文首声明） | 对应 `third_party/` |
| --- | --- | --- | --- |
| CppUnitLite2 | 10 | MIT，另加「禁用于军事项目/军事资助项目」限制 | `CppUnitLite2` |
| Curl | 25 | MIT 风格「COPYRIGHT AND PERMISSION NOTICE」 | `curl`（已退役） |
| JsonCpp | 51 | Public Domain；不承认公版的地域回落 MIT | vcpkg `jsoncpp` |
| MongoDB C++ Driver | 111 | Apache-2.0 | `mongodb/mongo_cxx_driver` |
| NedAlloc | 319 | Boost Software License 1.0 | `nedalloc` |
| NvMeshrender | 347 | NVIDIA 专有许可（2003，AS-IS） | `nvmeshmender` |
| Nvtt | 362 | MIT（NVIDIA Texture Tools 2.0） | `nvtt` |
| Openautomate SDK | 391 | NVIDIA OpenAutomate SDK 许可（专有） | `openautomate` |
| OpenSSL | 695 | 双许可：OpenSSL License + 原 SSLeay（均 BSD 风格） | `openssl-vs201x` / `openssl_android`（vcpkg 供源码） |
| pnglib | 825 | libpng 许可 | `png`（vcpkg 供） |
| Python | 941 | Python 软件基金会许可 | `python` |
| RE2 | 2175 | BSD-3-Clause | `re2` |
| Recast Navigation | 2208 | zlib 风格许可 | `recastnavigation` |
| SQLite3 | 2231 | 公共领域（作者已签署公版宣誓书） | `sqlite`（vcpkg 供） |
| WTL | 2243 | Microsoft Public License (MS-PL) | `WTL` |
| Zlib | 2270 | zlib 风格许可 | `zip`（vcpkg 供） |

## 二、参考构建的接入方式

### A. vcpkg manifest（`vcpkg.json`，7 个 port）

`builtin-baseline` 钉死 registry commit `10541e317a660f4165ba4ac2851ab54a8d4577b1`；
triplet `x64-linux-bw`（`build/vcpkg-triplets/`，静态、release-only）；安装到
`$(BW_THIRD_PARTY_BUILD_DIR)/vcpkg_installed`，stamp 目标 `vcpkg_install` 以
`vcpkg.json` + triplet 文件为输入。

port：`openssl`、`curl`(features `openssl`)、`jsoncpp`、`zlib`、`libpng`、`sqlite3`、`bzip2`。

`third_party_vcpkg.mak` 把 vcpkg 静态库改名为 BigWorld 链接行历来使用的名字
（`BW_VCPKG_PACKAGES`，共 7 项，**不含 bzip2**）：

| staging 名（进 `$(BW_LIBDIR)`） | vcpkg 源产物 |
| --- | --- |
| `libbwssl.a` | `libssl.a` |
| `libbwcrypto.a` | `libcrypto.a` |
| `libbwcurl.a` | `libcurl.a` |
| `libjsoncpp.a` | `libjsoncpp.a` |
| `libzip.a` | `libz.a` |
| `libpng.a` | `libpng16.a` |
| `libsqlite3.a` | `libsqlite3.a` |

改名是为让组件 `Makefile.rules` 的 `thirdPartyDependsOn` 链与 vcpkg 迁移前保持
不变；`bw` 前缀另有一层意图——`third_party_curl.mak` 明说「so we never accidentally
link a system libcurl」。`bzip2` 只在 manifest 里，因为 CPython 的 `_bz2` 扩展在
`third_party_python.mak` 的 `pythonSharedMods` 列表里，而本机无系统 bzip2 开发文件；
它不参与上表 staging。

### B. 系统库（探测 + sdk-config）

- **MySQL**（`third_party_mysql.mak`）：`mysql_config` 先找 `/usr/lib64/mysql/mysql_config`
  再回落 `PATH`（3.13 迁移修的坑：硬编码 el7 路径在 Debian/Ubuntu 不存在，导致每个
  `useMySQL` 二进制静默丢 `-lmysqlclient` 而链接失败）。`SYSTEM_HAS_MYSQL` 由
  `test_mysql.sh` 探测；Debug 配置下补 `-Wp,-U_FORTIFY_SOURCE` 抵消
  `mysql_config --cflags` 强加的 `_FORTIFY_SOURCE=2`。
- **SDL**（`third_party_sdl.mak`）：`/usr/bin/sdl-config` 取 `--cflags`/`--libs`，
  `SYSTEM_HAS_SDL` 由 `test_sdl.sh` 探测。
- **OpenSSL / cURL / jsoncpp**：`.mak` 文件保留但只指回 vcpkg 安装根
  （`CURL_BUILD_DIR` / `JSON_DIR` / `OPENSSL_BUILD_DIR := $(VCPKG_INSTALLED)`）；
  vendored 构建已退役——cURL 的 7.x configure 构建、jsoncpp 0.7.0 源码、OpenSSL 1.0.0d
  源码构建（无法在 gcc≥14 下编译，C23 `bool` 关键字；且低于 CPython 3.13 对
  `_ssl`/`_hashlib` 的 1.1.1 下限）。

### C. vendored（`third_party/`）

- **CPython**：`third_party/python` 是 heavily patched 的 BigWorld 构建（BWHooks 内存
  跟踪，见 `bw_changes.txt`），`BW_PYTHON_VERSION := 3.13`，目标 `bwpython3.13`，
  产出 `libpython3.13.a`。`third_party_python.mak` 注释钉死**不走 vcpkg**：
  「embedded CPython tree … is deliberately NOT taken from vcpkg」。configure 的
  `--with-openssl` 指回 vcpkg 的 OpenSSL；`_ssl`/`_hashlib` 与 `modulesWhichNeedSymbols`
  需在解释器符号表里解析同一批静态 OpenSSL 归档。`pythonSharedMods` 列表含
  `_hashlib _ssl _bz2 _sqlite3 zlib …` 等约 60 个扩展。
- **MongoDB**：`third_party/mongodb/{mongo_cxx_driver,boost}` 是**预编译**驱动与其依赖的
  boost（`boost_1_54_0.tar.gz`，`boost/build/{lib,include}`）。`-lmongoclient` 先于
  boost 库、boost 头文件先于驱动头文件——顺序写死在 `.mak` 里。`vcpkg.json` 注释说明
  为何不迁：vendored 驱动早于 vcpkg `mongo-cxx-driver` 3.x 的 API。
- **CppUnitLite2**：唯一由 Make 在树内构建的 third_party 组件
  （`third_party/CppUnitLite2/src`，见 BW-00-01-01）。
- **re2 / nedalloc**：进入 CMake server 目标的 `BW_LIBRARY_PROJECTS`
  （`re2 → third_party/re2`、`nedalloc → third_party/nedalloc`，见 BW-00-01-02）。
- **zip / png**：Make 侧已是 vcpkg 迁移后的残留空壳（`in_build = 0`）；CMake 侧仅作
  `NOT ZLIB_FOUND` / `NOT PNG_FOUND` 的回落项。
- **仅客户端 / Windows 侧**（Linux 服务面不触达）：`nvtt`、`nvmeshmender`、
  `openautomate`、`recastnavigation`、`WTL`、`putty`（`pageant.exe`、`plink.exe`、
  `puttygen.exe`，`build/cmake/BWVerifyServer.cmake` 用 `plink.exe` 做 SSH）、
  `openssl-vs2010/2012/2013/2015`（预编译 VS 库）、`openssl_android`、
  `cmake-win32-x86`（CMake 安装布局 `bin/doc/man/share`，Windows 侧工具链用）。

### D. 发行面 Python 模块（`third_party/python_modules/`）

`third_party_python_modules.mak` 消费的 tarball：`twisted-24.7.0`、`sqlalchemy-1.4.54`、
`msgpack-1.1.0`、`pika-1.3.2`、`setuptools-75.3.0`、`attrs-24.2.0`、`automat-24.8.1`、
`constantly-23.10.4`、`hyperlink-21.0.0`、`idna-3.10`、`incremental-24.7.0`、
`typing_extensions-4.12.2`、`Pympler-1.0`、`Imaging-1.1.7`（PIL）、`jpegsrc.v6b`、
`oursql-0.9.2`（含 `oursql-0.9.2-bwchanges.diff`）、另有 `AsyncSQLAlchemy.py`。
Make 侧对应 `bw-miscellaneous` 的 `python_*` 目标，装到
`bin/server/el7/server/commands/_helpers`（见 BW-00-01-01 安装布局）。

## 三、复用决策

| 依赖 | 参考接入 | boundless5 处置 |
| --- | --- | --- |
| CPython 3.13 | vendored，`libpython3.13.a` | **复用**：`crates/python-host` 的 `build.rs` 用 `BOUNDLESS_PYTHON` / PATH 上的解释器取 `sysconfig` 得到库目录与 loader 版本（3.13 取 `libpython3.13`），与参考的 `Python3_ROOT_DIR` 解析同法——解释器不重写，只重写宿主（BW-03） |
| MD5 摘要 | `MD5::Digest::quote()` | **替代**：workspace 外部依赖里已有 `md-5 = "0.10"`（大写 hex 对齐参考行为） |
| OpenSSL / cURL / jsoncpp / zlib / libpng / SQLite3 | vcpkg，staging 改名 | **替代**：尚未引入；各自 crate lane 需要时再按 Rust 等价物引入。当前 workspace 外部依赖仅 `libc` 与 `md-5` |
| MySQL client | 系统 `mysql_config` | **替代**：同上，未引入 |
| MongoDB C++ driver + boost 1.54 | vendored 预编译 | **替代**：未引入（Rust 无 boost 对应物） |
| CppUnitLite2 | 树内构建，单测框架 | **替代**：Rust 自带测试框架，无需对应物 |
| re2 / nedalloc | vendored，CMake server 目标 | **替代**：`regex` / Rust 分配器，按需引入 |
| bzip2 | vcpkg（仅供 `_bz2`） | 跟随复用 CPython，无需单独决策 |
| `python_modules` 一批（twisted 等 16 项） | tarball，装进 `_helpers` | 待 BW-03-03/04（script 模块与 role API）落地时逐条评估 |
| nvtt / nvmeshmender / openautomate / recast / WTL / putty / openssl-vs* / cmake-win32-x86 | 客户端 / Windows 侧 | 未涉及：与 Linux 服务面与当前 Rust lane 无关 |

## 四、NOTICE 缺口（需补齐或需替代）

`THIRD-PARTY-NOTICES` 未覆盖以下实际被消费的第三方件，属本项要标注的缺口：

1. **bzip2** — 在 `vcpkg.json` 依赖里，NOTICE 无段（zlib 风格许可，Zlib 段不可代指）。
2. **libmysqlclient / SDL** — 系统库，NOTICE 无段；`_helpers` 与 `bw` 链接行都触达。
3. **Boost 1.54.0** — vendored 于 `third_party/mongodb/boost`，NOTICE 只有
   MongoDB C++ Driver 的 Apache-2.0 段，boost 的 BSL-1.0 无段。
4. **`python_modules` 16 项**（twisted / sqlalchemy / msgpack / pika / setuptools /
   attrs / automat / constantly / hyperlink / idna / incremental / typing_extensions /
   Pympler / PIL / libjpeg / oursql）— NOTICE 无段，实际随包发行。
5. **putty**（`plink.exe` 等预编译产物）— NOTICE 无段。
6. **Python 段头陈旧** — 段首写「This is Python version 2.7.3」，而树内是 3.13
   （`third_party/python/LICENSE` 本身是版本无关的 PSF 许可正文，故陈旧只出现在
   NOTICE 文本里）。

参考侧的处置空间：`THIRD-PARTY-NOTICES` 首段声明「the attached notices are provided
for information only」，因此这些缺口是披露完整性问题，不是构建阻断项。
boundless5 侧只需在自己发行时把上述 1–5 各加一段，并同步第 6 项的版本声明。