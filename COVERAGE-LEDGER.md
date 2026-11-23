# 覆盖台账（BW-00-05）

原版（BigWorld 14.4.1）每个构建目标一行，索引到本仓任务号与夹具证据。
原版提交基线 `4657a17c`；二进制哈希见
`crates/test-support/cases/el7-binaries.sha256`，用例底本见
`crates/test-support/cases/original_inventory.txt`（910 用例）。

## 字段

| 列 | 含义 |
| --- | --- |
| 原版目标 | 原版构产出物名（Make goal / CMake target / 包） |
| 类别 | 库 / 服务端可执行 / 工具 / 扩展 / 单测 / 示例 / 发行物 |
| 启用条件 | 参编条件：宏、平台、条件开关（BW-00-01-01/-02 已登记） |
| 任务号 | 本仓承接它的任务族（多族时并列） |
| 夹具/证据 | 可复现证据：哈希清单、用例底本、路径 |
| 状态 | 已覆盖 / 进行中 / 已登记依赖 / 暂缓（范围外）/ 缺口 |

## 引擎库（Make `.DEFAULT_GOAL = lib<目录>`，产物 `build/el7/lib/`）

| 原版目标 | 类别 | 启用条件 | 任务号 | 夹具/证据 | 状态 |
| --- | --- | --- | --- | --- | --- |
| libcstdmf | 库 | make 主链常驻 | BW-01 | cstdmf_test 452 用例（`TESTING.md`）；单测 ELF 哈希在清单 | 已登记依赖 |
| libmath | 库 | 常驻；Windows 侧 D3DX typedef | BW-01 | math_test；`TESTING.md` 记录 test_blend_transform 为 Windows 专用未参编 | 已登记依赖 |
| libmoo | 库 | 常驻 | BW-01 | moo 目录无单测目录（unit_test 缺） | 已登记依赖 |
| libunit_test_lib | 库 | 常驻 | BW-18 | unit_test_lib_test | 已覆盖 |
| libtesting | 库 | 无 goal（仅头文件） | BW-18 | `lib/testing/Makefile` 无 `.DEFAULT_GOAL` | 已覆盖 |
| libbuild | 库 | 常驻 | BW-00 | `build/el7/lib/libbuild.a` 哈希在清单 | 已覆盖 |
| libnetwork | 库 | 常驻 | BW-02 | network_test（需 `net.core.rmem_max ≥ 16MB`） | 已登记依赖 |
| libconnection / libconnection_model | 库 | 常驻 | BW-02 | 随 network 模块单测 | 已登记依赖 |
| libdelegate_interface | 库 | 常驻 | BW-02 | 无单测目录 | 已登记依赖 |
| libchunk / libchunk_loading | 库 | 常驻 | BW-13 | 无单测目录；资源管线族 | 已登记依赖 |
| libresmgr | 库 | 常驻 | BW-13 | resmgr_test | 已登记依赖 |
| libentitydef / libentitydef_script | 库 | 常驻 | BW-04 | entitydef_test（嵌 Python） | 已登记依赖 |
| libscript / libpyscript | 库 | 常驻 | BW-03 | script_test、pyscript_test（嵌 Python） | 已登记依赖 |
| libsqlite | 库 | 常驻 | BW-09 | 无单测目录 | 已登记依赖 |
| libdb / libdb_storage | 库 | 常驻 | BW-09 | db_test | 已登记依赖 |
| libdb_storage_xml / libdb_storage_mysql | 库 | 常驻 | BW-09 | 对应扩展 .so 行 | 已登记依赖 |
| libspace / libspeedtree | 库 | 常驻 | BW-08 | 无单测目录 | 已登记依赖 |
| libterrain / libscene | 库 | 常驻 | BW-10 / BW-08 | terrain_test、scene_test | 已登记依赖 |
| libwaypoint | 库 | 常驻 | BW-10 | 无单测目录（原版服务器寻路自研） | 已登记依赖 |
| libmodel / libparticle | 库 | 常驻 | BW-11 | 无单测目录；客户端族 | 暂缓（范围外） |
| libphysics2 | 库 | 常驻 | BW-08 | physics2_test | 已登记依赖 |
| libserver | 库 | 常驻 | BW-05 / BW-06 | server_test | 已登记依赖 |
| 三方捆绑 9 项（CppUnitLite2、bwcrypto、bwcurl、bwpython3.13、bwssl、jsoncpp、png、sqlite3、zip） | 库 | 常驻，`build/make/third_party_*.mak` | BW-00 | 复用决策见 todo.md BW-00-01-04 | 已覆盖 |

## 服务端可执行与工具

| 原版目标 | 类别 | 启用条件 | 任务号 | 夹具/证据 | 状态 |
| --- | --- | --- | --- | --- | --- |
| baseapp（同 goal 产 serviceapp） | 服务端 | make 主链常驻 | BW-06 / BW-07 | baseapp_test、serviceapp_test？无；二进制哈希在清单 | 已登记依赖 |
| baseappmgr | 服务端 | 常驻 | BW-05 | baseappmgr_test | 已登记依赖 |
| cellapp | 服务端 | 常驻 | BW-08 | cellapp_test | 已登记依赖 |
| cellappmgr | 服务端 | 常驻 | BW-05 | cellappmgr_test | 已登记依赖 |
| dbapp / dbappmgr | 服务端 | 常驻 | BW-09 | dbapp_test、dbappmgr_test | 已登记依赖 |
| loginapp | 服务端 | 常驻 | BW-06 | loginapp_test | 已登记依赖 |
| reviver | 服务端 | 常驻 | BW-05 | reviver_test | 已登记依赖 |
| bots | 服务端 | 常驻 | BW-17 | 无单测目录 | 已登记依赖 |
| bwmachined2 | 工具 | 常驻 | BW-15 / BW-22 | 上游唯一有内容 RPM 的内核 | 已登记依赖 |
| message_logger（+ `_bwlog.so`） | 工具 | 常驻 | BW-25 / BW-12 | `_bwlog.so` 哈希在清单 | 已登记依赖 |
| process_defs | 工具 | 常驻 | BW-15 | 本机可跑基准（活基准条目） | 已覆盖 |
| res_packer | 工具 | 常驻 | BW-13 | `xml_packer.cpp` 走 `resources.xml` | 已登记依赖 |
| commands：clear_auto_load / consolidate_dbs / remove_db / sync_db / transfer_db | 工具 | 条件 goal，6 个已随产出 | BW-09 | 落 `server/commands/`，哈希在清单 | 已登记依赖 |
| _helpers/snapshot_helper | 工具 | 条件 goal | BW-09 | 30MB ELF，哈希在清单 | 已登记依赖 |
| _bw_profile | 工具 | 条件 goal，未随本构建产出 | BW-23 | 缺口：本 el7 树无产物 | 缺口 |

## 扩展与单测

| 原版目标 | 类别 | 启用条件 | 任务号 | 夹具/证据 | 状态 |
| --- | --- | --- | --- | --- | --- |
| bwengine_xml.so / bwengine_mysql.so | 扩展 | 常驻 | BW-09 | 扩展 .so 哈希在清单；零动态 mysql 依赖 | 已登记依赖 |
| cellapp_example.so | 扩展 | examples 开关 | BW-14 | 落 `server/cellapp-extensions/` | 已登记依赖 |
| 21 个 `*_test` | 单测 | `bw-run-all-unit-tests` 门禁 | BW-18 | 逐模块参编列表与执行命令见 todo.md BW-00-02-02；1540 用例 0 失败，其中 6 模块空注册表 | 已覆盖 |

## 客户端、导出器与导航目标（CMake `BW_CMAKE_TARGET`）

11 个 target 枚举见 todo.md BW-00-01-02。项目→路径索引（`build/cmake/BWConfiguration_*.cmake`，
47 条，去重）；宏：client 加 `-DBW_CLIENT`，exporters 加 `-DBW_EXPORTER`/`-D_SECURE_SCL=1`，
导出器族按 `FMOD`/`SPEEDTREE`/`UMBRA` 支持位门控。

| 原版目标 | 类别 | 启用条件 | 任务号 | 夹具/证据 | 状态 |
| --- | --- | --- | --- | --- | --- |
| client（`client/`） | 客户端 | `-DBW_CLIENT` | BW-11 | 源在树；make 链不产出 | 暂缓（范围外） |
| client_blob | 客户端 | 预编译二进制 blob，源缺失 | BW-11 | `find client_blobs` 无命中 | 缺口 |
| exporters：max2009–2013、maya2009–2014、visualexporter×5 | 导出器 | `-DBW_EXPORTER` + 插件支持位 | BW-13 | `tools/{animationexporter,mayavisualexporter,visualexporter}` | 暂缓（范围外） |
| exporters_blob | 导出器 | 预编译 blob，源缺失 | BW-13 | 无命中 | 缺口 |
| assetprocessor、asset_pipeline/*、compiler/test_compiler/test_converter、compiler+dependency 的 unit_test | 离线工具 | 常驻 | BW-13 | `tools/asset_pipeline/**` 含 converter×8（bsp/effect/hierarchical_config/primitive/space/texformat/texture/visual） | 暂缓（范围外） |
| navgen | 离线工具 | 常驻 | BW-13 | `tools/navgen` | 暂缓（范围外） |
| modeleditor、modeleditor_core、particle_editor、worldeditor、editor_shared、jit_compiler、batch_compiler、discovery、res_packer、process_defs、tools/common | 编辑器/工具 | 常驻 | BW-13 | `tools/` 下对应目录 | 暂缓（范围外） |
| tools_blob | 工具 | 预编译 blob，源缺失 | BW-13 | 无命中 | 缺口 |
| extras | 杂项 | 条件（支持位门控） | BW-11 | `BWConfiguration_extras.cmake` | 缺口 |
| bwentity（`lib/bwentity`） | 库 | 常驻 | BW-04 | lib 无对应 `.a`（无 `libbwentity.a`），仅 CMake target | 缺口 |

## 公共代码、示例与 FantasyDemo

| 原版目标 | 类别 | 启用条件 | 任务号 | 夹具/证据 | 状态 |
| --- | --- | --- | --- | --- | --- |
| `common/` Python 绑定（py_chunk/py_network/py_physics2 等） | 公共代码 | 常驻 | BW-03 | `programming/bigworld/common/`；py_network.hpp、py_physics2.cpp 等 | 已登记依赖 |
| examples：cellapp_extension、client_integration | 示例 | `user_shouldBuildExamples`（非 0 才建） | BW-14 | `examples/Makefile.rules`；产物 `cellapp-extensions/cellapp_example.so` 哈希在清单 | 已登记依赖 |
| simple_python_client | 示例 | examples 开关 | BW-14 | 已构建 ELF，哈希在清单（唯一参编示例） | 已登记依赖 |
| FantasyDemo（`programming/fantasydemo`，108 文件） | 示例工程 | 资源/脚本入仓，无构建目标 | BW-14 | res/scripts、res/server、web/php、`.kbengine/stubs`（遗留 KBEngine 生成物）；`fantasydemo.wpr` | 已登记依赖 |
| `docs/pdf/Server Installation Guide.pdf`、`Server Build Guide.pdf` | 文档 | 仅发行物 | BW-19 | 仓根 `README.md:68-69` 引用 | 暂缓（范围外） |

## 发行物、SDK 与缺失资源

| 原版目标 | 类别 | 启用条件 | 任务号 | 夹具/证据 | 状态 |
| --- | --- | --- | --- | --- | --- |
| bigworld-bwmachined-14.4.1.el7.rpm（4.39MB，唯一有内容） | 发行物 | 常驻 | BW-15 / BW-22 | sha256 fbdfe31b…；装 /etc/bwmachined.conf、sysctl 片段、/opt/bigworld/14.4/bwmachined/ | 已覆盖 |
| bigworld-devel-…rpm（2.1KB） | 发行物 | 常驻 | BW-15 | `rpm -qlp` = 空包（no files） | 已覆盖 |
| bigworld-web-integration-…rpm（1.9KB） | 发行物 | 常驻 | BW-15 | 空包 | 已覆盖 |
| 每日 tar.gz + deb + rpm（Linux）、zip + SHA256 manifest（Windows） | 发行物 | nightly/日常，**不打 tag 不发 release** | BW-22 | `scripts/package-daily{,-windows}`；版本=vcpkg.json 14.4.1 + `+daily<UTC>.<短sha>` | 已登记依赖 |
| BW SDK（商业产品线） | SDK | 不在开源树 | BW-14 | `find -iname "*sdk*"` 无命中（仅 tools/visual_studio/visualizers） | 缺口 |
| client_blob / tools_blob / extras | 发行物 | 预编译 blob，源缺失 | BW-13 / BW-11 | 无命中 | 缺口 |
| libbwentity.a | 库 | 仅 CMake target，make 链无产物 | BW-04 | `build/el7/lib/` 无此文件 | 缺口 |
| _bw_profile | 工具 | 条件 goal | BW-23 | 本 el7 树未产出 | 缺口 |
| game/res（资源树） | 资源 | res install 规则随全量构建 | BW-13 | `game/res/bigworld/`（含 scripts/） | 已覆盖 |

## 完整性复核（BW-00-05-06）

台账共 **65 行**原版目标（5 段数据表，状态列全数），每个启用目标均有
运行证据或显式缺口：

- **已覆盖 10 行**：libbuild、unit_test_lib/testing、三方捆绑 9 项、
  process_defs、bwmachined rpm、devel/web-integration 空包×2、game/res、
  21 个 `*_test`（含 6 模块空注册表的显式登记）。
- **已登记依赖 38 行**：由在建 crate 承接，逐行给出任务号。
- **缺口 10 行**：SDK、blob×3、libbwentity.a、_bw_profile、extras、
  客户端族 3 项（含 client_blob）、导出器族、assetprocessor 族。
- **暂缓（范围外）7 行**：客户端/导出器/编辑器族，源在树但本仓无对应面，
  逐行登记为不立项；含 bwentity 库、fantasydemo、文档 PDF。

运行时基线类缺口（集群启动、最小客户端登录、Windows 基线）不在本台账
行内，见 todo.md BW-00-02-03/-04/-05 的阻塞登记。
启用条件列指向 BW-00-01-01/-02 的宏与平台登记，未重复展开。