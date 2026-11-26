# BW-00-02-03 单机集群启动基线（实测全量运行）

本清册把 00-02-03 从「记录卡点」升级为「留存真实拓扑、配置和日志」：参考树单机集群
全量拉起并到达稳态，10 个进程 / 9 个应用角色全部出现 `---- <Role> is running ----`
标记。运行日期 2026-11-24，全部原始日志与派生清单见
`docs/review/data/bw00-02-03-logs/live-2026-11-24/`。

## 1. 拓扑（实测进程表）

启动顺序 = 依赖序，全部由参考树自带脚手架
`scripts/cluster-smoke/deep-chain.sh` 拉起（`setsid` 脱离，日志入
`/tmp/cluster-smoke/`）：

```
bwmachined2 -> dbappmgr/cellappmgr/baseappmgr -> dbapp -> cellapp
            -> baseapp -> baseapp(第二实例) -> loginapp -> serviceapp
```

| 角色 | PID | 稳态标记 |
| --- | --- | --- |
| bwmachined2 | 147680 | 无横幅（守护进程，日志走 syslog） |
| dbappmgr | 147744 | `DBAppMgr is running` |
| cellappmgr | 147747 | `CellAppMgr is running` |
| baseappmgr | 147751 | `BaseAppMgr is running` |
| dbapp | 147801 | `DBApp is running` |
| cellapp | 148397 | `CellApp is running` |
| baseapp | 149064 | `BaseApp is running` |
| baseapp（备份实例） | 149488 | `BaseApp is running` |
| loginapp | 149580 | `LoginApp is running` |
| serviceapp | 149736 | `ServiceApp is running` |

原始 `ps -eo pid,ppid,etimes,uid,comm` 快照：`topology.txt`；逐日志标记与发现记录：
`steady-markers.txt`。

第二 baseapp 不是冗余：`baseappmgr.cpp:1634-1654` 的 `onBaseAppDeath()` 在没有
备份实例时只报 `No backup for BaseApp` 并由 `shutDownServerOnBadState` 拆掉整个集群；
有两个 BaseApp 时 `adjustBackupLocations()` 才分配备份、由存活者接管实体。
serviceapp 同理——DBApp 与 LoginApp 在接受登录前要求 `desiredServiceApps`（默认 1）。

管理器平面互相发现（三向）：

```
Found interface BaseAppMgrInterface
Found interface CellAppMgrInterface
Found interface DBAppMgrInterface
```

## 2. 配置

资源链（`ResMgr` 逐条打印，顺序即解析顺序）：

```
<example res> = programming/bigworld/examples/client_integration/python/simple/res
<engine res>  = game/res/bigworld
<smoke res>   = scripts/cluster-smoke/res
```

- 配置文件链：`server/bw.xml` → 个性化 `server/bw_<pw_name>.xml`（Linux 上
  `BWConfig::init` 按运行用户拼文件名；脚手架把 `bw_deep.xml` 拷成
  `bw_cui.xml` 后生效）。原件入档：`config-bw_deep.xml`、`config-bw_cui.xml`。
- 示例资源树的 `server/bw.xml` 通过 `parentFile` 引用
  `server/development_defaults.xml`，该文件未随参考树发布，故个性化 overlay
  内联了三处必需配置：`loginApp/allowLogin = true`
  （`loginapp_config.cpp` 的 `BW_OPTION(allowLogin, false)` 默认为假，会拒绝一切登录）、
  `cellApp/aoiUpdateSchemes`（空节，`AoIUpdateSchemes::init()` 无空检查）、
  `personality = egclient4` + `gameUpdateHertz = 10`。
- 数据库配置：`db/type = mysql`，127.0.0.1:3306，库/用户/口令 `bigworld/bigworld`，
  `syncTablesToDefs = true`，`numConnections = 5`；MariaDB 11.8.6 由脚手架幂等
  provision（`CREATE DATABASE IF NOT EXISTS` + 用户授权）。
- 机器守护层 `/etc/bwmachined.conf`、`/etc/bigworld.conf` 本机均不存在：
  `bwmachined2 -f -p /tmp/bwmachined.pid` 以 WARN 继续（非致命，此前基线已登记）。
- 版本横幅（`BWConfig`，全 9 个应用角色）：`Version: 14.4.1. Config: hybrid`，
  UID 1000，User cui；构建时间戳即 el7 树实际产物时间。

## 3. 日志

10 份进程日志原件 + 3 份派生清单入档（`live-2026-11-24/`）：

| 文件 | 内容 |
| --- | --- |
| `bwmachined2.log` … `serviceapp.log`（10 件） | 各进程 stdout/stderr 全量 |
| `topology.txt` | 实测 PID/PPID/运行时长/UID 快照 |
| `steady-markers.txt` | 逐日志稳态标记、发现记录、版本横幅 |
| `config-bw_deep.xml` / `config-bw_cui.xml` | 生效配置原件 |
| `variant-note.txt` | 构建变体差异登记（见第 4 节） |

2026-11-22 那批「启动即败」的日志仍在
`docs/review/data/bw00-02-03-logs/`（`baseapp.log` 1318 字节即退出等），未覆盖。

## 4. 构建变体差异（如实登记）

参考树不发布许可凭据 `server/bigworld.key`，也不可伪造（本仓红线）。该凭据只被
一处消费：`server/baseapp/ping_manager.cpp` 的 `PingManager` 整块受
`#ifdef USE_OPENSSL` 门控，`baseapp.cpp:408` 在 `PingManager::init()` 失败时中止
启动；`ping_manager.cpp` 只在 `server/baseapp/Makefile.rules` 里列出，所以许可
检查只存在于 baseapp，cellapp/dbapp/loginapp/serviceapp 从不调用
`PingManager::init`。

本次运行使用参考树自带的运营者侧脚手架
`scripts/cluster-smoke/build_license_free_baseapp.sh` 产出的无许可检查 baseapp：
只把 `ping_manager.o` 以 `useOpenSSL=0` 重编（`PingManager` 的 `init()` 空实现返回
true），再用常规 OpenSSL 标志重链接 baseapp。产物核验：
`strings baseapp | grep -c bigworld.key` = 0（cellapp/dbapp/loginapp/serviceapp
同样为 0，它们本就没有该字符串）。详见 `variant-note.txt`。

顺带验证了为何**不能**用整树 `make useOpenSSL=0` 重建绕开许可检查：构建失败于
`lib/network/symmetric_block_cipher.cpp:15`
（`error: 'BlockCipherPtr' does not name a type`）。该文件带一段上游从未编译过的
`#if !defined(USE_OPENSSL)` 死分支且本身是错的：`SymmetricBlockCipher` 声明在
`namespace Mercury` 内（`symmetric_block_cipher.hpp:11-14`），而该分支在 `BW::`
作用域定义 `SymmetricBlockCipher::create`，返回值 `BlockCipherPtr`
（`network/block_cipher.hpp`，同属 `namespace Mercury`）在声明期查找中不可见。
另据脚手架头注释：baseapp 仍链接 OpenSSL（`openssl.c` 做 TLS），整二进制
`useOpenSSL=0` 在链接期也会失败。两条理由互相独立，均指向同一结论。

## 5. 登记的外部条件

- 参考树 `scripts/cluster-smoke/deep-chain.sh` 收尾阶段卡死：`timeout` 的 SIGTERM
  被 bash 延到前台子进程（`bwmachined2`）退出才处理，形成 `do_wait` 死等，脚本
  从未打出 `full app set steady` 那行。本次手动杀掉脚本壳（进程 147678）后集群
  全量存活（应用已由 setsid 脱离，PPID 为启动子 shell，杀脚本不影响应用）。
  稳态判定改用逐日志标记 + 进程存活直接核验，见 `steady-markers.txt`。
- `bwmachined2.log` 为 0 字节（守护进程记录走 syslog），进程由
  `ps`/`pgrep` 与 pidfile 机制核验。
- 应用资产为示例树 `client_integration/python/simple`（含真实 space 定义，
  cellappmgr 实测注册 `DefaultSpace`，entity=4801）；fantasydemo/res 仍是无
  space 定义的占位树（见 `bootstrap.sh` 头注释），故 00-02-03 的集群运行基于
  simple 示例资产树而非 fantasydemo。