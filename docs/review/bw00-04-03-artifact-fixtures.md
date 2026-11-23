# BW-00-04-03 — 采集资源、实体和数据库夹具

验收：每份可追溯原版版本与输入。

本任务把参考栈对**资源**、**实体**、**数据库**三种输入的产物固化为基准
样本。三份夹具都由参考栈自身（BigWorld `8a74479`，x86-64 Linux）从真实
输入产生，每份都记录参考版本与输入文件 SHA-256，可追溯。

- 参考栈：BigWorld `8a74479c78773d89f1c44ce9a84c804730514cda`，
  `programming/bigworld` 预编译静态库（`build/cmake-entitydef/...`）。
- 输入树：`programming/fantasydemo/res`（仓库提交的占位资产树，
  `test_fantasydemo_assets.cpp` 加载的那套）。
- 采集工具：`docs/review/tools/capture_artifacts.cpp`，编译命令见文件头注释。
- 原始输出：`docs/review/data/bw00-04-03-logs/capture.txt`。
- 基准样本：`docs/review/fixtures/artifacts/`（清单 `MANIFEST.txt`）。
- 确定性：两次运行 stdout 完全一致。

## 1. 资源夹具

四份资源文件经参考 `BWResource`/`DataSection` 解析后的规范树
（`resource-*.tree.txt`）：

| 输入 | 输入 SHA-256 | 夹具 |
|------|--------------|------|
| `scripts/entity_defs/Avatar.def` | `e35472f0...1fa50fa` | `resource-Avatar.def.tree.txt` |
| `scripts/entities.xml` | `b2e348d5...6371951` | `resource-entities.xml.tree.txt` |
| `scripts/db.xml` | `412252c6...428d140` | `resource-db.xml.tree.txt` |
| `server/bw.xml` | `212eecde...8a4d79` | `resource-server-bw.xml.tree.txt` |

`Avatar.def` 树捕获：`Volatile`（position/yaw/pitch=20）、`Properties`
（playerName STRING/ALL_CLIENTS/Persistent/Editable/Identifier；prop1
INT32/BASE_AND_CLIENT/Default 1；prop2 INT16/ALL_CLIENTS/Default 2/Persistent；
prop3 INT8/OTHER_CLIENTS/Default 3）、`ClientMethods`（fromCell STRING、
cellTick INT32）、`CellMethods`（空）、`BaseMethods`（logOff Exposed）。

`entities.xml`：`ClientServerEntities` 列 Account/Avatar/Guard/Space/ClientOnly；
`ServerOnlyEntities` 空。

`db.xml`：`_BigWorldInfo/LogOnMapping`（logOnName=FantasyAccount, type=Account,
entityID=2）、Account（playerName=FantasyAccount, character=FantasyHero,
databaseID=2）、Avatar（playerName=FantasyHero, prop1=1, prop2=2, prop3=3,
position 0,0,0, direction 0,0,0, spaceID=1, databaseID=1）。

`server/bw.xml`：personality=fantasydemo；baseApp archivePeriod=0；billingSystem
shouldAcceptUnknownUsers=true, entityTypeForUnknownUsers=Avatar。

## 2. 实体夹具

`EntityDescription::parse("Avatar","",NULL)` 解析 `Avatar.def` 得到的
`entity-Avatar.meta.txt`：

- name `Avatar`，index `65535`（未注册进 EntityType 映射时的
  `NULL_ENTITY_TYPE_ID`），propertyCount 4，canBeOnCell 1。
- 属性：playerName STRING persistent streamSize -1（变长）；prop1 INT32
  non-persistent 4；prop2 INT16 persistent 2；prop3 INT8 non-persistent 1。

**实体快照** `entity-Avatar.snapshot.hex`（15 字节）：固定输入值节
`<playerName>Admiral</playerName><prop1>100</prop1><prop2>200</prop2>
<prop3>300</prop3>`，经 `addSectionToStream(BASE|CELL|CLIENT)`：

```
64 00 00 00 | 07 41 64 6d 69 72 61 6c | c8 00 | 2c
│  prop1 INT32 = 100 (LE)   │  playerName STRING: len 07 + "Admiral"  │  prop2 INT16 = 200  │  prop3 INT8 = 44
```

- 属性写入序为 prop1、playerName、prop2、prop3（按数据域 pass 排序，非
  def 声明序）。
- prop3 输入 300 溢出 INT8，参考截断为 `0x2c`（44），并发出
  `MF_ASSERT_DEV` 告警（`integer_data_type.cpp:127`）——这是参考对越界
  输入的真实行为，夹具按截断值固化。

## 3. 数据库夹具

`database-Avatar.row.hex`（38 字节）：参考 `XMLDatabase::getEntity` 的
行负载公式（`lib/db_storage_xml/xml_database.cpp`）——
`addSectionToStream(BASE|CELL|ONLY_PERSISTENT_DATA)`，实体 `canBeOnCell`
时追加 `position`、`direction`、`spaceID`：

```
07 41 64 6d 69 72 61 6c | c8 00 | 00 00 80 3f | 00 00 00 40 | 00 00 40 40 | 00 00 00 00 00 00 00 00 00 00 00 00 | 07 00 00 00
│  playerName STRING: len 07 + "Admiral"  │  prop2 INT16 = 200  │  position (1.0, 2.0, 3.0) float×3  │  direction (0,0,0) float×3  │  spaceID INT32 = 7
```

- 只含持久属性 playerName、prop2（prop1/prop3 非持久被排除）✓
- 空间数据按 `position/direction/spaceID` 顺序追加 ✓

## 复用说明

差异报告（BW-00-04-06）以这三类夹具为基准：原版全栈与新版全栈分别产出
资源树/实体快照/数据库行，与基准逐字段比较；实体与数据库夹具已把
`index 65535`（未注册类型）、`streamSize -1`（变长）及 INT8 截断行为纳入，
比较时按可忽略字段口径处理运行时差异。
