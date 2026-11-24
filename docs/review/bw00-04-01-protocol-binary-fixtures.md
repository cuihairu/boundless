# BW-00-04-01 协议二进制夹具(消息 ID、方向、原始字节)

前置条件变化:此前判「依赖运行时夹具未采集:原始字节需集群运行中抓包」。BW-00-02-03
的全量单机集群(见 `bw00-02-03-live-cluster-baseline.md`)把该外部条件消解了:本清册
在集群运行中用 tcpdump 抓登录链路全量 UDP 流量,驱动源 `simple_python_client`,产出
**pcap 原始字节 + 方向清单 + 逐字段线格式解码**。夹具目录
`docs/review/data/bw00-04-01-protocol-fixtures/live-2026-11-24/`,运行 2026-11-24。

## 1. 采集方法

```
tcpdump -i lo -n -s 0 -w login-wire.pcap 'udp and (port 20013 or port 50463)'
BW_RES_PATH=<simple/res>:<game/res/bigworld>:<cluster-smoke/res> \
  simple_python_client -server 127.0.0.1 -user MyAccount -password pass
```

20013 = loginapp 外部口,50463 = baseapp 外部口(客户端本会话端口 59631)。夹具:

| 文件 | 内容 |
| --- | --- |
| `login-wire.pcap` | 49 包原始字节(snaplen 0 全长) |
| `direction-list.txt` | 逐包方向清单(带时间戳/长度;含上一死会话 47724 的残留流,已注记) |
| `login-hexdump-full.txt` | 全量十六进制 |
| `wire-decode.txt` | 逐字段解码(见下) |
| `client.log` | 客户端日志,回调序列与包序对应 |

## 2. 方向与消息 ID(实证)

| 包 | 方向 | 长度 | msgID | 名称 | 关键字段 |
| --- | --- | --- | --- | --- | --- |
| P1 | client→loginapp | 273 | `0x00` | LoginInterface::login | flags 0x0001 HAS_REQUESTS,len 260,replyID 101331,版本 0x02090000(2.9.0),LogOnParams 为 RSA 密文 |
| P2 | loginapp→client | 28 | `0xFF` | REPLY_MESSAGE_IDENTIFIER | len 21(含 replyID),replyID 101331 回显,LoginReplyRecord 密文段 |
| P3 | client→baseapp | 16 | `0x00` | BaseAppExtInterface::baseAppLogin | 定长 5B(SessionKey u32+numAttempts u8),replyID 101332,字节账 2+1+4+2+5+2=16 闭合 |
| P4 | baseapp→client | 32 | (信道流) | AoI/实体更新起点 | flags 0x04F4 = ACKS\|RELIABLE\|FRAGMENT\|SEQ\|INDEXED\|CUMULATIVE_ACK |

消息 ID 的权威分配规则(`lib/network/interface_minder.cpp:34`
`MessageID id = static_cast<MessageID>(elements_.size())`):接口内声明序、0 起。
LoginInterface = {login=0, probe=1, challengeResponse=2}
(`lib/connection/login_interface.hpp:52-60`);BaseAppExtInterface 首两个 =
{baseAppLogin=0, authenticate=1}(`lib/connection/baseapp_ext_interface.hpp:27-31`)。
线上字节实证:P1/P3 的 id 字节 `00`、P2 的 `ff`、P2↔P1 的 replyID 回显、
P3 相对 P1 请求号 +1,四点交叉一致。

## 3. 线格式结论(带源码锚点)

1. **字节序:小端(主机序)**。`lib/cstdmf/binary_stream.ipp:131-136` 的
   `#else // BIG_ENDIAN` 分支把 `BW_HTONS/BW_HTONL`(及其逆)定义为恒等宏,故本机
   x86 el7 构建的 Mercury 包头整数字段以主机字节序上线:len=260 记作 `04 01`,
   replyID=101331 记作 `d3 8b 01 00`。此前 docs 架构页登记的「协议 baseline 大端
   矛盾」由此落定:大端口径出自部分 `BW_UNPACK3`/BinaryStream 调用点,Mercury
   包头在 LE 构建上是 LE,按字段所在层判别,两者并存不矛盾。
2. 包结构 = `[flags u16][消息流][footer 反向排布]`(flags 位定义
   `lib/network/packet.hpp:62-71`;footer 打包序 `lib/network/udp_bundle.cpp:419-565`)。
3. 消息头 = `[id u8]` + 变长消息再带 `[len u16]`;请求消息头后跟
   `[replyID u32][nextRequestOffset u16]`;回复 = `[0xFF][len u32][replyID u32]`,
   len 覆盖 replyID+payload(`lib/network/interface_element.cpp:103-118,517-560`,
   `lib/network/udp_bundle_processor.cpp:400-490`)。
4. 协议版本字段逻辑值 0x02090000(=2.9.0,与 loginapp 日志一致)。
5. login 的 LogOnParams 段为 loginapp.pubkey RSA 密文(高熵);P2/P4 的会话数据为
   客户端 LogOnParams encryptionKey 加密的密文——原始字节完整保留,明文层不做断言。

## 4. 登记的限制

- 信道化流量(P4 及之后 80→144B 周期包)只留原始字节:逐消息解码需完整信道机
  (序号窗/ACK/分包重组),不在本项验收面内。
- 客户端退出后 baseapp 向死端口的残留发包(前一 21:30 会话 47724 在 21:36 仍收
  1432B 包)如实入档 `direction-list.txt`,登记为服务器发送队列残留行为。
- 上一批(21:30)抓包因 snaplen=160 截断,已废弃重抓;本批 snaplen 0 全长。
- 夹具来自 simple 示例资产的登录链路;fantasydemo 资产为占位树(见 00-02-03),
  不构成额外夹具来源。