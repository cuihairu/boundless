# BW-00-04-03 资源、实体与数据库夹具(可追溯版本与输入)

前置条件变化:原判「依赖 storage crate 与 entitydef 模块未建,随 BW-04/BW-09 立项
解锁」。两车道均已交付,且 BW-00-02-03 全量集群打通了 dbapp 的 MySQL 同步
(`db/syncTablesToDefs=true`):本清册从同一运行采集**实体表 DDL、系统表内容、
资源树哈希、种子数据原件**四件夹具,彼此以 `bigworldEntityDefsChecksum` 与
输入文件哈希互锁。夹具目录
`docs/review/data/bw00-04-03-fixtures/live-2026-11-24/`,运行 2026-11-24。

## 1. 夹具清单与追溯链

| 文件 | 内容 | 追溯 |
| --- | --- | --- |
| `entity-tables-ddl.txt` | tbl_Account/tbl_ClientAvatar(+_prop5_dictValue)/tbl_Guard/tbl_Space 的 SHOW CREATE TABLE | 由 entity defs 经 syncTablesToDefs 生成 |
| `system-tables-ddl.txt` | 11 张 bigworld* 系统表 DDL | dbapp 引导结构 |
| `system-tables-rows.txt` | 系统表实测行 | 见下 |
| `resources-sha256.txt` | simple 示例 res/scripts 全树 40 文件 sha256 | 输入侧冻结 |
| `db-seed.xml` | res/scripts/db.xml 种子原件 | XML 后备数据库输入 |

追溯锚:`bigworldEntityDefsChecksum = 89D667A1571FE40F0483C0F67133EE6D`
绑定 defs 树内容;`resources-sha256.txt` 冻结生成该 defs 的脚本与 def 文件;
参考树版本锚点见 BW-00-02-01 清册(提交/tag 与二进制哈希)。

## 2. 实体表结构要点(类型映射证据)

- **FIXED_DICT 拆列**:`prop5` → `sm_prop5_intValue int(11)` +
  `sm_prop5_stringValue varbinary(50)` + 子表
  `tbl_ClientAvatar_prop5_dictValue(parentID, sm_key, sm_value, parentIDIndex)`
  ——字典成员按类型各成一列,变长字典值落子表。
- **默认值入 DDL**:`sm_prop2 smallint(6) NOT NULL DEFAULT 2`(defs 默认 2);
  种子侧 db.xml 给 73,证明默认值来自 defs 而非种子。
- **挥发改动列**:`vm_{0,1,2}_{position,direction} float`——volatile movement
  属性按通道拆列。
- **索引**:`UNIQUE KEY playerNameIndex (sm_playerName(255))`——defs 里的
  INDEX 声明落到 MySQL 前缀唯一索引。
- **通用列**:`id bigint AUTO_INCREMENT`、`gameTime int unsigned`、
  `timestamp ... ON UPDATE current_timestamp()`;引擎 InnoDB、
  `utf8mb4/utf8mb4_general_ci`(与 bw_deep.xml `unicodeString` 配置一致)。
- blob 列(`sm_character`/`sm_playerName`)承载 pickle 序列化属性。

## 3. 系统表实测行

```
bigworldEntityTypes: (1,0,Guard) (2,1,ClientAvatar) (3,2,Account) (4,3,Space)
bigworldEntityDefsChecksum: 89D667A1571FE40F0483C0F67133EE6D
bigworldGameTime: 0
bigworldInfo: version=9, snapshotTime=NULL, isPasswordHashed=1
bigworldNewID: 9601
```

实体表本运行 0 行:登录走 `billingSystem/shouldAcceptUnknownUsers=true` 在线建实体
(未知用户→ClientAvatar),`baseApp/archivePeriod=0` 且断连即毁,未触发落盘;
`bigworldLogOns/LogOnMapping/Spaces/SpaceData/UsedIDs/SecondaryDatabases` 亦为空,
如实登记。

## 4. 种子数据(db.xml)

XML 后备数据库的种子(原件入档 `db-seed.xml`):账号 `MyAccount`(口令 `a`,
entityID 2)→ Account{character=MyCharacter, databaseID=2} →
ClientAvatar{playerName=MyCharacter, prop2=73,
prop5={intValue=122, stringValue=Blah, dictValue={coopers:carlton,
naughty:nice, happy:sad}}, position=0,0,0}。MySQL 模式下该种子不导入
(0 行佐证),作为 XML 模式的对照输入保留。

## 5. 登记的限制

- `resources-sha256.txt` 含 `__pycache__/*.pyc`(cpython-313)——运行期生成物,
  哈希如实入清单,复现时以 .py 源为准。
- fantasydemo/res 为占位树(见 00-02-03),其 108 文件清单已在 BW-00-05-04
  登记,不重复采集;本夹具的资产源是 simple 示例树。
- DB 行级夹具(实体持久化行)需 archivePeriod>0 的长会话运行,留待后续
  需要时按同法补采。