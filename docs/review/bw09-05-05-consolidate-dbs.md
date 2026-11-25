# BW-09-05-05 consolidate_dbs — 复核证据

- **任务编号**: BW-09-05-05（todo.md 行 1396）
- **验收**: 原版输入合并结果吻合
- **Boundless 模块**: `crates/db/src/tools/consolidate_dbs.rs`（实现）、
  `crates/db/src/tools/consolidate_dbs/tests.rs`（74 条单测）、`tools/mod.rs`
- **平台**: Linux x86_64

## 原版形状

| 原版位置 | 形状 |
|---|---|
| main.cpp:28-125 `BIGWORLD_MAIN` | `DebugFilter` WARNING 阈值 → `BWResource::init`/`BWConfig::init` → **`BWConfig::isBad()` 真即 `return EXIT_SUCCESS`（定值 quirk）**；解析失败→`"Failed to parse command line options.\n"` + 失败；`ConsolidateDBsApp app( !options.shouldIgnoreSqliteErrors() )`；`shouldReportToDBApp = hadNonOptionArgs()`；`shouldLock = shouldClear()`（带「scalable DB」TODO 注释）；init 失败静默返失败；`--clear` 成功→0/失败→`"Failed to clear secondary databases\n"`+1；`--list` 同形（`"Failed to list secondary databases\n"`）；defs 不符→1；**有位置参→只合并**（snapshot_helper 已转移完毕），无位置参→`transferAndConsolidate()` |
| command_line_parser.cpp:21-34 | `USAGE_MESSAGE` 常量（匿名命名空间），`printUsage()`（:263）用 **`puts` 打 stdout**（不经日志） |
| command_line_parser.cpp:38-50 构造函数 | `primaryDatabaseInfo_ = DBConfig::connectionInfo()`——无位置参时取 bw.xml |
| command_line_parser.cpp:58-170 `init` | `s_shortOptions = ":r:vh"`（**清/列/忽略无短形**）；`getopt_long` 环：`case ':'`→`"Missing argument for option -%c"`（**无尾换行**）；`case '?'`→`"Unknown option -%c"`（**无尾换行**，未知长选项 optopt=0 故带字面 NUL）；`'r'` 吃掉簇余或下一参并终止簇；长选项取参缺失同样走 `':'`；`'h'` 打印用法并失败；位置参规则：**0→真**、**1→`"Primary database specified, but no path to secondary database list was specified\n"`+用法+假**、**>2→`"Extraneous arguments after secondary database list path ignored"`（WARNING，**无尾换行**）**；`--clear`/`--list` 与位置参同现→`"consolidate_dbs: The --<clear|list> option does not take additional arguments\n"`（clear 优先）且**不解析**、用 bw.xml；GNU 置乱（`--` 终止选项解析） |
| command_line_parser.cpp:174-220 `parsePrimaryDatabaseInfo` | `split(';')` 非 5 段→`"Primary database argument must be in the form <host>;<port>;<username>;<password>;<database>\n"`；端口 `strtoul(text,&endptr,0)`，`endptr!=start`→`"Could not parse database port :%s\n"`；`port>=1<<16`→`"Primary database port is out-of-range: %s\n"` |
| command_line_parser.cpp:233-261 `readSecondaryDatabaseList` | 打不开→`"Could not open secondary database list at \"%s\"\n"`+假；空行跳过 |
| consolidate_dbs_app.cpp:84-139 `init` | `DatabaseToolApp::init("ConsolidateDBs","consolidate_dbs",isVerbose,shouldLock,info)` 失败**静默假**；`enableSignalHandler(SIGINT/SIGHUP)`；`localIP = watcherNub().udpSocket().getLocalAddress().ip`；`shouldReportToDBApp` 而 DBApp 缺/`init` 失败→**静默假**（`"No DBApp found!"` 只在注释里）；随后 TRACE `"ConsolidateDBsApp: Connected to primary database: host=%s:%d, username=%s, database=%s\n"`；`BWResource::resolveToAbsolutePath` 非目录→`"ConsolidateDBsApp::init: Configuration setting <%s> specifies a non-existent directory: %s\n"` |
| consolidate_dbs_app.cpp:141-191 `transferAndConsolidate` | `getSecondaryDBInfos` 失败即假；空集→`"…No secondary databases to consolidate\n"`+假；转移失败→`"…Failed to transfer secondaryDBs\n"`+假；合并失败→`"…Failed to consolidate secondaryDBs\n"`+假；成功→`cleanUpRemoteFiles` + `clearSecondaryDBEntries` + `"…Completed successfully\n"` |
| consolidate_dbs_app.cpp:194-245 `transferSecondaryDBs` | `connectionsListener.init(0,internalIP,count)`；`getBoundAddr` 失败**静默假**；成功→INFO `"…Listening on: %s\n"`；逐个 `transferDB.transfer( htonl(ip), location )` 失败→置 `shouldAbort_` + 假；`processUntilBreak()` + `fileReceiverMgr.finished()` |
| consolidate_dbs_app.cpp:247-295 `consolidateSecondaryDBs` | INFO `"…Number of connections = %d.\n"`；`primaryDBQueue.init` 失败→ERROR `"…Failed to initialize primaryDBQueue\n"`+假；**一次** `ConsolidationProgressReporter(*this, filePaths.size())`；循环 `consolidateSecondaryDB` 首个失败即**立刻**按 `shouldAbort_` 选 `"…Data consolidation was aborted\n"` 或 `"…Some entities were not consolidated. Data consolidation must be re-run after errors have been corrected.\n"`（均 WARNING）+ 假 |
| consolidate_dbs_app.cpp:296-330 `getSecondaryDBInfos` | `SELECT ip, location FROM bigworldSecondaryDatabases`；**`htonl(ip)`**（:312）；异常→ERROR `"…Failed to get secondary DB information from primary database: %s\n"` |
| consolidate_dbs_app.cpp:332-375 `consolidateSecondaryDB` | `secondaryDB.init(path)` 失败**静默假**；INFO `"…Consolidating '%s'\n"`；`getChecksumDigest()` 失败**静默假**；摘要不符→`"…%s failed entity digest check\n"`；`shouldStopOnError_ = !ignoreSqliteErrors_`；合并失败→`pErrorMgr_->addSecondaryDB(path)` |
| consolidate_dbs_app.cpp:377-394 `checkEntityDefsDigestMatch` | `Digest::unquote` 失败→`"ConsolidateDBsApp::checkEntityDefsDigestMatch: Not a valid MD5 digest\n"` |
| consolidate_dbs_app.cpp:408-424 `onSignalled` | SIGINT/SIGHUP→WARNING `"…aborting consolidation\n"` + `abort()`（`dispatcher().breakProcessing()`） |
| consolidate_dbs_app.cpp:431-490 defs 校验 | 内层失败→`"ConsolidateDBsApp::init: Our entity definitions do not match the ones used by the primary database\nDatabase consolidation should be run before making changes to entity definitions. Changing entity definitions potentially invalidates unconsolidated data.\nRun \"consolidate_dbs --clear\" to allow the server to run without doing data consolidation. Unconsolidated data will be lost.\n"`；`SELECT checksum FROM bigworldEntityDefsChecksum` 空集→`"ConsolidateDBsApp::checkEntityDefsMatch: Checksum table is empty\n"`（消息沿用**非 Internal** 拼法）；异常→`"…Failed to retrieve the primary database entity definition checksum: %s\n"` |
| consolidate_dbs_app.cpp:503-537 `clearSecondaryDBEntries` | `DELETE FROM bigworldSecondaryDatabases` 事务；成功→`"…Cleared %d entries from %s:%d (%s)\n"`+回填 `pNumEntriesCleared`；异常→**无尾换行** `"…clearSecondaryDBEntries: %s"`（catch 内） |
| consolidate_dbs_app.cpp:541-568 `printDatabases` | `"\n"` + `"Secondary databases to consolidate = %zu\n"` + `"%15s %s\n"`（`Address(hostIP,0).ipAsString()`，:561） |
| basictypes.cpp:119-131 `ipAsString` | **先 `hip = ntohl(ip)`** 再取四位大端八位组——与 `getSecondaryDBInfos` 的 `htonl` 相消，打印的是 DB 原值八位组 |
| consolidation_progress_reporter.cpp | `reportProgressNow`→`"Consolidating %s (%d/%d entities) (%d/%d databases)"`；`onStartConsolidateDB` 置库名/实体数、`doneDbs++` 并**立即**上报；`onConsolidatedRow` 计数 + sluggish 上报 |
| file_transfer_progress_reporter.cpp | 构造即上报；`startedFiles<numFiles`→`"Data consolidation is waiting for  all secondary database file transfers to start (%d/%d started)"`（**双空格**）；否则→`"Transfering secondary databases for consolidation (%d/%d bytes) (%d/%d files completed)"`（**Transfering 拼写照留**）；`onFinishTransfer` 只计数不上报 |
| sluggish_progress_reporter.hpp:55 | 报告间隔 = 0.5 × `stampsPerSecondD()`，`now - last > interval` 才上报并推进 last |
| md5.cpp:484-497 / 499-527 | `quote` 用 `"0123456789ABCDEF"`（**大写**）；`unquote` 仅查 32 长，逐 nibble `c > '9' ? c - ('A'-10) : c - '0'` **无十六进制校验**（小写字母歪打正着、垃圾字符出垃圾字节） |

## 端口语义

- `ConsolidateConfig`（bwConfig 坏/默认连接信息/连接数/合并目录及其 XML 路径/解析为目录）、
  `ToolAppFace`（`initTool`/信号/本机 IP）、`PrimaryDatabase`（三查询）、`EntityDefs`
  （持久属性摘要）、`DbAppFace`（状态通道）、`StatusReporter`、`ClockFace`、
  `TransferLeg`（监听/发起转移/等待/已收路径/清理）、`MergeLeg`（队列/二级库 init/
  摘要/合并）、`ListFileReader` 九个 trait 承载参照的 MySQL/网络/文件系统世界。
  记录的命令序列、查询结果、合并参数与消息日志即验收的「原版输入合并结果」，
  退出码逐字。
- `strtoul_base0` 把 C `strtoul(…,0)` 的 base-0 语义（空串 `endptr==start`、
  前导零含自身按八进制、`0x` 十六进制、负号回绕、尾随垃圾留在 endptr 之前）
  落成可测函数；`ip_as_string` 保留参照的内层 `ntohl`，故与 `htonl` 相消。

## 验收对应：原版输入合并结果吻合

同形输入逐一钉死：getopt 簇（`-vrX` 附参、`--res=X`、`--res`/`-r` 吞参、
`-c/-l/-i` 无短形→未知短选项、`-z`/`--bogus` 的 optopt、未知长选项的字面
NUL、`--verbose=1` 的 optopt、`--` 终止、选项后置乱序）→ 同一份日志序列；
五段 spec 的 base-0 端口簇（空段=0、`-1` 回绕越界、`0x1F90`/`010`/`65535`、
尾随垃圾、65536 越界）→ 同一份错误串与退出码；转移路径六条消息逐字（含
`Listening on`、`Number of connections`、`Cleared 3 entries from primary:3306
(bigworld)`、`Completed successfully`）且 `listen(0x0A000009,1)`/
`request(htonl(0x7F000001),…)`/`consolidate(path,false,false)` 全部落账；
合并路径的失败顺序（首个失败即停、`/b.db` 不再触碰）、abort 两种 WARNING、
摘要三分支、`clear` 回填与无换行 catch、`printDatabases` 的 `%15s` 右对齐、
SIGINT/SIGHUP 中止、四条 `main` 分支退出码（0/1）——**74 条单测，368/368 绿**。

## 证据

- `cargo test -p db --lib`：**368 passed / 0 failed**
- `cargo test --workspace`：**4615 passed / 0 failed**（exit 0）
- `crates/db/src/tools/consolidate_dbs.rs` 行覆盖 **99.86%**（724 DA /
  1 missed——missed 即长选项分支的 `_ => unreachable!()` 穷举兜底臂，六项长选项
  各走各的手柄，结构性死分支，见诚实登记）；`snapshot_helper.rs`/`dry_run.rs`/
  `sync_db.rs`/`transfer_db.rs` 覆盖保持
- 作用域内 rustfmt / clippy 全清（仅存量 asset-pipeline/blowfish
  manifest 警告）

## 诚实登记

- **printUsage 的汇点**：参照的 `printUsage()` 用 `puts` 打 **stdout**（不经日志
  通道），本移植把该用法文本记入日志面以便逐字断言——**汇点偏差**，文本本身
  逐字（`"\nUsage: consolidate_dbs [options]\n       consolidate_dbs [options]
  <primarydb_spec> <secondarydb_list> (when invoked from DBApp)\n\nOptions:\n …"`）。
  同理 getopt 自带的 stderr 诊断（长选项误带值、无参长选项误带值）参照里就
  绕过日志，本移植以 `optopt` 映射后的那条消息代偿。
- **未知长选项的 optopt**：glibc 报 `'?'` 且 `optopt = 0`，故参照
  `"Unknown option -%c"` 带**字面 NUL**——移植保留（单测钉死）。
- **前缀缩写未移植**：glibc 的无歧义长选项前缀匹配（`--verb`→verbose）未移植，
  移植按精确长选项名匹配。
- **abort 的 dispatcher**：参照 `abort()` 除翻 `shouldAbort_` 外还调
  `dispatcher().breakProcessing()`（传输层），本移植保留标志位（即消息所观测者），
  不建模事件循环中断。
- **控制台启动/关闭装饰未移植**：`main` 的 `START_MSG` 与
  `ShutdownLogSuppressor` 属控制台装饰，不在日志面内。
- **真实引擎未跑**：未连真 MySQL、未开真 TCP 监听、未跑真 sqlite 合并——
  九 double 记录型代偿（`TransferLeg`/`MergeLeg`/`PrimaryDatabase` 等）。
- 目录形态仍落 `crates/db/src/tools/` 命名空间，与另两席并存，合并择一。
