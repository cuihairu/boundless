# BW-00-05-04 公共代码、示例与 FantasyDemo 索引

> 证据面（映射验收：脚本及资源入口可追溯）。

入口点分三类：

- **公共代码**——嵌入 Python 运行时的 stage（`game/res/bigworld/scripts/`：`common/` 与 `server_common/`，含标准库与动态扩展模块 stage）；
- **FantasyDemo**——`programming/fantasydemo/`：实体脚本（base/cell/client）、实体定义（`entity_defs/`）、配置（`db.xml`/`entities.xml`/`res/server`）与项目文件；
- **示例**——`programming/bigworld/examples/`：客户端集成示例与 cellapp 扩展。

每行给出入口点（路径）、`任务号` + `夹具`，或具体缺口来源——脚本与资源入口由此可追溯：

| 入口点 | 路径 | 文件数 | 任务号 | 夹具 | 状态 | 说明 |
| --- | --- | --- | --- | --- | --- | --- |
| `bw_site.py` | `game/res/bigworld/scripts/common/bw_site.py` | 1 | BW-00-03-03 | bw00-04-02-python-callback-fixtures | 已覆盖 | 嵌入 Python 宿主 site 钩子（解释器启动面） |
| `Lib` | `game/res/bigworld/scripts/common/Lib` | 2325 | BW-00-03-03 | bw00-04-02-python-callback-fixtures | 已覆盖 | Python 标准库 stage（嵌入 sys.path） |
| `site-packages` | `game/res/bigworld/scripts/common/site-packages` | 670 | BW-00-03-03 | bw00-04-02-python-callback-fixtures | 已覆盖 | site-packages stage |
| `lib-dynload-el7` | `game/res/bigworld/scripts/server_common/lib-dynload-el7` | 59 | BW-00-03-03 | bw00-04-02-python-callback-fixtures | 已覆盖 | 动态扩展模块 stage |
| `site-packages` | `game/res/bigworld/scripts/server_common/site-packages` | 1224 | BW-00-03-03 | bw00-04-02-python-callback-fixtures | 已覆盖 | site-packages stage |
| `base` | `programming/fantasydemo/res/scripts/base` | 4 | 未映射 | — | 缺口 | 独立游戏资产；本机证据均取自 simple_python_client 示例（BW-00-02-04/BW-00-04-03），FantasyDemo 未被任何证据练习 |
| `cell` | `programming/fantasydemo/res/scripts/cell` | 3 | 未映射 | — | 缺口 | 独立游戏资产；本机证据均取自 simple_python_client 示例（BW-00-02-04/BW-00-04-03），FantasyDemo 未被任何证据练习 |
| `client` | `programming/fantasydemo/res/scripts/client` | 5 | 未映射 | — | 缺口 | 独立游戏资产；本机证据均取自 simple_python_client 示例（BW-00-02-04/BW-00-04-03），FantasyDemo 未被任何证据练习 |
| `entity_defs` | `programming/fantasydemo/res/scripts/entity_defs` | 6 | 未映射 | — | 缺口 | 独立游戏资产（Avatar.def 等）；未被任何证据练习 |
| `db.xml` | `programming/fantasydemo/res/scripts/db.xml` | 1 | 未映射 | — | 缺口 | 独立游戏资产；未被任何证据练习 |
| `entities.xml` | `programming/fantasydemo/res/scripts/entities.xml` | 1 | 未映射 | — | 缺口 | 独立游戏资产；未被任何证据练习 |
| `server` | `programming/fantasydemo/res/server` | 1 | 未映射 | — | 缺口 | 独立游戏资产；未被任何证据练习 |
| `fantasydemo.wpr` | `programming/fantasydemo/fantasydemo.wpr` | 1 | 未映射 | — | 缺口 | 工作区项目文件；无对应编辑器（Windows 面） |
| `simple` | `programming/bigworld/examples/client_integration/python/simple` | 43 | BW-00-02-04 | bw00-02-04-client-login | 已覆盖 | 登录驱动与实体脚本 |
| `android` | `programming/bigworld/examples/client_integration/c_plus_plus/android` | 21 | 未映射 | — | 缺口 | 客户端 C++ 示例（Android）；无对应实现 |
| `ios` | `programming/bigworld/examples/client_integration/c_plus_plus/ios` | 489 | 未映射 | — | 缺口 | 客户端 C++ 示例（iOS）；无对应实现 |
| `cellapp_extension` | `programming/bigworld/examples/cellapp_extension` | 17 | BW-00-01-01 | bw00-01-build-targets | 缺口 | cellapp 扩展示例；参考侧已盘点，无运行时练习证据 |

## 汇总

| 项 | 数 |
| --- | --- |
| 入口点 | 17 |
| 覆盖文件总数 | 4871 |
| 已覆盖（可追溯） | 6 |
| 缺口（带来源） | 11 |
| 类别 | FantasyDemo 实体定义 1、FantasyDemo 实体脚本 3、FantasyDemo 配置 3、FantasyDemo 项目 1、公共代码 5、示例 4 |

未索引 = 0：脚本与资源入口点逐项可追溯（任务号+夹具或缺口来源）。

**已覆盖**的入口点：`公共代码` 运行时 stage（登录捕获期间嵌入解释器经其导入脚本/标准库，BW-00-02-04/BW-00-04-02）与 `simple_python_client` 示例（BW-00-02-04 登录驱动）。

**缺口**的入口点：`FantasyDemo` 全部（独立游戏资产，本机证据均取自 simple_python_client 示例——bw00-04-03 的 24 份资源夹具为该示例的 db.xml/entities.xml/5 个 `.def`/base/cell/client/common 脚本，与 FantasyDemo 的 Avatar.def 实体集不同）与客户端 C++/cellapp 扩展示例（无对应实现）。

## 可追溯性

- 索引：`reference/bw00_05_04_script_resource_index.json`。
- 资源夹具：`reference/bw00_04_03_entity_db_fixtures.json`（24 份，带路径+SHA-256+字节）。
- 登录驱动：`reference/bw00_02_04_client_login.json`。

生成：`script_resource_index.py` + `script_resource_index_doc.py`。
