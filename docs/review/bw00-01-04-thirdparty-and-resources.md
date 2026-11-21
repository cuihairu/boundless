# BW-00-01-04 第三方依赖与资源来源清册

基线 `4657a17cf10d55e2fbebf258f96da96a3dbe0c22`，路径相对 `../BigWorld/`。对应 todo.md BW-00-01-04：**登记第三方依赖与资源来源，每项有许可证、NOTICE 及复用决策**。

## 采集方法

1. `THIRD-PARTY-NOTICES`（2292 行）逐节读，16 个分节标题提取。
2. `programming/bigworld/third_party/` 22 个目录逐一 `find -iname "*license*|COPYING*|COPYRIGHT*"` 定位许可证文件，缺失者登记。
3. **复用决策权威源**：`docs/vcpkg-migration.md` §1 决策表（OpenSSL→vcpkg 3.6.4、curl→8.22、jsoncpp→1.9.x、zlib→1.3.x、libpng→1.6.58、sqlite3→3.53.4、CPython/.mongodb 留 vendored、png/zip 树保留作 Windows fallback）+ `vcpkg.json`（builtin-baseline 钉版）。
4. 资源面：`game/res`（122 MB）布局实查 + `third_party/python_modules/`（18 个 sdist + README 流程记录）。

## 表 A：第三方库清单（许可证 / NOTICE / 复用决策）

| 库（目录） | 许可证 | NOTICE 节 | 复用决策 |
|---|---|---|---|
| OpenSSL（`openssl-vs2010/12/13/15`、`openssl_android`） | Apache-2.0（3.x；旧 1.0.0d 为 OpenSSL/SSLeay 双许可） | OpenSSL | **vcpkg 3.6.4 静态**（gcc15 修复 + Python `_ssl`/`_hashlib` 恢复，R1）；Linux 面原 `third_party/openssl` 已删；vs20xx/android 目录为 Windows/Android 预编译残留（仅发行物形态） |
| curl（`curl`） | curl license（MIT 式许可） | Curl | **vcpkg 8.22**（openssl feature），链接名保留 `bwcurl` |
| JsonCpp（已删） | MIT / 公有领域双许可 | JsonCpp | **vcpkg 1.9.x**；消费面仅 `Reader::parse`/`Value`/`FastWriter`，目录已删 |
| zlib（`zip`） | zlib license | Zlib | **vcpkg 1.3.x**（链接名保留 `zip`）；vendored 树保留作 Windows CMake fallback（BW-00-01-02 的 FIND_PACKAGE 规则）；`MY_ZCALLOC` 钩子随迁移失去，zlib 分配不再计入 BW 内存统计 |
| libpng（`png`） | libpng license | pnglib | **vcpkg 1.6.58**；`lib/moo/png.cpp` 改走公开 API；vendored 树同上保留 fallback |
| SQLite3（`sqlite`） | 公有领域（作者声明放弃版权） | SQLite3 | **vcpkg 3.53.4**；`lib/sqlite` 仅留封装层，直连消费者 `useSQLite := 1` |
| CPython（`python`） | PSF LICENSE（`python/LICENSE`）+ `PC/crtlicense.txt` | Python | **留 vendored 3.13.16**——vcpkg python3 端口不带 BWHooks 内存跟踪补丁（`bw_changes.txt`）；`_ssl`/`_hashlib` 经 vcpkg OpenSSL 恢复 |
| MongoDB C++ driver（`mongodb`） | Apache-2.0（driver）+ 捆绑 boost（Boost 许可） | MongoDB C++ Driver | **留 vendored legacy**——vcpkg mongo-cxx-driver 为 3.x API 不兼容；仅 message_logger 使用 |
| CppUnitLite2 | MIT **+ 军事用途限制**（notices 明文 added restriction） | CppUnitLite2 | 留 vendored（引擎自带测试框架）；树内无独立 license 文件，以 notices 为准 |
| WTL（`WTL`） | MS-PL（`MS-PL.txt`） | WTL | 留 vendored（Windows-only UI） |
| putty（`putty`） | putty license（MIT 式）——**树内仅 3 个 exe（plink/pageant/puttygen），无许可文本** | （无节） | 留 vendored 仅二进制；许可证文本缺失登记待补 |
| cmake-win32-x86 | CMake BSD-3（发行版，`bin/doc/man/share`） | （无节） | 留 vendored 工具链（bigworld_cmake.py 默认 cmake.exe） |
| nedalloc | Boost Software License 1.0（`License.txt`） | NedAlloc | 留 vendored（Windows 分配器路径） |
| nvmeshmender | NVIDIA 源码，AS-IS 条款（notices NvMeshrender 节）——树内无独立 license 文件 | NvMeshrender | 留 vendored（tools 面） |
| nvtt | MIT（`NVIDIA_Texture_Tools_LICENSE.txt`） | Nvtt | 留 vendored（纹理工具，tools 面） |
| openautomate | **NVIDIA Software License Agreement（专有条款）** | Openautomate SDK | 留 vendored（client-only，`lib/open_automate`）；再分发条款受限，不在 server 交付面 |
| recastnavigation | zlib 式（memon，`License.txt`） | Recast Navigation | 留 vendored（tools 导航面：Recast+Detour） |
| re2 | BSD-3（`re2/LICENSE`） | RE2 | 留 vendored（resmgr 及各目标链接） |
| bzip2（vcpkg） | bzip2 license | （**notices 无节**） | vcpkg 新增项（主机无系统 bzip2 dev，供 CPython `_bz2`）；NOTICE 缺口登记 |

## 表 B：资源来源（`game/res`，122 MB）

- 布局：`res/bigworld/scripts/{common,server_common}`——BigWorld 自有服务器/公共脚本，BigWorld License（随仓 LICENSE）。
- `scripts/common/site-packages/`：vendored Python 包的**已安装树**（attrs/automat/constantly/hyperlink/idna/incremental/twisted 族/Pympler/setuptools/msgpack/oursql/sqlalchemy/zope.interface 等），来源为 `third_party/python_modules/` 的 sdist（twisted-24.7.0、sqlalchemy-1.4.54、setuptools-75.3.0、Imaging-1.1.7 等 18 个 tarball + `AsyncSQLAlchemy.py` + `oursql-0.9.2-bwchanges.diff` 本地补丁）；各包许可在各 `*.dist-info/LICENSE` 内随包携带。
- `python_modules/README.txt` 记录的是**旧版本**（Twisted 11.0.0/Zope 2.11.4/oursql 0.9.3.1/SQLAlchemy 0.6.6/Pympler 0.3.0）安装流程；现存 tarball 为 Python 3.13 迁移后的现代化版本——README 与实物版本不对应，登记待更新。
- 无美术/音频资源（纯脚本树）；`logo.svg` 为 BigWorld 自有设计素材（禁区，永不代餐）。

## NOTICE 完整性登记

- `THIRD-PARTY-NOTICES` 16 节：CppUnitLite2、Curl、JsonCpp、MongoDB C++ Driver、NedAlloc、NvMeshrender、Nvtt、Openautomate SDK、OpenSSL、pnglib、Python、RE2、Recast Navigation、SQLite3、WTL、Zlib。
- 与迁移后实际链接集的差异： 6 项已切 vcpkg 的库（openssl/curl/jsoncpp/zlib/libpng/sqlite3）notices 文本仍对应旧 vendored 版本； bzip2（vcpkg 新增）无节； putty/cmake-win32-x86 无节。若对外分发二进制，notices 需按 vcpkg 端口版本更新——**更新动作未做，登记待办**。

## 复用决策汇总（boundless 视角）

- Linux server 交付面实际第三方闭包 = vcpkg 7 项（openssl、curl、jsoncpp、zlib、libpng、sqlite3、bzip2）+ vendored 4 项（CPython 3.13 BWHooks、mongodb legacy driver、re2、nedalloc）+ 测试框架 CppUnitLite2 + 运行时 Python 包（res site-packages）。
- Windows 面另挂 fallback（png/zip vendored 树）与 client/tools 专属（WTL、openautomate、nvtt、nvmeshmender、recast、putty、cmake-win32-x86）。
- 「需替代实现」项：无强制项——vcpkg 覆盖了全部可迁移库；CPython/mongodb 因补丁/API 断层留源码树，boundless 对应面已由 pyhost（BW-03）/message_logger 侧自有实现承接（不在本清册展开）。

## 待验登记

1. putty 许可文本与 notices 缺节项属文本缺口，未补（登记待办，非本项验收阻塞）。
2. openautomate/nvmeshmender 的 NVIDIA 条款再分发限制仅作登记；两者不在 Linux server 交付面，法务确认不在本席位范围。
3. `python_modules/README.txt` 版本漂移登记待更新；site-packages 各包许可随包可查，未逐包摘录（`dist-info/LICENSE` 在树可证）。

## 结论

验收达成：22 个 third_party 目录 + vcpkg 7 项 + 资源面（res 脚本树 + site-packages + python_modules）逐项登记许可证、NOTICE 归属（16 节对照）与复用决策（vcpkg 切换/vendored 保留/fallback 三态，以 vcpkg-migration 决策表为权威）；NOTICE 缺口（bzip2 无节、6 项版本漂移、putty/cmake 无节）与 README 版本漂移如实登记待办。
