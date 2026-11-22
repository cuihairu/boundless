# BW-00-02-01 原版运行环境采集

> 证据面（BW-00-02-01 验收：记录提交、二进制哈希、构建参数、Python 和 DB 版本）。

由 `tools/runtime_env_scan.py` 扫描参考仓 `cuihairu/BigWorld` 生成；源数据 `reference/bw00_02_01_runtime_env.json`。

## 源与构建参数

| 项 | 值 |
| --- | --- |
| 参考仓提交 | `916dc72a6c15`（916dc72a，2026-11-23 00:14:46 +0800，“test: add cluster deep chain smoke”） |
| REVISION_NUMBER | `-1`（`build/el7/build/build_revision.generated.hpp`，构建时未注入 git 版本号） |
| 平台 / 配置 | `el7` / `hybrid`（`BW_CONFIG_DEFAULT := hybrid`；hybrid 不带 `_hybrid` 目录后缀，`build/make/Makefile:86`） |
| C++ 标准 | `-std=c++23`（`build/make/platform_el7.mak`） |
| 编译器 | `Ubuntu 15.2.0-16ubuntu1 15.2.0`（stamp 取自 `server/cellapp`） |

## Python 与数据库版本

| 组件 | 版本 | 来源 |
| --- | --- | --- |
| 嵌入解释器（静态） | **CPython 3.13.16** | `third_party/python` vendored，静态链入每个角色二进制（`libpython3.13.a`） |
| 系统 `python3` | Python 3.14.9 | 开发机 PATH，不参与运行面 |
| OpenSSL（静态，vcpkg） | OpenSSL 3.6.4 25 Aug 2026 | `build/el7/third_party/vcpkg_installed/x64-linux-bw/include/openssl/opensslv.h` |
| 系统 OpenSSL | OpenSSL 3.5.5 27 Jan 2026 (Library: OpenSSL 3.5.5 27 Jan 2026) | `ldd` 仅 MySQL 命令类经 `libmysqlclient.so.24` 传递引入 |
| MySQL 客户端（动态） | `libmysqlclient.so.24`，`mysql_config` 8.4.13（包 `libmysqlclient-dev 8.4.13-0ubuntu0.26.04.2`；另装 `libmariadb3 1:11.8.6` 备选） | 系统 |
| SQLite3（静态，vcpkg） | 3.53.4 | `include/sqlite3.h` |
| MongoDB 驱动 | legacy 26compat **2.6.7** | `third_party/mongodb/mongo_cxx_driver/mongo-cxx-driver-legacy-0.0-26compat-*.tar.gz` |
| 系统 sqlite3 | 3.46.1 | CLI，未被二进制引用 |

### vcpkg port 集（`builtin-baseline 10541e317a660f4165ba4ac2851ab54a8d4577b1`）

| port | 版本 |
| --- | --- |
| `openssl` | OpenSSL 3.6.4 25 Aug 2026 |
| `curl` | 8.22.0 |
| `jsoncpp` | 1.9.6 |
| `zlib` | 1.3.2 |
| `libpng` | 1.6.58 |
| `sqlite3` | 3.53.4 |
| `bzip2` | 1.0.8 |

## 链接形态

对 46 个 ELF 逐一 `ldd`：

- **静态派**（角色二进制 / 工具 / 单测，共 41 个）：动态面只有 `libstdc++.so.6`、`libm.so.6`、`libgcc_s.so.1`、`libc.so.6`——vcpkg 静态库与 `libpython3.13.a` 全部链进可执行。例：`server/cellapp`。
- **MySQL 动态派**（5 个：`server/commands/clear_auto_load`、`consolidate_dbs`、`sync_db`、`server/commands/_helpers/snapshot_helper`、`server/dbapp-extensions/bwengine_mysql.so`）：系统 `libmysqlclient.so.24` 动态链接，并传递引入系统 `libssl.so.3` / `libcrypto.so.3` / `libz.so.1` / `libzstd.so.1` / `libresolv.so.2`。`transfer_db` 是 SQLite-only（Make 特性 `useSQLite=1`），不在此列、纯静态。

即发行脚本注释所称「third-party libraries are statically linked (vcpkg), only libc/libstdc++/mysql client stay dynamic」与本机实测一致（MySQL 传递引入的系统 ssl/crypto/z 也在动态面）。

## 二进制哈希

安装树 `game/bin/server/el7/` 下 59 个文件：46 个 ELF 与 13 个非 ELF。下表列出全部 ELF 及脚本 `remove_db`（47 行）；其余 12 个为单测断言转储（`assert.*.log`）与 `unit_tests/xmldataresource.xml` 数据文件，不入哈希表（见 JSON）。

| 路径 | sha256 | 大小 |
| --- | --- | --- |
| `examples/simple_python_client` | `7466433421fe4b0e4f4066430a3fb507a6c3aa33f8ebb0c7576d8a77b8d3894b` | 157,245,128 |
| `server/baseapp` | `dda3c0870262e9486a53645749fcdb2e312a3e1fc27e8bf3048032e9ac2366b8` | 194,526,064 |
| `server/baseappmgr` | `45ecb643dfa3928ce2034ce46427bf817ec82d3c7b1fc703584affc1a8a68eff` | 80,704,008 |
| `server/bots` | `2679087634ecafabbe9202df4a39357eb10d5b25c626bcc53a0bc3622e363769` | 195,885,392 |
| `server/cellapp` | `8440f399776780017f5406a0f3a41c2f8e8901506b19fb4b1bff81b6503a437d` | 227,588,416 |
| `server/cellapp-extensions/cellapp_example.so` | `a088179bc17486d9e6e9ab9f0270229e1713a5aabe82dca4e90b5d33a3338fa1` | 2,417,544 |
| `server/cellappmgr` | `b9a882d68ab65d7e629f299cbcab706d3cd4d408161088d9b1117607af83d847` | 84,351,320 |
| `server/commands/_helpers/snapshot_helper` | `b0403b32f3d252f952dd79a1f4d607185c7a58e2e3953a88555d9f02d0e6db9c` | 30,347,728 |
| `server/commands/clear_auto_load` | `5e918b821ba1c6a1ecd3053f37df69cbb8c312a7aa82a47f6dbc922a83cb2b1a` | 147,937,800 |
| `server/commands/consolidate_dbs` | `d0e7d28260a3485891d3c70f3ec081bd45571a6d02c2526371b7c08303ef8673` | 153,866,368 |
| `server/commands/remove_db` | `208c1dc7ea6995dc32bc8c1cc7cf1f5c3b8f44d1745fc5ecaaa9aaa50a98455b` | 891 |
| `server/commands/sync_db` | `ff3618744f96d916c4bf5b1d7395d2f275a88dd62be089229400b07a0ddfb802` | 149,884,136 |
| `server/commands/transfer_db` | `18d71a6519d1a3888a69457a9d97520ad70976615fe3159a426bb047a606f893` | 72,088,336 |
| `server/dbapp` | `118ab870032bb33b5e63acc96bbc13e92970629088ef7bf1a93c14b74d8a3798` | 163,472,232 |
| `server/dbapp-extensions/bwengine_mysql.so` | `90cf5d17536e21d007d83317d8411f6dc9bfb6ffd8e78fa7c580297275d64f50` | 17,949,104 |
| `server/dbapp-extensions/bwengine_xml.so` | `6f367467375d0cce88642f48461985bc6624ed234456295b7eec1c0a197f326a` | 2,433,528 |
| `server/dbappmgr` | `ea1a6ccd95f135ae81c16a59e4e93e659c6f9d4580fb152e1472c6a158681eeb` | 75,032,728 |
| `server/loginapp` | `fc2b53d78da4fd95c9a334098f2aac9ec8b7e8b42c693c7c7fd7e95118893764` | 88,630,448 |
| `server/reviver` | `1ec58a0f24da7bd32497bcbeaf474cc6bb1a83744bd9274f36b981e44637d2f6` | 74,656,096 |
| `server/serviceapp` | `dda3c0870262e9486a53645749fcdb2e312a3e1fc27e8bf3048032e9ac2366b8` | 194,526,064 |
| `tools/_bw_profile.so` | `f122d05f3aa596e9f043f58fa250d53aec6812b6d6307f5246739ac7a945c4ea` | 29,690,376 |
| `tools/_bwlog.so` | `bd9141a767f7e3c28dfcce7229e7264ec5d49443e1830ab6e68a2795b48b297b` | 46,312,792 |
| `tools/bwmachined2` | `3bdb7f2bda77d6c4fcf1e2eadd2868c54b84eed2d46a433fc1a5a7dcb036d022` | 40,444,416 |
| `tools/message_logger` | `6652426729d39edd1e4ade04a1a56cc67d0c449cf8dd8ff24bde9878d386fc93` | 74,978,344 |
| `tools/process_defs` | `857d42e7cb230206750fd8485fa82ac7040ce671c5e8fef742e4c03b48e6f426` | 129,644,472 |
| `tools/res_packer` | `19cfec7a828e28432dce0f351cd7fc9184038627148d0884f80af5ab503bc044` | 45,636,240 |
| `unit_tests/baseapp_test` | `ade58b690b40e3b2fca7b201d5b605a7fcb87ae9e750dfa1dd95b4d4aaf6643c` | 84,346,240 |
| `unit_tests/baseappmgr_test` | `efd525b2ec2c969671cb898ff757f2f5ba9d6072c1e3daa7b12f60609c580ee6` | 32,195,384 |
| `unit_tests/cellapp_test` | `5fa8de9cd2e61b17b238e3714d75bbd463008a76d0e9faeefb69a1e3ae877cc9` | 102,642,032 |
| `unit_tests/cellappmgr_test` | `9e1b1693b5842a84fc52b7586f9c37fe0b5a6f3f9c657fd8b99e12a961ff23e7` | 32,195,384 |
| `unit_tests/cstdmf_test` | `264d8ac7b2081e7bce7ea04e048a078ba034755bfaec072229c16f5e4f38b2de` | 72,121,976 |
| `unit_tests/db_test` | `6a2d92ba7015d243d092e2df3d461d415d11de4f7e60e11499e847a31593c977` | 69,853,432 |
| `unit_tests/dbapp_test` | `f6c0cb71aa74291ee488453dbdcf949ad71d3ac2594c3071b62fd16aa99dbc3e` | 32,195,328 |
| `unit_tests/dbappmgr_test` | `a63692f27c674585cca58fb8a4ea11ea8e7b0d23adfd670ff741980820318df4` | 32,195,368 |
| `unit_tests/entitydef_test` | `93718669946c43551fdcefb0d8476f49431b9b918ab5ec07c158bb3cc72c2870` | 189,790,040 |
| `unit_tests/loginapp_test` | `c03a1b0bf985e62e977c43bb48bb8b95279d813ac3d15417dbb09339e6482fc9` | 32,195,368 |
| `unit_tests/math_test` | `93d3386211e4ef85a0fd9faf74338922cf9f28adb7e2a3bf9bc1d809601f4c30` | 34,949,984 |
| `unit_tests/network_test` | `f7fb778a5cf65c835e3f93ffb1dd751f326fbe88d99b078f96770a74ce727870` | 107,019,168 |
| `unit_tests/physics2_test` | `32448a05519ce47892c2b14ba5767285e8d2c5398f6e560f7b9a452f57b2d05a` | 50,652,112 |
| `unit_tests/pyscript_test` | `70c351370a160c3fd445e44e7eeb6e4a35fc56eb91816ee44580c6ec0671163a` | 110,139,000 |
| `unit_tests/resmgr_test` | `87381867a2e8c83642ecb25d5e26e7d842311f1f3b7f8e286fa1c19ce31c6776` | 53,615,288 |
| `unit_tests/reviver_test` | `217d8eed5845269172427857faedc6f9837a768faa107644ad98deaf3cb033d2` | 32,195,368 |
| `unit_tests/scene_test` | `474a4a26608901e0bd84e64ad151f73a1b07c14ff625127a4396d8225b4e7da1` | 33,879,576 |
| `unit_tests/script_test` | `c75416fe6228caadd8ff8caa854a28a5539cd9788c618e6b5c63bffe262e065b` | 91,608,328 |
| `unit_tests/server_test` | `ac426590a7e4dce4295a7ce459cf5df9c9a506670b6bbaa3cb8bfdc47bad939f` | 34,893,688 |
| `unit_tests/terrain_test` | `1e5ed57ddef0a128c7bca83dc3df5e34104430144f6837cdd679c1a8a52a8abb` | 33,468,392 |
| `unit_tests/unit_test_lib_test` | `b407303e49cd7aad3b6a57d51088ff79d45c928b12325e5e6111b6ff199c0556` | 32,616,800 |

生成：`runtime_env_scan.py` + `runtime_env_doc.py`。
