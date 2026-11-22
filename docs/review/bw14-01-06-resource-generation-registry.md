# BW-14-01-06 登记资源生成与完整包

**验收** 每个样本可追溯至来源。
**结论** 21 项生成资源与完整 res 包逐项登记来源/版本/可验路径；每项可追溯到生成器内嵌常量与参考路径，仓内跟踪字节一致镜像。

## 1. 生成器源

- 脚本：`programming/bigworld/scripts/generate-fantasydemo-assets.py`（参考仓；`scripts/` 下）。
- 版本：参考 HEAD `167688a4`（`bigworld-14.4.1-367-g167688a4`）时采集。生成器确定性输出（内容为脚本内嵌常量 + 文件头 marker），同脚本任意提交重跑字节一致。
- 输出基址：`programming/fantasydemo/res/`（`OUT_BASE`）。共 21 项（`FILES` 元组），每项 = 一个名常量。
- 运行方式：`scripts/generate-fantasydemo-assets.py [repo-root]`（根需含 `programming/`）。

## 2. 完整资源包 = 21 项生成资源

包 = `programming/fantasydemo/res/`：1 份服务器配置（`server/bw.xml`）+ 20 份脚本资源（`scripts/`）。每项来源 = 生成器常量；跟踪镜像 = `docs/review/fixtures/fantasydemo_def/res/`（2026-11-22 采集，`diff -r` 与参考 `programming/fantasydemo/res/` 字节一致）。

| # | 输出路径（res/ 下） | 生成器常量 | 说明 |
|---|---|---|---|
| 1 | `server/bw.xml` | `BW_XML` | 最小自包含服务器配置（personality=Fantasydemo、baseApp.archivePeriod、billingSystem 未知用户→Avatar） |
| 2 | `scripts/entities.xml` | `ENTITIES_XML` | 5 实体清单（Account/Avatar/Guard/Space/ClientOnly） |
| 3 | `scripts/db.xml` | `DB_XML` | DB 种子：LogOnMapping（FantasyAccount/a→Account/entityID=2）+ Account/Avatar 行 |
| 4 | `scripts/entity_defs/alias.xml` | `ALIAS_XML` | 类型别名（OBJECT_ID/ANGLE/ITEM_ID，占位未用） |
| 5 | `scripts/entity_defs/Account.def` | `ACCOUNT_DEF` | base-only 代理：playerName/character，chatMessage |
| 6 | `scripts/entity_defs/Avatar.def` | `AVATAR_DEF` | player proxy：playerName/prop1/prop2/prop3，logOff(fromCell/cellTick) |
| 7 | `scripts/entity_defs/Guard.def` | `GUARD_DEF` | cell+client NPC 占位：name |
| 8 | `scripts/entity_defs/Space.def` | `SPACE_DEF` | DefaultSpace bootstrap：无属性/方法 |
| 9 | `scripts/entity_defs/ClientOnly.def` | `CLIENT_ONLY_DEF` | client-only：name/prop1，method1 |
| 10 | `scripts/base/Account.py` | `BASE_ACCOUNT_PY` | base 占位 |
| 11 | `scripts/base/Avatar.py` | `BASE_AVATAR_PY` | base 占位 |
| 12 | `scripts/base/Guard.py` | `BASE_GUARD_PY` | base 占位 |
| 13 | `scripts/base/Space.py` | `BASE_SPACE_PY` | base 占位 |
| 14 | `scripts/cell/Avatar.py` | `CELL_AVATAR_PY` | cell 占位 |
| 15 | `scripts/cell/Guard.py` | `CELL_GUARD_PY` | cell 占位 |
| 16 | `scripts/cell/Space.py` | `CELL_SPACE_PY` | cell 占位 |
| 17 | `scripts/client/Account.py` | `CLIENT_ACCOUNT_PY` | client 占位 |
| 18 | `scripts/client/Avatar.py` | `CLIENT_AVATAR_PY` | client 占位 |
| 19 | `scripts/client/ClientOnly.py` | `CLIENT_CLIENT_ONLY_PY` | client 占位 |
| 20 | `scripts/client/Guard.py` | `CLIENT_GUARD_PY` | client 占位 |
| 21 | `scripts/client/Space.py` | `CLIENT_SPACE_PY` | client 占位 |

### 可验路径

- 参考原始：`/home/cui/workspaces/BigWorld/programming/fantasydemo/res/`（字节源）。
- 仓内跟踪镜像：`docs/review/fixtures/fantasydemo_def/res/`（21 文件，`diff -r` 与参考一致）。
- 摘要吻合（01-01，845831f）：`crates/entitydef/tests/bwentity_load.rs::fantasydemo_loads_the_five_sample_definitions` 逐类断言五类定义摘要吻合参考 `process_defs` 捕获（`docs/review/reference/fantasydemo_entitydef.json`），全树 digest 吻合。

## 3. 边界（不属本项）

- **美术资源**（meshes/models/visuals/spaces/textures/UI）：生成器明确**不产出**（"the art side stays with whoever supplies real fantasydemo assets"）——本包是引擎形状的占位资源层，非美术代餐；美术面归 BW-14-01-06 后续「完整包」的资源来源登记（若引入真实 fantasydemo 资产）。
- **server/development_defaults.xml**：参考 client-integration 示例引用的父配置在参考仓 `bigworld/res` 不存在；本 `bw.xml` 自含所需设置（生成器注释已注），不外链。

## 4. 结论

每项生成资源与完整 res 包均可追溯：来源（生成器常量）+ 参考路径 + 仓内跟踪字节一致镜像 + 01-01 摘要吻合。验收「每个样本可追溯至来源」满足。
