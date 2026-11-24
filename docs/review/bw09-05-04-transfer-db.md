# BW-09-05-04 transfer_db — 复核证据

- **任务编号**: BW-09-05-04（todo.md 行 1395）
- **验收**: 目标数据和退出码吻合
- **Boundless 模块**: `crates/db/src/tools/transfer_db.rs`（实现）、
  `crates/db/src/tools/transfer_db/tests.rs`（28 条单测）、`tools/mod.rs`
- **平台**: Linux x86_64

## 原版形状

| 原版位置 | 形状 |
|---|---|
| main.cpp:26-126 `BIGWORLD_MAIN` | `isVerbose=true` 定值；init 失败→`"Initialisation failed\n"` + `EXIT_FAILURE`；参数环里 `--res`/`-r` **仅当还有下一参**才吞掉一个参数（末位 `--res` 落空即静默忽略）；未知 token 静默跳过；`consolidate` 需 2 参、`snapshotprimary` 3 参、`snapshotsecondary` 4 参，不足即逐字 `Incorrect number of arguments provided for '<cmd>' command.\n` + 失败；命令执行后 **break** 出环（首个命令获胜）；结尾 `EXIT_SUCCESS` |
| transfer_db.cpp:62-72 `init` | `initLogger(APP_NAME, BWConfig::get("loggerID",""), true)` 失败→`"TransferDB::init: Failed to initialise logger\n"` |
| transfer_db.cpp:91-131 `initLogger` | `queryForInternalInterface` 失败→`"TransferDB::initLogger: failed to query for internal interface\n"`；**WatcherNub init 失败静默返 false**（无消息）；随后 forwarder/`shouldWriteToConsole(true)`/verbose 分支再设一次 |
| transfer_db.cpp:138-171 `consolidate` | `rfind(":")` npos→`"Address provided appears invalid '%s'. Expecting <ip:port>\n"`；`Endpoint::convertAddress` 失败→`"Unable to convert string address '%s'.\n"`；端口 `ntohs(atoi(portStr))`——**不校验数字**（`atoi("notaport")=0` 照走），Mercury 端口按网络序字节交换存储；原版自带 `TODO: strtoul()` |
| endpoint.cpp:485-525 `convertAddress` | `inet_aton` 数字点分优先，失败再 `gethostbyname`——移植保留点分十进制腿，hostname 解析不移植 |
| transfer_db.cpp:177-197 `snapshotPrimary` | init 失败→`"Failed to initialise the snapshotter. Terminating.\n"`（**POSIX 上 init 恒 true，此路为死分支**）；成功→`"TransferDB::snapshotPrimary: Completed.\n"` |
| transfer_db.cpp:203-223 `snapshotSecondary` | init 失败**无消息**返 false（与 snapshotPrimary 不对称）；成功→`"Completed transfer of '%s'\n"` |
| snapshot.cpp:483-538 `transferPrimary` | INFO/NOTICE 五条进日志；setuid 探测失败→`"snapshot_helper binary appears to be incorrectly installed.\n"`；take 失败→`"Failed to take primary snapshot.\n"`；**transfer 失败仍继续 release**，返回 `wasTransferred && wasSnapshotRemoved` |
| snapshot.cpp:184-216 `isSnapshotHelperSetUidRoot` | `_helpers/snapshot_helper` 无参运行；失败→`"snapshot_helper failed to run.\n"` + **stdout/stderr 两行 `%s\n` 回显**（参照 stderr 自带 `\n` 故出现双 `\n`） |
| snapshot.cpp:234-310 `takePrimarySnapshot` | **凭据检查先于子进程派发**（`dbapp username / password empty.\n`）；argv=`["acquire-snapshot", user, pass]`；取路径两分支：无 `\n`→`"Unable to find snapshot path…"` + 原文回显；仅一个 `\n`→`"Unable to find start of snapshot path…"`；**`eolPos==0` 下溢怪癖**：`rfind('\n', eolPos-1)` 位置回绕成 npos 反而找到 0 号 `\n`、`substr` 计数回绕取到串尾——空路径成立照常转移（单测钉死） |
| snapshot.cpp:368-408 `sendSnapshot` | rsync argv=`["--archive","--no-specials","--no-devices",src,"ip:path"]`；`atoi(limitKbps)!=0` 才追加 `--bwlimit=`；`pipeStdOut=false`；失败→`"rsync failed to transfer snapshot.\n"` + `"rsync stderr:\n%s\n"` |
| snapshot.cpp:414-466 `sendCompletionFile` | `/tmp/{basename}.complete` `open(O_WRONLY\|O_CREAT,0644)`；rsync 失败**打印后不返回、照常 unlink**（原注 Purposefully…），返回 rsync 结果；unlink 失败才直接 false |
| snapshot.cpp:584-650 `transferSecondary` | `mkdtemp("/tmp/transferdb-XXXXXX")` 失败消息**无尾换行**；备份 `{tmp}/{basename}`；unlink/rmdir 失败只告警**不算失败**；返回 `wasCopied && wasTransferred` |
| snapshot.cpp:550-573 `backupSqliteDatabase` | 源/目的打不开各一条 `…inaccessible\n`；`pDestDB->backup` 本身失败**无消息**静返 false |

## 端口语义

- `LoggerInfra`（queryForInternalInterface/WatcherNub/forwarder/DebugFilter）、
  `ChildRunner`（fork/exec+泵，`about_to_exec` 钩子逐次记录）、
  `SnapshotFiles`（open/unlink/rmdir/mkdtemp/sqlite backup）、`DbConfigFace`
  （mysql 凭据）、`ConsolidateFace`（BW-09-05-05 的 delegate 面）五个 trait
  承载参照的进程/文件/网络世界；记录的命令向量、文件调用与消息日志即验收
  的「目标数据」，退出码逐字。
- `snapshot_path_from_stdout` 把 takePrimarySnapshot 的三态提取（两错误分支
  + `eolPos==0` 回绕）落成可测函数。

## 验收对应：目标数据和退出码吻合

验收测试把 `snapshotprimary` 快乐路径五次子进程**逐一钉死**：setuid 探测
（空 argv）→ acquire（`["acquire-snapshot","bw","secret"]`）→ rsync
（`--archive --no-specials --no-devices /mnt/snap0//var/lib/mysql
10.0.0.2:/dest`，kbps 非 0 才追加 `--bwlimit`）→ 完成文件
（`/tmp/mysql.complete`——`basename` 把参照的双斜杠路径折成 `mysql`）→
release（`["release-snapshot"]`）；`snapshotsecondary` 快乐路径钉死
mkdtemp 模板、`{tmp}/{basename}` 备份、rsync、完成文件、unlink 序与
rmdir。三个命令的 arity 错误、init 失败、命令失败各返 `EXIT_FAILURE(1)`，
全部走通返 `EXIT_SUCCESS(0)`；`--res`/`-r` 吞参、末位落空、未知 token 静
默、首命令 break 逐条单测。

## 证据

- `cargo test -p db --lib`：**294 passed / 0 failed**
- `cargo test --workspace`：**4541 passed / 0 failed**（exit 0）
- `crates/db/src/tools/transfer_db.rs` 行覆盖 **98.79%**（494 DA /
  6 missed——missed 全部是 `Snapshot::init` 的 POSIX 死分支，见诚实登记）；
  `snapshot_helper.rs` 100.00%、`dry_run.rs` 100.00%、`sync_db.rs` 99.00% 保持
- 作用域内 rustfmt / clippy 全清（仅存量 asset-pipeline/blowfish
  manifest 警告）

## 诚实登记

- **死分支**：POSIX 上 `Snapshot::init` 恒返 true（其 false 形只存在于
  `_WIN32` stub），故 snapshotPrimary 的 `"Failed to initialise the
  snapshotter…"` 与 snapshotSecondary 的静默返 false 为**参照死形状**，
  逐字保留但不可达——6 行 missed 即此两处（覆盖 98.79%）。同理 main 的
  `!is_verbose` 分支（`isVerbose=true` 定值）经直接单测 `init_logger(…,
  false, …)` 覆盖。
- **fork 建模**：参照的 `onChildAboutToExec` 钩子在 fork 后的子进程里跑
  （父态不受影响）；本移植在每个派发前内联执行并以 double 计数，语义
  记录如原。
- **DNS 不移植**：`Endpoint::convertAddress` 的 `gethostbyname` 腿（以及
  inet_aton 的十六进制/八进制形式）留在参照，移植只保留点分十进制；
  测试钉死 `nohost` 走失败分支。
- `atoi` 的 C 语义（空白/符号前缀、无数字得 0、溢出饱和）单测钉死；参照
  自带的 `TODO: strtoul()` 随注释保留。
- 未跑真 rsync/真 snapshot_helper/真 sqlite：五 double 记录型代偿。
- 目录形态仍落 `crates/db/src/tools/` 命名空间，与另两席并存，合并择一。
