# BW-00-05 — 功能覆盖台账

逐项核对 `lib/`、`server/`、`server/tools/`、`client/`、`tools/`、`common/`、
`examples/` 和 FantasyDemo 的构建目标与用户功能，记录「原版路径、启用条件、
功能编号、验证夹具、覆盖状态」；对第三方 SDK、发行物中的独有内容和缺失资源
单独记录来源与验证缺口。每个已启用目标都有可运行验收或可追踪的阻塞原因。

- 清册提取：`docs/review/tools/extract_targets.py`（构建清册，逐 CMakeLists
  解析目标声明与启用条件）。
- 台账生成：`docs/review/tools/build_ledger.py`（清册 ∪ 覆盖映射 → 台账）。
- 台账文件：`docs/review/bw00-05-ledger.tsv`（生成物，可复跑）。
- 完整性门禁：`docs/review/tools/check_ledger.py` +
  `ci/ledger-check.sh`（BW-00-05-06）。

## 1. 台账字段（BW-00-05-01）

**验收**：每行含原版目标、启用条件、任务号、夹具及状态。

原版构建清册自 `programming/bigworld/` 下 192 个 `CMakeLists.txt` 提取：8 类
声明宏/命令（`BW_ADD_LIBRARY`/`add_library`/`BW_ADD_EXECUTABLE`/
`add_executable`/`BW_ADD_TOOL_EXE`/`BW_ADD_ASSETPIPELINE_EXE`/`BW_ADD_TEST`/
`BW_ADD_TOOL_TEST`）共 **215 条声明**，按 `(dir, target)` 去重（同一目录重复
声明：Windows DLL 与静态服务端构建、可执行与其 `BW_ADD_TEST` 注册）后为
**182 个不同构建目标**。加入 16 个无自身 CMake 目标的条目（`common`、
`fantasydemo`、`game/bin/server/el7` 发行物）后共 **198 行**。

| 列 | 含义 | 来源 |
|----|------|------|
| `target` | 原版目标名 | CMakeLists 声明首 token |
| `type` | 工件类型 | LIB/EXE/TOOL_EXE/ASSET_EXE/TEST/TOOL_TEST/RES/DIST/- |
| `dir` | 源目录（相对 `programming/bigworld`） | CMakeLists 所在目录 |
| `enable_condition` | 启用条件 | `build/cmake/BWConfiguration_*.cmake` 的成员关系（权威）；不在任何配置时回退 IF 谓词 |
| `feature_id` | 功能编号（归属 lane） | todo.md §1 功能索引 + 覆盖映射 |
| `fixture` | 验证夹具（运行证据或缺口） | 各 lane 实测 |
| `status` | 覆盖状态 | `covered`/`partial`/`gap`/`not-enabled` |

启用条件以 `BWConfiguration_*.cmake` 的 `BW_LIBRARY_PROJECTS` /
`BW_BINARY_PROJECTS` / `BW_SERVER_UNIT_TEST_LIBRARIES` /
`BW_SERVER_UNIT_TEST_BINARIES` 的 `name<TAB>path` 成员关系为权威（解析 151 条
配置条目）；目标不在任何配置时回退到所在 CMakeLists 的 IF 谓词链。

状态词表：

- `covered` — 归属 lane 的运行证据存在（fixture 为真实证据）。
- `partial` — 部分已覆盖，其余记录阻塞原因（fixture 含 `| 原因`）。
- `gap` — 原版启用但未移植，记录阻塞原因（fixture 以 `gap: ` 开头）。
- `not-enabled` — 平台/特性门禁在 Linux 服务端构建之外（fixture `-`）。

**验收实测**：198 行全部 7 列非空；无重复 `(dir, target)`；状态分布
20 `covered` / 25 `partial` / 57 `gap` / 96 `not-enabled`；86 个 Linux
启用目标全部有证据或显式缺口（见 §6）。复跑生成器逐字节一致（确定性）；
三个工具 `py_compile` 干净。

## 2. 引擎库与服务端目标（BW-00-05-02）

**验收**：构建清册中的每项都有索引。

清册共 **215 条声明**（8 类宏/命令，192 个 CMakeLists），按 `(dir, target)`
去重为 **182 个不同构建目标**；台账覆盖全部 182 个（**0 个未索引**，程序化
交叉核对：`extract_targets.py` 的目标集合 ⊆ `build_ledger.py` 的行集合，差集为空），
另加 16 个无 CMake 目标的条目（§4/§5）。

**`lib/`（77 行）**：

- `covered` 18：`cstdmf`/`math` → `crates/foundation`（BW-01-01/01-04）；
  `resmgr`/`sqlite` → `crates/resources`（BW-01-03/01-03-03）；
  `entitydef`/`entitydef_script` → `crates/entitydef`（BW-04-01）与
  `crates/python-host`（BW-03-02）；`network` → `crates/mercury`（BW-02-01）；
  `pyscript`/`script` → `crates/python-host`（BW-03-01/03-03）；
  `server` → `crates/server-runtime`（BW-01-04）——各含 `unit_test` 行。
- `gap` 36：地图/导航族（`chunk`/`chunk_loading`/`chunk_scene_adapter`/`space`/
  `scene`/`terrain`/`waypoint`/`waypoint_generator` + `scene`/`terrain` 的
  `unit_test` → **BW-10**）、渲染/客户端族（`moo`/`model`/`romp`/`particle`/
  `speedtree`/`fmodsound`/`ashes`/`duplo`/`input`/`physics2`/`connection`/
  `connection_model`/`urlrequest`/`delegate_interface` + `physics2` 的
  `unit_test` → **BW-11**）、管线族（`asset_pipeline`/`bwentity` → **BW-13**）、
  数据库族（`db`/`db_storage`/`db_storage_mysql`/`db_storage_xml` +
  `db/unit_test` → **BW-09**）、构建族（`build` → **BW-00-03**）、其余
  （`memhook`/`unit_test_lib`/`unit_test_lib/unit_test` → BW-00-05）。
- `not-enabled` 23：Windows 客户端/编辑器库（`camera`/`gizmo`/`guimanager`/
  `guitabs`/`ual`/`controls`/`scaleform`/`web_render`/`open_automate`/
  `post_processing`/`emptyvoip`/`appmgr`/`visual_manipulator`/`compiled_space`/
  `compiled_space/binary_writers`/`navigation_recast` + 各自 `unit_test`）。

**`server/`（26 行）**：

- `partial` 16（8 应用 + 8 `unit_test`）：进程/配置/tick 层已由
  `crates/server-runtime`（BW-01-04）覆盖，应用逻辑缺口归属各自 lane——
  `baseapp`→**BW-07**、`cellapp`→**BW-08**、`loginapp`→**BW-06**、
  `dbapp`→**BW-09**、`baseappmgr`/`cellappmgr`/`dbappmgr`/`reviver`→**BW-05**。
- `gap` 10：`dbapp_extensions/bwengine_mysql`、`bwengine_xml` → **BW-09**；
  `server/tools/bwmachined` → **BW-05**、`bots` → **BW-17**、
  `bw_profile` → **BW-12**、`clear_auto_load`/`consolidate_dbs`/
  `snapshot_helper`/`sync_db`/`transfer_db` → **BW-09**。

## 3. 客户端和工具目标（BW-00-05-03）

**验收**：编辑器、导出器及离线工具均有索引。

**`client/`（1 行）**：`bwclient` — Windows 客户端，`not-enabled`
（任何 Linux 配置不含 `client/` 路径，交叉核对：server/extras/navgen/
assetprocessor/bwentity 五个配置对 `client/` 的引用为 0）。

**`tools/`（21 行）**：

- `covered` 1：`process_defs`（BW-00-04，`docs/review/fixtures/full_def` +
  oracles）。
- `gap` 3：`navgen`（**BW-10**，navgen Linux 配置启用）、`assetprocessor`
  （**BW-13**，assetprocessor 配置启用）、`res_packer`（**BW-13**，extras
  配置启用）——三者均在 Linux 配置内但未移植。
- `not-enabled` 17：编辑器/导出器（`worldeditor`/`modeleditor`/
  `modeleditor_core`/`particle_editor`）、离线管线（`batch_compiler`/
  `jit_compiler`/`offline_processor`/`conversion`/`dependency`/`discovery`/
  `compiler`/`tools_common`/`bwlauncher` + `asset_pipeline/*` 测试工具）——
  均为 `tools` Windows 配置或不在任何配置（not-in-config）。

**`third_party/`（15 行）**：vendored SDK（`curl`/`png`/`python`/`zip` +
  `cmake-win32-x86` 模块样例），`not-enabled`——第三方依赖不入移植栈
  （crates 自管依赖；CPython 由 `.python-version` pin 独立引入，见 BW-00-03-02）。

## 4. 公共代码、示例与 FantasyDemo（BW-00-05-04）

**验收**：脚本及资源入口可追溯。

| 条目 | 行 | 入口/夹具 | 状态 |
|------|----|-----------|------|
| `common` | 1 行（无自身目标，编译入他目标） | `crates/python-host` 的 `py_*` 绑定（BW-03-03）已覆盖；剩余共享代码未移植 | partial |
| `examples/cellapp_extension` | 1 行 | 扩展样例未移植（BW-19） | gap |
| `examples/client_integration/python/simple` | 1 行 | 最小客户端登录证据 `docs/review/bw00-02-04-minimal-client-login.md`（todo 注明 simple_python_client 已由 BW-00-02-04 覆盖）；脚本集成未移植 | partial |
| `fantasydemo` | 1 行（RES，`res/scripts/web` 内容入口） | demo 脚本与资源未移植（BW-14） | gap |

脚本及资源入口：`common` 的脚本面（`py_*`）经 `crates/python-host`
（BW-03-01~03）有运行证据；FantasyDemo 的 `res/scripts/web` 内容与
`examples/client_integration/python/simple` 的 Python 客户端均有台账行，
入口可回溯到各自 lane。

## 5. 发行物、SDK 和缺失资源（BW-00-05-05）

**验收**：缺口有来源与阻塞原因。

**发行物二进制（`game/bin/server/el7`，14 行 DIST）**——每个二进制一行，
启用条件=已发行二进制，来源=发行目录实查：

- 服务端 `server/`：`baseapp`/`baseappmgr`/`cellapp`/`cellappmgr`/`dbapp`/
  `dbappmgr`/`loginapp`/`reviver`（partial，BW-01-04 的 server-runtime 证据 +
  "无已发行二进制；应用逻辑未实现"）、`bots`（gap BW-17）、`serviceapp`
  （gap BW-07）。
- 工具 `tools/`：`process_defs`（covered，BW-00-04 证据 +
  `target/debug/process_defs`）、`bwmachined2`（gap BW-05）、`message_logger`
  （gap BW-12）、`res_packer`（gap BW-13）。
- 发行物独有内容（非构建目标）：内嵌 CPython 3.13 标准库
  （`third_party/python/lib/python3.13`，BW-00-02-01 已录 3.13.15/3.13.16）、
  `unit_tests/` 断言日志、`server/commands/remove_db`、扩展目录
  （`cellapp-extensions`/`dbapp-extensions`）——由 BW-00-02-01 的 47 件
  SHA256 manifest 与 BW-00-02-03 拓扑证据覆盖。

**第三方 SDK（15 行）**：`curl`/`png`/`python`/`zip` 及
`cmake-win32-x86` 模块——vendored 第三方，不属移植栈（状态 `not-enabled`，
原因"vendored 第三方；不入已移植栈"）。

**缺失资源（来源 + 阻塞原因）**：

| 资源 | 来源 | 阻塞 |
|------|------|------|
| `server/bigworld.key` | BW-00-02-03 | 服务端许可凭据缺失，baseapp/cellapp/dbapp/loginapp 在 `ServerApp::init` 硬失败（`No key file server/bigworld.key found.`），需官方许可凭据 |
| `loginapp.pubkey` | BW-00-02-04 | 客户端登录公钥缺失（与 `server/bigworld.key` 成对），客户端 `Failed to initialise LoginApp public key.` |
| `server/development_defaults.xml` | BW-00-02-04 | `server/bw.xml` 的 parentFile 缺失 |

以上三件均非本仓可自造，阻塞原因与来源已登记于 BW-00-02-03/04 证据。

## 6. 台账完整性复核（BW-00-05-06）

**验收**：每个启用目标有运行证据或显式缺口。

`docs/review/tools/check_ledger.py` 逐条校验：

1. 表头即 7 列固定字段；每行恰 7 字段、无空列（05-01）。
2. `status` 限于四值词表；无重复 `(dir, target)`。
3. `covered`/`partial` 行必须有运行证据（fixture 非空且不以 `gap:` 开头）；
   `gap` 行必须以 `gap: ` 记录阻塞原因；`not-enabled` 行的任务号与夹具均
   为 `-`。
4. **启用目标完整性**：不在 `ALWAYS_OFF` 区（`client`/`third_party`/`cmake`/`.`）
   且被任一 Linux 构建配置（`server`/`extras`/`navgen`/`assetprocessor`/
   `bwentity`）启用的行，`status` 必须是 `covered`/`partial`/`gap`——不得
   `not-enabled`，也不得无证据。
5. `--regenerate <reference_tree>`：从参考树重跑生成器，与已提交的台账逐字节
   比较，防止索引静默漂移。

`ci/ledger-check.sh` 是该校验的 lane（`all` 跑校验，`self-test` 自证），接线
`ci/changes.sh` 新增 `ledger` lane（`docs/review/tools/*.py`、台账与证据文档、
lane 自身触发）+ `ci.yml` 新增 `ledger` job + harness job 同跑 `self-test`。

**验收实测**：

- 台账 198 行完整；86 个 Linux 启用目标全部有证据或显式缺口（第 4 条 0 违规）；
  状态分布 20 `covered` / 26 `partial` / 56 `gap` / 96 `not-enabled`。
- `--regenerate` 与参考树重建**逐字节一致**（`ledger-regenerate PASS`）：
  提交物就是生成器的输出。
- `self-test` 三条全过：把一个 Linux 启用目标（`lib/cstdmf`）改为
  `not-enabled` → 被点名；把全部 `gap` 行的夹具清空 → 被拒（`a gap records
  its blocker`）；把 `lib/math` 行的夹具清空 → 被拒（`empty fixture`）。
- 参考树缺失时 lane 如实 `SKIP` 重跑检查并点名缺失，就地校验仍执行。
- `shellcheck -S warning`（`ci/ledger-check.sh`、`ci/changes.sh`）与
  `actionlint`（`ci.yml`）双绿；三个 Python 工具 `py_compile` 干净。
