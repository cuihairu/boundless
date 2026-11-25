# BW-09-05-03 快照工具 — 复核证据

- **任务编号**: BW-09-05-03（todo.md 行 1391）
- **验收**: 原版读者可读取结果
- **Boundless 模块**: `crates/db/src/tools/snapshot_helper.rs`（实现）、
  `crates/db/src/tools/snapshot_helper/tests.rs`（27 条单测）、`tools/mod.rs`
- **平台**: Linux x86_64

## 原版形状

| 原版位置 | 形状 |
|---|---|
| snapshot_constants.hpp | `SUCCESS = EXIT_SUCCESS(0)`、`FAILURE = EXIT_FAILURE(1)`——**transfer_db 必须认识**这两个返回值 |
| snapshot_helper.cpp:51-108 `main` | 先 `printf("SnapshotHelper started\n")`；`argc <= 1`→试 setuid 位（成功 0 失败 1）；`ConfigReader("/etc/bigworld.conf").read()` 失败→`"Unable to read config file.\n"` +1；`validate_config` 失败→1；按 `argv[1]` 分发 |
| snapshot_helper.cpp:77-88 **arity 怪癖** | `acquire-snapshot` 的 `argc != 4` **只打印** `"Invalid argument list for 'acquire-snapshot'\n"` **不返回**，随后照常调 `acquire_snapshot(config, argv[2], argv[3])`（Rust 侧以缺省空串代偿 C 的越界读）；`release-snapshot` 同形 |
| snapshot_helper.cpp:101-105 | 未知命令→`"Unknown arguments provided.\n"` +1 |
| snapshot_helper.cpp:115-138 `validate_config` | `snapshot` 节五值 `datadir/lvgroup/lvorigin/lvsnapshot/lvsizegb`，**每个缺失值各打一行** `"Unable to read config value for '%s'\n"`，任一缺失即 false |
| snapshot_helper.cpp:142-247 `acquire_snapshot` | `becomeRootUser`→读五值→`mysql_init`→`mysql_real_connect(..., "localhost", user, pass, NULL, 0, NULL, 0)`→`FLUSH TABLES WITH READ LOCK`→`execl(LVCREATE, "-L{n}G", "-s", "-n", snap, "/dev/{group}/{origin}", NULL)`→`UNLOCK TABLES`→`mysql_close`→`execl("mount", "/dev/{g}/{snap}", "/mnt/{snap}/")`→`chmod -R 755 /mnt/{snap}/{datadir}`→`printf("%s\n", snapshotFiles)` |
| snapshot_helper.cpp:195-199 **execl argv[0] 怪癖** | execl 的第一个可变参落在 `argv[0]` 槽——lvcreate 看到的 `argv[0]` 是 **`"-L5G"`**（首个选项），不是程序名 |
| snapshot_helper.cpp:240 **chmod typo** | 失败消息 `"Failed to chmod the snapshot file.s\n"`——`file.s` 逐字保留 |
| snapshot_helper.cpp:244 **读者契约** | 最终 `printf("%s\n", snapshotFiles)` 就是 transfer_db 从 stdout **末两 `\n` 之间**取的路径 |
| snapshot_helper.cpp:254-290 `release_snapshot` | `umount /mnt/{snap}/` 失败→`"Unable to unmount snapshot.\n"` **不挡**后续；`lvremove -f /dev/{g}/{snap}` 失败→`"Unable to 'lvremove' snapshot.\n"`；任一失败返回 false（3.13 迁移注记：原逗号表达式把 `"/dev/" + lvGroup` 丢掉只传 `"/" + lvSnapshot`，已修为全路径） |
| snapshot_helper.cpp:297-306 `becomeRootUser` | `setuid(0) == -1`→`"setuid root failed: %s\n"`（strerror） |

## 端口语义

- `ConfigReaderFace`（`read` + `get_value(section, key)`）承载
  bigworld.conf 面；`SnapshotSystem`（`become_root`/`mysql_init`/
  `mysql_connect`/`mysql_query`/`mysql_close`/`exec`）承载 setuid、MySQL 线缆
  与 execl 三个特权面——`Err` 文本就是参照打印的 `strerror(errno)` /
  `mysql_error()` / exec 失败标记。
- `main_face(args, config, system, log) -> i32` 是 main 的分发面，
  `SUCCESS=0`/`FAILURE=1` 与常量逐字。
- `path_between_last_two_newlines(stdout)` 把读者契约落成可测函数。

## 验收对应：原版读者可读取结果

验收测试走完整 acquire 路径，把日志按 stdout 拼接后用
`path_between_last_two_newlines` 提取——得到
`/mnt/snap0//var/lib/mysql`。**双斜杠是参照自己的**：
`"/mnt/" + lvSnapshot + "/" + dataDir` 拼上前导斜杠的 datadir 即成；
逐字保留。lvcreate 的 `argv[0] == "-L5G"` 怪癖单测钉死。

## 证据

- `cargo test -p db --lib`：**252 passed / 0 failed**
- `cargo test --workspace`：**4499 passed / 0 failed**（exit 0）
- `crates/db/src/tools/snapshot_helper.rs` 行覆盖 **100.00%**（180 DA /
  0 missed）；`dry_run.rs` 100.00%、`sync_db.rs` 99.00% 保持
- 作用域内 rustfmt / clippy 全清（仅存量 asset-pipeline/blowfish
  manifest 警告）

## 诚实登记

- **execl 语义**：参照的 `execl` 成功即**替换进程**，其后代码不再执行；
  本移植把 exec 成功建模为「命令已派发、流程继续」，因为读者契约依赖最终
  路径打印可达。exec 向量按参照原样记录（含 argv[0] 怪癖）。
- **arity 怪癖的越界读**：C 里 `argc < 4` 时 `argv[2]/argv[3]` 越界是 UB；
  Rust 侧以 `args.get(2)/get(3)` 缺省空串代偿，打印行为逐字保留。
- `TODO` 注释（dbUser/dbPass 未消毒、localhost 假设、挂载目录可能不存在、
  失败后无法回卷）属参照原文，随函数文档保留语义；未在本项实现。
- 未跑真 LVM/真 MySQL：`ConfigDouble`/`SystemDouble` 记录型 double 代偿。
- 目录形态仍落 `crates/db/src/tools/` 命名空间，与另两席并存，合并择一。