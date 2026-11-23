# BW-00-04-01 — 采集协议二进制夹具

验收：记录消息 ID、方向及原始字节。

本任务把 Mercury 网络协议的原始线上字节固化为二进制夹具，供端口实现
（Rust `mercury` 侧）做字节级对照。字节来源是参考栈本身，不是重实现：
`docs/review/tools/capture_protocol.cpp` 用 `#define DEFINE_SERVER_HERE`
展开 `BEGIN_MERCURY_INTERFACE(FixtureInterface)`，定义四个覆盖消息头文法
全部形态的元素，然后用真实 `Mercury::NetworkInterface` 发往回环 UDP
监听套接字，再把数据报原样读回打印。

- 参考栈：BigWorld 4657a17，`programming/bigworld` 预编译静态库
  （`build/cmake-network/libnetwork.a` 等），x86-64 Linux，主机字节序 LE。
- 采集工具：`docs/review/tools/capture_protocol.cpp`，编译命令见文件头注释。
- 原始输出：`docs/review/data/bw00-04-01-logs/capture.txt`。
- 二进制夹具：`docs/review/fixtures/protocol/*.bin`，清单
  `docs/review/fixtures/protocol/MANIFEST.txt`。
- 确定性：两次运行字节完全一致（跨进程无随机性，请求/回复的 replyID
  在该无请求管理器的裸接口路径下恒定）。

## 接口

`FixtureInterface`（消息 ID 按声明顺序从 0 起）：

| 元素 | ID | 声明 | 文法形态 |
|------|----|------|----------|
| ping | 0 | `MERCURY_EMPTY_MESSAGE` | 无长度字段 |
| fixed4 | 1 | `MERCURY_FIXED_MESSAGE(fixed4,4,NULL)` | 无长度字段，定长 4 |
| var1 | 2 | `MERCURY_VARIABLE_MESSAGE(var1,1,NULL)` | 1 字节长度字段 |
| var2 | 3 | `MERCURY_VARIABLE_MESSAGE(var2,4,NULL)` | 4 字节长度字段 |

## 线上格式（经参考源码核验）

- **数据报头**：`Packet::HEADER_SIZE = sizeof(Flags)`，`Flags` 是 `uint16`，
  网络序。本组用例除 request 外均为 `00 00`（`NO_FLAGS`）。
- **消息头**：`[消息ID:1][长度字段]`。空消息无长度字段；定长消息无长度字段
  且正文长度固定；变长消息长度字段为 1 或 4 字节（`lengthParam`），正文超长
  时用特殊压缩形（见下）。
- **变长长度压缩**（`InterfaceElement::compressLength` /
  `specialCompressLength`）：当正文长度超出长度字段容量时，长度字段填
  `0xff`，随后写 4 字节长度，消息正文前 4 字节被挪到消息末尾
  （`udp_bundle.cpp` / `interface_element.cpp`）。
- **请求附加头**（`UDPBundle::startRequest`）：变长消息的请求形在长度字段
  之后是 `[replyID:int32 网络序][下一请求偏移:uint16 网络序]`，正文长度只
  计正文（不含附加头）；`ReplyID` 是 `int32`（`misc.hpp:110`），数据报
  `FLAG_HAS_REQUESTS(0x0001)` 置位，报文**页脚**追加
  `firstRequestOffset:uint16 网络序`（`udp_bundle.cpp:564`）。
- **回复形**（`UDPBundle::startReply`）：`InterfaceElement::REPLY` 是变长
  消息（ID 255，长度字段 4 字节），长度**包含**正文里的 replyID
  （`udp_bundle.cpp:329` "stream on the id (counts as part of the length)"）。

> 注：长度与正文里的标量以主机字节序写入（x86-64 为小端）；网络序字段
> （flags、replyID、偏移）用 `BW_HTONS/BW_HTONL`。夹具标定平台为
> x86-64 Linux。

## 各用例字段逐字节解析

以下「flags」「id」等均指夹具字节流内的偏移。十六进制串与 `.bin`
文件一一对应。

### notification-empty (ping, id 0) — `notification-empty.bin` (3 字节)

```
00 00 00
│  │  └─ id = 0 (ping, EMPTY_MESSAGE，无长度字段)
│  └──── flags = 0x0000 (NO_FLAGS)
└─────── 数据报头 2 字节
```

### notification-fixed (fixed4, id 1) — `notification-fixed.bin` (7 字节)

```
00 00 01 01 02 03 04
│  │  │  └─ 正文 = 01 02 03 04（定长 4，无长度字段）
│  │  └──── id = 1 (fixed4)
│  └─────── flags = 0x0000
└────────── 数据报头 2 字节
```

### notification-var1-short (var1, id 2) — `notification-var1-short.bin` (8 字节)

```
00 00 02 04 41 42 43 44
│  │  │  │  └─ 正文 = 41 42 43 44（'ABCD'）
│  │  │  └──── 长度 = 4（1 字节长度字段，正文 < 0xff）
│  │  └─────── id = 2 (var1)
│  └────────── flags = 0x0000
└───────────── 数据报头 2 字节
```

### notification-var1-long (var1, id 2) — `notification-var1-long.bin` (308 字节)

正文 300 字节 0xAA，触发特殊压缩形：

```
00 00 02 ff 2c 01 00 00 aa aa ... aa
│  │  │  │  └────── 长度 = 300（0x0000012c，小端 u32，特殊压缩形）
│  │  │  └───────── 0xff 哨兵：正文超出 1 字节长度字段容量
│  │  └──────────── id = 2 (var1)
│  └─────────────── flags = 0x0000
└────────────────── 数据报头 2 字节
```

正文 300 字节 `aa`（前 4 字节被 4 字节长度位移至消息末尾，因正文全同故
视觉上仍是 300 个 `aa`）。

### notification-var2 (var2, id 3) — `notification-var2.bin` (15 字节)

```
00 00 03 08 00 00 00 01 02 03 04 05 06 07 08
│  │  │  └──────┴── 长度 = 8（小端 u32，4 字节长度字段）
│  │  │              └─ 正文 = 01 02 03 04 05 06 07 08
│  │  └───────── id = 3 (var2)
│  └──────────── flags = 0x0000
└─────────────── 数据报头 2 字节
```

### request-var1 (var1, id 2, 请求形) — `request-var1.bin` (16 字节)

```
01 00 02 04 00 00 00 01 00 00 41 42 43 44 02 00
│  │  │  │  │        │  │  │  │  │  │  │  │  └─ 页脚 firstRequestOffset = 2 (消息起点偏移)
│  │  │  │  │        │  │  │  │  │  │  │  └──── 正文 = 41 42 43 44（'ABCD'）
│  │  │  │  │        │  │  │  │  │  └───────── 下一请求偏移 = 0（末请求，2 字节网络序）
│  │  │  │  │        │  │  │  └────────────── replyID = 1（int32 网络序，4 字节）
│  │  │  │  │        │  │  └──────────────────（replyID 高位，续上）
│  │  │  │  │        │  └───────────────────── 长度 = 4（正文 4 字节，不含附加头）
│  │  │  │  │        └──────────────────────── id = 2 (var1)
│  │  │  │  └────────────────────────────────── flags = 0x0001 (FLAG_HAS_REQUESTS)
│  │  │  └───────────────────────────────────── 数据报头 2 字节（续）
│  │  └──────────────────────────────────────── 数据报头 2 字节
│  └─────────────────────────────────────────── 数据报头 2 字节
└────────────────────────────────────────────── 数据报头 2 字节
```

字段次序：`[flags:2][id:1][长度:1][replyID:4][下一请求偏移:2][正文:4][页脚:2]`。
replyID=1：该采集路径无请求管理器（裸外部接口），请求未走
`RequestManager::addReplyOrder`，replyID 区域保持预留值，恒定=1
（两次运行一致）。

### reply-var1 (REPLY, id 255) — `reply-var1.bin` (13 字节)

```
00 00 ff 06 00 00 00 07 00 00 00 4f 4b
│  │  │  │  │  │  │  │  │  │  │  │  └─ 正文 = 4f 4b（'OK'）
│  │  │  │  │  │  │  │  │  │  │  └──── replyID = 7（小端 u32，计于长度内）
│  │  │  │  │  │  │  │  │  │  └─────── （replyID 高位）
│  │  │  │  │  │  │  │  │  └────────── 长度 = 6（replyID 4 字节 + 正文 2 字节）
│  │  │  │  │  │  │  │  └───────────── （长度高位，小端 u32）
│  │  │  │  │  │  │  └──────────────── （长度高位）
│  │  │  │  │  │  └─────────────────── （长度高位）
│  │  │  │  │  └────────────────────── id = 255 (InterfaceElement::REPLY)
│  │  │  │  └───────────────────────── flags = 0x0000
│  │  │  └──────────────────────────── 数据报头 2 字节
│  │  └─────────────────────────────── 数据报头 2 字节
│  └────────────────────────────────── 数据报头 2 字节
└───────────────────────────────────── 数据报头 2 字节
```

字段次序：`[flags:2][id:1(REPLY=255)][长度:4 小端][replyID:4 小端][正文]`。
`REPLY` 长度字段为 4 字节且长度含 replyID（replyID=7 是 `startReply((ReplyID)7)`）。

## 复用说明

Rust 端口实现应能对每个用例产出与 `.bin` 完全一致的字节；对照时以
「数据报头 2 字节 flags」为边界，消息部分（id + 长度 + 附加头 + 正文）逐
字节匹配。`MANIFEST.txt` 列出全部 7 个夹具及其字节数。
