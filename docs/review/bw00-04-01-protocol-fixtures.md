# BW-00-04-01 协议二进制夹具（machined）

> 证据面（BW-00-04-01 验收：记录消息 ID、方向及原始字节）。

由 `tools/protocol_fixture_scan.py` 在参考仓自带的 `scripts/cluster-smoke/deep-chain.sh` 全栈拉起期间抓取 UDP 20018 （`lib/network/portmap.hpp` 的 `PORT_MACHINED`）流量；源数据 `reference/bw00_04_01_protocol_fixtures.json`，原始报文另存 `reference/bw00_04_01_machined.pcap.gz`。

| 项 | 值 |
| --- | --- |
| 参考仓提交 | `525449e8` |
| 捕获时间 | `2026-11-23 02:24:51 +0800` |
| 捕获模式 | `owned`（`owned`=本扫描拉起并拆除集群；`live`=集群已有、只旁听） |
| 拉起 | rc=0（47.0s），拆除 rc=0 |
| 捕获窗口 | 90s，20018 端口 386 个报文，12 个（消息 ID, 方向）键 |

## 消息清单

| 消息 | ID | 方向 | 次数 | 含义 | 原始字节样本 |
| --- | --- | --- | --- | --- | --- |
| `WHOLE_MACHINE` | 1 | in | 38 | tool/server -> machined | `0100c9bc000000000000000000` |
| `PROCESS` | 2 | in | 60 | process discovery (register / find / birth / death) | `020074d58101e803e7603400ac3f00000a63656c…` |
| `PROCESS` | 2 | out | 30 | process discovery (register / find / birth / death) | `020174d58101e803e7603400ac3f00000a63656c…` |
| `PROCESS_STATS` | 3 | in | 47 | per-process stats poll | `030072d52101e803e7603400000000000e6d6573…` |
| `PROCESS_STATS` | 3 | out | 47 | per-process stats poll | `030172d52101e80300000000000000000e6d6573…` |
| `LISTENER` | 4 | in | 44 | watch-nub listener query | `040003c58100e803ea603400960f000013426173…` |
| `LISTENER` | 4 | out | 44 | watch-nub listener query | `040103c58100e803ea603400960f000013426173…` |
| `TAGS` | 7 | in | 1 | tag set / query | `070072110114085365727669636573` |
| `TAGS` | 7 | out | 1 | tag set / query | `070172110000` |
| `QUERY_INTERFACE` | 12 | in | 31 | interface version query | `0c006ed500000000` |
| `QUERY_INTERFACE` | 12 | out | 31 | interface version query | `0c016ed5c0a805bc` |
| `MACHINED_ANNOUNCE` | 64 | in | 4 | machined to machined announce | `4000c6bc0000000000` |

## 帧格式（按观测到的线路字节）

每个 UDP 报文是一个 `MGMPacket`（`lib/network/machine_guard.cpp` `MGMPacket::read`）：

- 报文头：`u8 packet flags` + `u32 buddy`（小端）。
- 之后按 `[u16 消息长度][消息体]` 重复；消息体以 `u8 message` + `u8 flags` + `u16 seq` 开头，随后是子类字段（`process`/`listener`/`signal` 等，`lib/network/machine_guard.hpp`）。
- 方向以 20018 端口为参照：目的端口 20018 为 `in`，源端口 20018 为 `out`（`MACHINED_ANNOUNCE` 是 machined→machined 广播，也落在目的 20018，故记为 `in`）。

**线路与源不符的 delta（已登记）**：参考**二进制**把每条消息的长度字段写为**小端**（观测一致），而参考**源码** `machine_guard.cpp:114` 用 `BW_HTONS`（大端）。夹具按观测线路记录；移植侧（`crates/contracts` 的 `machined.rs`，`Header`/`ProcessMessage`）应以二进制线路为准核对，不能照抄源码的字节序。

## 可追溯性

- 消息 ID 表：`lib/network/machine_guard.hpp` `MachineGuardMessage::Message`（与 `crates/contracts/src/machined.rs` `message_id` 逐项一致：1..15 与 64）。
- 完整报文：`reference/bw00_04_01_machined.pcap.gz`。

生成：`protocol_fixture_scan.py` + `protocol_fixture_doc.py`。
