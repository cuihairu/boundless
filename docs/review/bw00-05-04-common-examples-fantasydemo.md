# BW-00-05-04 映射公共代码、示例与 FantasyDemo — 记录

- **任务编号**: BW-00-05-04（lane BW-00-05 功能覆盖台账，todo.md 行 147）
- **验收**: 脚本及资源入口可追溯。

## 结论

台账新增 **14 行 `origin=resource`**（公共代码源入口 / 示例 / FantasyDemo /
生成脚本），总行数 137→151。参考树入口全部可追溯（每行带任务号+状态）。

## 登记的入口（14 行）

| 行（target|dir） | 类型 | 任务号 | 说明 |
|---|---|---|---|
| py_chunk.cpp | source | BW-10-01 | common/ Chunk Python 绑定 |
| py_network.cpp | source | BW-02 | common/ 网络 Python 绑定 |
| py_physics2.cpp | source | BW-10-03 | common/ 物理 Python 绑定 |
| py_terrain.cpp | source | BW-10-03 | common/ 地形 Python 绑定 |
| py_server_lib.cpp | source | BW-03-03 | common/ 服务端脚本库 |
| py_timer.cpp | source | BW-03-03 | common/ 定时器 Python 绑定 |
| py_urlrequest.cpp | source | BW-02-04 | common/ HTTP Python 绑定 |
| simple_client_entity.cpp | source | BW-14-02 | common/ SDK 样例客户端实体基类 |
| space_data_types.hpp | source | BW-10-01 | common/ SpaceData 类型头 |
| client_integration_cpp | script | BW-14-02 | examples 的 C++ 客户端集成样例（android/ios 子目录） |
| fantasydemo_res | datadir | BW-14-01 | Demo 资源树（server 配置+entities/defs/base/cell/client 脚本，占位由生成脚本产出） |
| fantasydemo_web_php | script | BW-14-03 | PHP Web 集成（Authenticator/BigWorld.php 等） |
| fantasydemo_kbengine | legacy | BW-29 | KBEngine 时代遗留（debug.json/stubs），随 BW-29 差异复核 |
| generate-fantasydemo-assets.py | script | BW-14-01 | 参考树生成脚本；Boundless 侧未移植（仓内无 scripts/） |

## 追溯口径

- **归属**：任务号按消费 lane（公共代码随其被编入的库/进程 lane；示例与
  FantasyDemo 归 BW-14）。`fantasydemo.wpr`（Wing IDE 工程文件，非资源入口）
  与 `scripts/package-daily*`（发行脚本）不在此处——前者在记录中注明，
  后者归 BW-00-05-05（origin=release，BW-14-04）。
- **不重复**：`examples/client_integration/python/simple`（=make 行
  `simple_python_client`）与 `examples/cellapp_extension`（=make 行
  `cellapp_example`）已按 make 行索引，不另建 resource 行；台账记功能
  覆盖，入口与目标同名时以目标行为准。
- **状态**：全部 `pending`（Boundless 侧尚未移植/未验证）；阻塞面无——这些
  入口的移植不被平台/许可挡，仅排期。

## 校验与门禁

- `python3 docs/review/tools/check_coverage_ledger.py`：**151 行全字段合法**，
  origin=make 79 项双向 1:1（cmake/resource 行豁免清单核对），rc=0。
- `--schema-only` rc=0；`py_compile` 过；`--scaffold` 空。
- 本项纯台账；未触 crate；workspace 级 cargo 测试未跑（兄弟会话在途债务无关）。

## 边界声明

本项完成公共代码/示例/FantasyDemo 入口索引（origin=resource）；发行物、
SDK、缺失资源（origin=release/gap）归 BW-00-05-05，完整性复核归 BW-00-05-06。

## 状态

- [x] 公共代码源入口可追溯（9 行，按消费 lane）
- [x] 示例与 FantasyDemo 入口可追溯（5 行，含生成脚本）
- [x] 同名入口不重复（python simple / cellapp_extension 用 make 行）
- [x] 校验绿（151 行，rc=0）
