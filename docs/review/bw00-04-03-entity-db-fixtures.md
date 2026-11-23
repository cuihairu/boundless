# BW-00-04-03 资源、实体和数据库夹具

> 证据面（BW-00-04-03 验收：每份可追溯原版版本与输入）。

由 `tools/entity_db_fixture_scan.py` 采集（只读，不驱动集群）：资源夹具取参考树文件，实体夹具取 `.def` 解析 + 数据库类型 ID + 一次真实登录的运行时实体，数据库夹具取 dbapp 落下的表结构与行。源数据 `reference/bw00_04_03_entity_db_fixtures.json`。

| 项 | 值 |
| --- | --- |
| 参考仓提交 | `4b637c8d` |
| 捕获时间 | `2026-11-23 02:45:48 +0800` |
| 模式 | `live`（集群为他人所有，只旁听、不驱动） |
| 数据库 | `bigworld@bigworld on 127.0.0.1`，`11.8.6-MariaDB-5ubuntu0.1 from Ubuntu` |
| 资源路径 | `/home/cui/workspaces/BigWorld/programming/bigworld/examples/client_integration/python/simple/res:/home/cui/workspaces/BigWorld/game/res/bigworld:/home/cui/workspaces/BigWorld/scripts/cluster-smoke/res` |
| 运行时实体 | `ClientAvatar` proxy_id `4805`，客户端 `192.168.5.188:55283` |
| 夹具数 | 资源 24、实体 5、表 16 |

## 资源夹具

每个文件记录路径、角色、SHA-256 与字节数；`source_commit` 即其版本。`.def`/`.py`/`.xml` 均取自客户端集成示例资产树，`bw_deep.xml` 是 deep-chain 实际运行的配置覆盖层。

| 角色 | 路径 | 字节 | SHA-256 |
| --- | --- | --- | --- |
| 数据库种子（dbapp 装载输入） | `programming/bigworld/examples/client_integration/python/simple/res/scripts/db.xml` | 1069 | `68bebf275113a44411d59a0e371e490dabbc1c46013a8ec26e36c549b5110557` |
| 实体注册表 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/entities.xml` | 160 | `17a4de53bf4bc3b218fe168782e7d3f6bc745f484c4eafc802fd0958a9c647a0` |
| 数据类型别名 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/alias.xml` | 404 | `776bcaf4710b7e804a4e599738dd53373f0a5c9398a1318fa9ff4df7eab9c843` |
| 实体定义（.def） | `programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/Account.def` | 537 | `5158a0b45a71f44a77e593860f2b7b5ed01ac1c0205fa17eedff50d1bcdb0dae` |
| 实体定义（.def） | `programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/ClientAvatar.def` | 1530 | `d038aad9c3474a41decd9d4d38f716753292e2001fe700c10fd7da3c6f4d2493` |
| 实体定义（.def） | `programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/Guard.def` | 216 | `72963aaf90473a88c98dce3c3da4869e40bcdc400dd7eb38bd49f8aefccbb0c5` |
| 实体定义（.def） | `programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/Space.def` | 144 | `8dc0224eaf00fa1126788366f6b52dde02e2bcf1f9350cdea16a47f60f52dbf5` |
| 实体定义（.def） | `programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/ClientOnly.def` | 706 | `e38e6189cd4249c36ca7bc7c4138aa496f1a8517924992d06f139c17391107a7` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/base/Account.py` | 2912 | `ee97a7cd98ab863aefe7dfa0d6dbb2b61d6ad5a954df44c385aa8e6c37e498f8` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/base/ClientAvatar.py` | 1237 | `5230f31f62b08fa4b6bf2ee28a8a6b7baad3f383d27c5f81400a0f533b600940` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/base/Guard.py` | 522 | `7490827ea55b55be136d26f42bb1be6ac2881fe53677d460a1caefbafdcd0bcf` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/base/Space.py` | 411 | `e3aba0f2aa9834a84d083c19a73acb47780c622c6da967d6594caa2fae9036c8` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/base/egclient4.py` | 259 | `23ed25ce223ceca10fa8b44478b14f1ced851b395affd81f8cdb32f590372862` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/cell/ClientAvatar.py` | 1273 | `b5ab12887761bba14fc94aec1e86766686716f5af9ebc137f902567a074c7113` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/cell/Guard.py` | 164 | `58902c474a0ba4920e1fbf4138a4fcfc2ad2a344a4a694326c89b501629852ca` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/cell/Space.py` | 134 | `caf981c714fb6fa2e46ac456ef226e442d290f43086995258e197776a1d9dd08` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/client/Account.py` | 778 | `66e1bb26b2b58e3a37b04c17c77df904194efa5509b4c7067cdd9acc271a1a2c` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/client/ClientAvatar.py` | 1507 | `7fdcadd5b327b58ed1622702df5fd32ca7dd4c3bb873069bf894dbf9cc4d6a02` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/client/ClientOnly.py` | 648 | `60d26595afb5f39ab2c69f1d1be7bae1d139e9cd45ee4409bbd4e9e17c665b54` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/client/Guard.py` | 201 | `3715d06bd67d4ef11b90486702df8ccb43643b013cea31bf3dbe4d279d66347a` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/client/Space.py` | 54 | `49a74738cae6589ea1d46768702cc6825a0d26808b295f2c8972b2ebe9642b70` |
| 实体脚本 | `programming/bigworld/examples/client_integration/python/simple/res/scripts/common/TestDataType.py` | 2827 | `205c65ee681ad5d992d20266f6112ff1099a821144cb39524c7fa93ca6365aa8` |
| 运行配置 | `programming/bigworld/examples/client_integration/python/simple/res/server/bw.xml` | 545 | `b2764784ce55a739457720b8492ee82d8ea152b115d338599e5d6036fd98fbdb` |
| 运行配置 | `scripts/cluster-smoke/res/server/bw_deep.xml` | 2334 | `d39deb1f59b60de3ab46a96bf61c6753dd3e84023f78126758f2d4c2f9e40c68` |

## 实体夹具

每个实体类型由 `.def` 解析（属性、可见性、持久化、默认值与方法），并接到数据库认识它的类型 ID（`bigworldEntityTypes`）。`ClientOnly` 无数据库类型 ID——客户端专属实体不参与服务端/数据库，这是预期。运行时实体取自 BW-00-02-04 真实登录：`ClientAvatar`（proxy_id `4805`）。

### `Account`

- 定义：`programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/Account.def`（SHA-256 `5158a0b45a71f44a77e593860f2b7b5ed01ac1c0205fa17eedff50d1bcdb0dae`，537 B）
- 数据库类型 ID：`3`；BigWorld ID `2`。
- Volatile：—。
- 方法：客户端 1、cell 0、base 0。

| 属性 | 类型 | 可见性 | 持久 | 默认 | 标识 |
| --- | --- | --- | --- | --- | --- |
| `playerName` | `STRING` | BASE_AND_CLIENT | true | `—` | — |
| `character` | `STRING` | BASE | true | `—` | — |

### `ClientAvatar`

- 定义：`programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/ClientAvatar.def`（SHA-256 `d038aad9c3474a41decd9d4d38f716753292e2001fe700c10fd7da3c6f4d2493`，1530 B）
- 数据库类型 ID：`2`；BigWorld ID `1`。
- Volatile：position, yaw, pitch。
- 方法：客户端 2、cell 0、base 1。

| 属性 | 类型 | 可见性 | 持久 | 默认 | 标识 |
| --- | --- | --- | --- | --- | --- |
| `playerName` | `STRING` | ALL_CLIENTS | true | `—` | true |
| `prop1` | `INT32` | BASE_AND_CLIENT | — | `1` | — |
| `prop2` | `INT16` | ALL_CLIENTS | true | `2` | — |
| `prop3` | `INT8` | OTHER_CLIENTS | — | `3` | — |
| `prop4` | `INT8` | CELL_PRIVATE | — | `4` | — |
| `prop5` | `USER_TYPE`<TestDataType.instance> | ALL_CLIENTS | true | `—` | — |
| `_timer1` | `INT32` | CELL_PRIVATE | — | `—` | — |
| `_timer2` | `INT32` | CELL_PRIVATE | — | `—` | — |
| `_timerTick` | `INT32` | CELL_PRIVATE | — | `—` | — |

### `ClientOnly`

- 定义：`programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/ClientOnly.def`（SHA-256 `e38e6189cd4249c36ca7bc7c4138aa496f1a8517924992d06f139c17391107a7`，706 B）
- 数据库类型 ID：`—`（无——客户端专属实体）；BigWorld ID `—`。
- Volatile：position, yaw, pitch。
- 方法：客户端 1、cell 0、base 0。

| 属性 | 类型 | 可见性 | 持久 | 默认 | 标识 |
| --- | --- | --- | --- | --- | --- |
| `name` | `STRING` | ALL_CLIENTS | — | `—` | — |
| `prop1` | `INT32` | OWN_CLIENT | — | `1` | — |
| `prop2` | `INT16` | OWN_CLIENT | — | `2` | — |
| `prop3` | `INT8` | OWN_CLIENT | — | `3` | — |

### `Guard`

- 定义：`programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/Guard.def`（SHA-256 `72963aaf90473a88c98dce3c3da4869e40bcdc400dd7eb38bd49f8aefccbb0c5`，216 B）
- 数据库类型 ID：`1`；BigWorld ID `0`。
- Volatile：position, yaw, pitch。
- 方法：客户端 0、cell 0、base 0。

### `Space`

- 定义：`programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/Space.def`（SHA-256 `8dc0224eaf00fa1126788366f6b52dde02e2bcf1f9350cdea16a47f60f52dbf5`，144 B）
- 数据库类型 ID：`4`；BigWorld ID `3`。
- Volatile：—。
- 方法：客户端 0、cell 0、base 0。

## 数据库夹具

`SHOW CREATE TABLE` 是 dbapp 从 `.def` 派生的列类型权威记录，`bigworldEntityDefsChecksum` 把表结构钉回实体定义夹具；本次捕获稳定=True（3 次连续读取一致）。实体行在登出后清空（实体表 rows 0），属正常已登出状态；书签表仍记录世界状态。

| 表 | 行 | 列 |
| --- | --- | --- |
| `bigworldEntityDefsChecksum` | 1 | `checksum` |
| `bigworldEntityTypes` | 4 | `typeID, bigworldID, name` |
| `bigworldGameTime` | 1 | `time` |
| `bigworldInfo` | 1 | `version, snapshotTime, isPasswordHashed` |
| `bigworldLogOnMapping` | 0 | `logOnName, password, entityType, entityID` |
| `bigworldLogOns` | 0 | `databaseID, typeID, objectID, ip, port, salt, shouldAutoLoad` |
| `bigworldNewID` | 1 | `id` |
| `bigworldSecondaryDatabases` | 0 | `ip, port, location` |
| `bigworldSpaceData` | 0 | `id, spaceEntryID, entryKey, data` |
| `bigworldSpaces` | 0 | `id` |
| `bigworldUsedIDs` | 0 | `id` |
| `tbl_Account` | 0 | `id, gameTime, sm_character, sm_playerName, timestamp` |
| `tbl_ClientAvatar` | 0 | `id, gameTime, sm_playerName, sm_prop2, sm_prop5_intValue, sm_prop5_stringValue, sm_spaceID…` |
| `tbl_ClientAvatar_prop5_dictValue` | 0 | `id, parentID, sm_key, sm_value` |
| `tbl_Guard` | 0 | `id, gameTime, sm_spaceID, timestamp, vm_0_direction, vm_0_position, vm_1_direction, vm_1_p…` |
| `tbl_Space` | 0 | `id, gameTime, sm_spaceID, timestamp, vm_0_direction, vm_0_position, vm_1_direction, vm_1_p…` |

书签值：

- `bigworldEntityDefsChecksum.checksum` = `89D667A1571FE40F0483C0F67133EE6D`（实体定义校验和）
- `bigworldGameTime.time` = `0`（世界时钟）
- `bigworldLogOnMapping.isPasswordHashed` = `—`（密码哈希开关）
- `bigworldLogOnMapping.version` = `—`（schema 版本）
- `bigworldNewID.id` = `4801`（实体 ID 计数器）

其余表行数：`bigworldInfo`=1、`bigworldLogOns`=0、`bigworldSecondaryDatabases`=0、`bigworldSpaceData`=0、`bigworldSpaces`=0、`bigworldUsedIDs`=0

## 可追溯性

- 每份夹具携带 `source_commit`（参考仓提交）与 `inputs`（资源路径、登录命令、数据库实例），差异工具可对每个字节指出它来自哪个版本、哪个输入。
- 资源夹具 `db.xml`（db-seed）是数据库夹具的种子输入；实体夹具 `.def` 与数据库夹具表结构通过 `bigworldEntityDefsChecksum` 互相钉住。
- 完整数据：`reference/bw00_04_03_entity_db_fixtures.json`。

生成：`entity_db_fixture_scan.py` + `entity_db_fixture_doc.py`。
