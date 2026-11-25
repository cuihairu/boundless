# OPT-02-01-04 复跑版本混部

对应 todo.md OPT-02-01-04：**复跑版本混部。验收：原版和新版 UDP/TCP 请求均通过。**

交付面：
- `crates/server-runtime/tests/el7_mixed_deploy.rs`：一个 `--ignored` 集成 target（`the_mixed_deploy_rerun_passes_all_legs`），五条腿把原版 EL7 二进制与新 Rust 织网放进同一部署，原版→新版与新版→原版的 UDP/TCP 请求全部真机走通。
- 勾账过程中发现并修复 **四个真互操作 bug**（`contracts::machined` 编解码三处 + `message_logger` 记录头一处，见下）——这是本行真正的交付价值：此前编解码只对过 Rust↔Rust 自洽，本行第一次与原版二进制逐字节对撞。
- 记录 TSV：`docs/review/fixtures/network-baseline/el7-mixed-deploy.tsv`（EL7_MIXED_OUT 落盘）。

## 部署拓扑与五条腿

回环单机，端口 20018 上**同一部署只跑一个 machined**（测试开头毒气探针：先向 20018 广播 WholeMachine 查询，有应答即 panic——跨席残留的 bwmachined2 会让腿 2/3 静默测错对象）。

1. **新版→原版 UDP**：`MachinedClient` 问原版 `bwmachined2`：pid 1 查询（running=1）+ WholeMachine 查询，原版应答。
2. **原版→新版 UDP**：原版 `message_logger` 对着新版 `MachinedDaemon` 启动：它的 watcher 注册（`addWatcher` "network/publicAddress" 等）与 birth/death 监听注册两条 ListenerMessage 被新版解析入册（registry 3 条=两假组件+logger）。
3. **发现广播**：两个假组件（2.7/2.5 两代）注册进新版 registry；原版 logger 的 ProcessStats 广播（WATCHER_NUB 类别）**逐匹配应答**找到它们，把 `logger/addTCP`（2.7→TCP）与 `logger/add`（2.5→UDP）推送件送到组件自己的套接字——原版侧版本门两臂都被踩到。
4. **新版→原版 TCP**：组件连推送件里的 TCP 地址，发注册信封（`108`）+ 4 条记录信封（`107`），每帧 `[i32 LE 长度][字节]`（= `LoggerEndpoint::sendImmediate` 线形）；原版 logger 收册组件并把记录写进 mldb `strings`。
5. **新版→原版 UDP**：2.5 组件同样信封裸数据报发到 `logger/add` 推的 UDP 地址，记录同样落 mldb。

mldb 侧断言 `component_names`/`strings` 两文件字节级包含组件名与格式串（刷新周期轮询 20s）。

## 四个真互操作 bug（全部字节级实证后修）

1. **v51 起 pid 是 u32**（`machine_guard.hpp:521` `uint32 pid_` "// uint16 in the original protocol"，`:919` PidMessage 同宽）：我们的编解码按原版 v≤50 的 u16 发，原版按 u32 读，pid 查询整包错位答 Unknown+NOT_UNDERSTOOD。`ProcessMessage`/`PidMessage`/registry 全面 u32 化。
2. **Listener/Signal 的 extra 块在字段之后**（`machine_guard.cpp:1001-1012` Listener=body+preAddr+postAddr、`:1130-1142` Signal=body+signal；信封序 `hpp:155-176` write=writeImpl+writeExtra，extra 恒殿后）：我们此前把 extra 放字段前，原版解析时 extra 串进地址/信号字段。改为 write_body+字段+write_extra / read_body+字段+read_extra，字节向量按原版排布钉死。
3. **ProcessMessage.port 线上是网络序**（`machined_utils.cpp:79` `pm.port_ = srcAddr.port`——Mercury::Address.port 本就网络序，bwmachined 原样中转；logger.cpp:1243 原样消费）：我们按主机序发，logger 找到的端口全是字节倒置像（47177→18872、39694→3739）。编解码边界 `swap_bytes`，结构体字段保持主机序。
4. **记录头 source 字段是 4 字节裸枚举**（`logger_message_forwarder.hpp:115-125` 的 `DebugMessageSource` 是普通 C++ 枚举；流家族没有它的重载，落到通用模板按 `sizeof==4` 主机序原样搬——`binary_stream.hpp:143-171`+`binary_stream.ipp:536-543`；两 priority 才是 uint8）：我们此前每字段 1 字节，38 字节负载被原版读出 41 字节需求→"too short (only 38 bytes)" 逐条丢记录。字节账：42B 记录−4B 传输消息 ID=38B 载荷；1+1+**4**+1+30+4=41>38 恰好对上。`MessageHeader::write/read` 改 u32。
   附带发现：**ProcessStats 逐匹配应答**（`bwmachined.cpp:1410-1452` `replies.append` 循环，命中几个答几个，未中答 pid-0 终止符）——原版 `answer_query` 只答首个匹配，多组件发现全靠它，新增 `answer_query_all`。

## 记录（2026-11-26 本机，TSV 一轮）

- 腿 1：pid 1 running=1、WholeMachine 有应答（原版答新版）。
- 腿 2：registry 3 条、监听 2 条（原版对新版注册成功）。
- 腿 3：addTCP/add 推送件 36B/33B，路径名与版本门两臂全对。
- 腿 4/5：注册信封 22B、TCP 记录 4 帧×45B、UDP 记录 3 帧；mldb `component_names` 含 cellapp01、`strings` 含两条格式串（120B）。
- 原版 logger stderr 仅两条已判良性：`findIndicatedInterface: No interface matching '0.0.0.0'`（monitoringInterface 未配→退化 INADDR_ANY，记录面不受影响）与 `resolveUID: Couldn't resolve UID 1000`（machined 用户名查询超时回退用 uid）。无 MF_ASSERT、无丢记录。

## 结论

1. **验收达成**：原版和新版 UDP/TCP 请求均通过——双向四组合（新版→原版 UDP/TCP、原版→新版 UDP/发现广播）全真机绿。
2. 四个互操作 bug 全部是「自洽测试测不出」的线序/宽度歧义，只能与原版二进制对撞才暴露；字节账（38 vs 41）是定位的关键工具。
3. 已知缺口：新版 `MachinedDaemon` 不做原版的 birth-notification 广播扇出（组件上下线通知其它监听者），发现面靠逐匹配查询不受影响；记为后续面不阻本行验收。
4. 端口 20018 不可配（PORT_MACHINED 常量），跨席并发跑会互踩——毒气探针是复跑的前置件。

## 复跑命令（canonical）

```sh
EL7_MIXED_OUT=<绝对路径> cargo test -p server-runtime \
    --test el7_mixed_deploy the_mixed_deploy_rerun_passes_all_legs -- --ignored --exact
```

前置：无其它进程占用 UDP 20018（测试自带毒气探针会替你查）；原版二进制在 `game/bin/server/el7/tools/`（或 BW_MACHINED_BIN/BW_MESSAGE_LOGGER_BIN）。

## 门禁（全绿后落账）

- `cargo test -p contracts`：44 绿（编解码向量含三处互操作修复的字节钉）。
- `cargo test -p server-runtime`：lib 1700 绿；`--test el7_mixed_deploy --ignored` 五腿绿（4.97s）。
- `cargo test -p message_logger`：160 绿（记录头 4 字节 source 面两向量重钉）。
- `cargo test -p platform --lib`：64 绿。
- clippy `-D warnings`、rustfmt `--edition 2024 --check`：净。

## 坑集（复现与规避）

- **枚举没有流重载就是 sizeof 长度**：C++ 枚举过 BinaryIStream 走通用模板 `extractRaw`（`sizeof(T)` 主机序原样搬）；`typedef uint8 X` 才是 1 字节。对线前先查类型是 enum 还是 typedef。
- **字节账定位法**：报"too short (only N bytes)"时把两端逐字段加宽度——38（我们有）vs 41（原版要）差 3=source 字段 1→4，一次锁定，不用二分。
- **组件套接字必须 INADDR_ANY**：logger 把推送件挂到应答的源 IP（本机=网卡地址），绑回环的套接字收不到；`bind(fd, htons(0), 0)`。
- **UDP 腿发到 add 推送件里的地址**：`logger/add`（UDP 门）推的是 logger 的 UDP 侦听地址，别拿 addTCP 的值。
- **后台跑重定向到文件不用管道**：`> run.log 2>&1`；`| tail` 会藏光全部输出且超时孤儿进程。清场用 `pkill -x bwmachined2`/`pkill -x message_logger`（`-x` 不误伤自己 shell，`-f` 会）。
- **同 backend 各 case 共用端口要毒气探针**：20018 全机唯一，跨席残留会让"原版对新版"静默变成"原版对原版"。
- **llvm-cov 增量重编译混陈旧行映射须清 target**（BW-08 老坑，本行复验覆盖率时同样适用）。
