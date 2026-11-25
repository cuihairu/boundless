# BW-09-05-06 clear_auto_load + remove_db — 复核证据

- **任务编号**: BW-09-05-06（todo.md 行 1401）
- **验收**: 用隔离样本校验各自操作及退出码
- **Boundless 模块**: `crates/db/src/tools/clear_auto_load.rs`（272 行）+
  `clear_auto_load/tests.rs`（28 测）、`crates/db/src/tools/remove_db.rs`
  （89 行）+ `remove_db/tests.rs`（12 测）、`tools/mod.rs`
- **平台**: Linux x86_64

## 原版形状

| 原版位置 | 形状 |
|---|---|
| main.cpp:15-19 | `USAGE_MESSAGE` 常量（`Usage: clear_auto_load [options]\n\nOptions:\n --verbose \| -v      …\n --help \| -h         …\n`） |
| main.cpp:37-60 参数环 | **`--res`/`-r` 仅当 `i < argc-1` 才吞下一参**；末位 `--res` 落空穿透整条 else-if 链→`printf("Unrecognised option: %s\n")`（**stdout**）+ 失败；`--verbose`/`-v` 置位；`--help`/`-h`→`puts(USAGE)` + **立刻** 0；未知 token 同 printf + 1 |
| main.cpp:63-67 | `DebugFilter::shouldWriteToConsole(isVerbose)`、`BWResource`/`BWConfig::init`（环境面，未建模） |
| main.cpp:69-75 | `clearAutoLoad.init(isVerbose)` 失败→`ERROR_MSG("Failed to initialise\n")` + 1 |
| main.cpp:77-83 | `START_MSG("ClearAutoLoad")`（控制台装饰，不在日志面）；`run()` 失败→`ERROR_MSG("Failed to clear auto-load data\n")` + 1；成功 0 |
| clear_auto_load.cpp:46-61 `init` | `DatabaseToolApp::init("ClearAutoLoad","clear_auto_load",isVerbose,shouldLock=true)` 失败**静默假**（database_tool_app.cpp 的 logger 腿消息留在参照里）；`entityTypeMappings_.init` 失败→`"Failed to initialise Entity Type Mappings"`（**无尾换行**）；随后 checkTablesExist |
| clear_auto_load.cpp:72-95 `run` | 单个外层 try：事务 + `DELETE FROM bigworldSpaces` / `DELETE FROM bigworldSpaceData` / `UPDATE bigworldGameTime SET time = 0` + `deleteAutoLoadEntities()` + commit；catch→`"Failed to clear auto-load data: %s\n"` + 假；成功→`INFO_MSG("Cleared auto-load data\n")` |
| clear_auto_load.cpp:102-141 checkTablesExist | `SHOW TABLES`（try 内）逐行**向下计数** `NUM_REQUISITE_TABLES=4`（bigworldLogOns/bigworldSpaceData/bigworldSpaces/bigworldGameTime 命中即 `--left`）——**同表重复行各自递减，可掩盖缺失表**；`left>0`→`"Missing a required table in the database, please run sync_db.\n"`+假；`SHOW TABLES` 异常→`"Could not enumerate tables in database: %s\n"`+假 |
| clear_auto_load.cpp:146-185 deleteAutoLoadEntities | **自带 try**：`SELECT et.bigworldID, lo.databaseID FROM bigworldLogOns lo, bigworldEntityTypes et WHERE lo.typeID = et.typeID AND lo.shouldAutoLoad`；逐行 `entityTypeMappings_[typeID]`（**无守卫向量下标**）→`getEntityDescription(typeID).name()`→`deleteWithID(connection, databaseID)` 假→WARNING `"ClearAutoLoad::deleteAutoLoadEntities: Could not delete entity %s with DBID=%lld\n"`（**DatabaseID 是 int64、FMT_DBID "lld" 带符号十进制**）；循环尾 `connection.query("DELETE FROM bigworldLogOns")`（**非抛 `MySql::query` 变体**，失败不进任何 catch）；catch→`"ClearAutoLoad::deleteAutoLoadEnitites: An exception occurred while removing auto-loading entities from the database: %s\n"`（**"Enitites" typo 逐字**，吞掉——**事务照样提交、run 照样成功**） |
| entity_type_mappings.hpp:32 | `operator[]` = 无守卫 `container_[typeID]`；deleteWithID = entity_type_mapping.cpp:501 |
| basictypes.hpp:185/191/201 | `EntityTypeID=uint16`、`DatabaseID=int64`、`FMT_DBID "lld"` |
| remove_db:13-16 | **42 行 Python 脚本**（非 C++）：`args = sys.argv`；首个 `'--res'` 与其后一项删除（`del args[resIdx:resIdx+2]`——**切片端点钳制**，末位 `--res` 只删自己） |
| remove_db:18-21 | `filePath = args[1]`——缺位即 **IndexError**；`os.stat(filePath)` 即存在性检查 |
| remove_db:25-34 | sqlite3（pysqlite2 兜底）`connect` + `cursor.execute("SELECT * FROM tbl_checksum")`（**execute 从不 fetch**）+ `close()`——注释明言开合一遍是为**清掉残留 journal 文件** |
| remove_db:37-41 | `os.remove(filePath)`；全程**零错误处理**——任何异常即 traceback 退出码 1，正常 `sys.exit(None)`=0 |

## 端口语义

- clear_auto_load 五 trait：`ToolAppFace`（DatabaseToolApp 引导，含
  `"ClearAutoLoad|clear_auto_load|verbose|lock"` 记账）、`ClearAutoLoadDb`
  （begin/execute/show_tables/select_auto_load_entities/query/commit，Err 即
  对应 try 的异常文本）、`EntityTypeMappingsFace`（init + 查找并入的
  delete_with_id：Ok(false) 走 WARNING、Err 进内层 catch）、`EntityDefsFace`
  （entity_name）、`Faces.log`（消息面）。操作序列 `SHOW TABLES → begin →
  三语句 → SELECT → query → commit` 全落账。
- remove_db 三 seam：`RemoveDbFileSystem`（os.stat/os.remove）、
  `RemoveDbSqlite`（connect+execute+close 折叠为 clear_journal_files——
  fetch-less execute 即要点）、`StdFileSystem`（std::fs 真实现，供隔离样本
  验收）。
- Python 语义的 Rust 落形：`args.index('--res')`=`position`（首个）、
  `del args[i:i+2]`=`drain(i..min(i+2,len))`（钳制）、`args[1]` 缺位与
  `os.stat` 失败与 sqlite 失败与 `os.remove` 失败统一 `EXIT_FAILURE(1)`。

## 验收对应：用隔离样本校验各自操作及退出码

- clear_auto_load（28 测，隔离 double 面全落账）：`-h`/`--help` 用法文本逐字
  + 0 且不触 init；`-v`/`--verbose` 进 `init_tool("ClearAutoLoad",
  "clear_auto_load", true, true)`；`--res /path` 与 `-r /path` 吞参照走、
  **末位 `--res`/`-r` 报 `Unrecognised option` + 1**、`--bogus`/`-x` + 1、
  吞参后仍未知 + 1；tool app 假→`Failed to initialise\n`+1 且 mappings 未
  init；mappings 假→**无尾换行**消息 + `Failed to initialise\n`+1 且
  checkTablesExist 未跑；缺表→`Missing a required table…run sync_db.\n`+1
  且事务未开始；**重复行掩盖缺失表**（两个 bigworldSpaces + 另两表→left=0
  照走成功）；多出的表被忽略；`SHOW TABLES` 异常→`Could not enumerate…`+1；
  happy 全序（`SHOW TABLES→begin→DELETE bigworldSpaces→DELETE
  bigworldSpaceData→UPDATE bigworldGameTime SET time = 0→SELECT→query
  bigworldLogOns→commit`）+ `Cleared auto-load data\n`+0；首/三语句异常→
  `Failed to clear auto-load data: boom\n`+`Failed to clear auto-load
  data\n`+1 且后续操作全部缺席；commit 异常→deleteAutoLoadEntities 照跑
  （query 已落账）；逐行删除有序；Ok(false)→WARNING 带实体名与 **DBID 带符
  号十进制**（`DBID=-5` 钉死）且循环继续、查询照跑；deleteWithID 异常→
  **typo 消息逐字**、循环中止、**查询被跳过但 commit 照跑、退出码 0**；
  SELECT 异常→typo 消息、无删除无查询、照 commit 成功；空结果集→只 query。
- remove_db（12 测，**真临时文件隔离样本** + sqlite/记录 double）：
  happy→0 且样本文件真被 `os.remove`、sqlite 收到同一路径；文件缺失→1 且
  sqlite 未触；`--res <v> <db>` 双删后 argv[1]=db→0；**末位 `--res` 切片钳
  制只删自己**→0；单 `--res`→IndexError→1；`--res <v>` 对耗尽→1；无参→1；
  **仅首个 `--res` 被删**（第二对幸存→argv[1] 即字面 `--res`→stat 失败→1）；
  **精确匹配**（`--RES` 不算）→1；sqlite 失败→1 且**文件幸存**（次序：
  remove 在 sqlite 之后）；stat→sqlite→remove 次序落账；remove 失败→1。

## 证据

- `cargo test -p db --lib`：**408 passed / 0 failed**（368 + 40）
- `cargo test --workspace`：**4655 passed / 0 failed**（exit 0）
- `clear_auto_load.rs` 行覆盖 **100.00%**（112 DA / 0 missed）；
  `remove_db.rs` 行覆盖 **100.00%**（31 DA / 0 missed）；既有五工具覆盖保持
- 作用域内 rustfmt（仅四个叶子文件）/ clippy 全清（仅存量
  asset-pipeline/blowfish manifest 警告）

## 诚实登记

- **stdout 汇点偏差**：参照的 `puts(USAGE_MESSAGE)` 与
  `printf("Unrecognised option: …")` 打 **stdout**（不经日志通道），本移植
  一并记入日志面以便逐字断言——文本本身逐字。
- **`operator[]` 无守卫**：`entityTypeMappings_[typeID]` 是无守卫向量下标，
  非法 typeID 在参照里未定义；移植把查找并入 `delete_with_id` seam，
  越界行为不建模。`getEntityDescription` 对非法 typeID 在参照里断言
  （在内层 catch 之外），移植将 name 查找无穷化（不抛）。
- **名称查找次序**：参照每行先解析 mapping 指针与实体描述、后调
  deleteWithID；移植按「先 name、后 delete」落形（deleteWithID 的失败在
  name 之后产生 WARNING），两者可观测次序一致。
- **事务面折叠**：`MySqlTransaction` 构造/commit 建模为 db 面的
  `begin`/`commit`（rollback-on-drop 未建模——参照 run() 里 commit 前抛出
  即异常路径，析构回滚是资源面行为，不在验收面内）。
- **环境面未建模**：`DebugFilter::shouldWriteToConsole`、`BWResource`/
  `BWConfig::init`、`START_MSG` 为控制台/资源配置装饰，不在日志面。
- **remove_db 的 sqlite 未跑真库**：journal 清理语义（connect+execute 不
  fetch+close）以 double 落账；真文件腿用 std::fs 真样本验证（创建→移除、
  缺失→1、失败→文件幸存）。pysqlite2 兜底 import 是 Py2.4 时代兼容，移植
  不设兜底分支。
- **退出码简化**：Python traceback 场景真实退出码是 1（未捕获异常），移植
  以 `EXIT_FAILURE(1)` 统一；不存在其他码。
- 目录形态仍落 `crates/db/src/tools/` 命名空间，与另两席并存，合并择一。
