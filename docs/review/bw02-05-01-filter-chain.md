# BW-02-05-01 实现过滤链与包校验 — 复核证据

- **任务编号**: BW-02-05-01（todo.md 行 419）
- **验收**: 过滤顺序及校验失败结果一致。
- **Boundless 模块**: `crates/mercury/src/{message_filter.rs,udp_channel.rs,udp_bundle.rs,network_interface.rs,bundle_dispatch.rs,tcp_bundle_dispatch.rs,tcp_channel.rs}`
- **平台**: Linux x86_64

## 本项交付的两条过滤链

### 1. `MessageFilter` — 消息级首过（新移植）

原版 `MessageFilter`（`lib/network/message_filter.hpp:25`）挂在 `Channel` 基类
（`pMessageFilter_`），两个 bundle 处理器都在**投递点**取它：

| 处理器 | 取 | 投递点 |
|---|---|---|
| `UDPBundleProcessor` | `udp_bundle_processor.cpp:59-60` | `:161-170` |
| `TCPBundleProcessor` | `tcp_bundle_processor.cpp:63` | `:178-185` |

两处的形状相同：有过滤器则 `filterMessage(addr, header, mis, ie.pHandler())`，
无则 `ie.pHandler()->handleMessage(addr, header, mis)`；过滤器自己拿到 handler，
**由它调用 handler 完成投递**，不调用即吞掉。

端口实现：
- 新模块 `message_filter.rs`：`trait MessageFilter::filter_message(src_addr, header, data, handler)`，
  裸 trait（原版 Mercury 层无具体子类，`encryption_filter`/`NullMessageFilter`
  之类都在应用层），`MessageFilter` 只作类型 seam。
- `UdpChannel` / `TcpChannel` 各加 `p_message_filter` 槽 + `p_message_filter()` /
  `set_p_message_filter()`（对应 `channel.hpp` 的基类存取）。
- `bundle_dispatch::dispatch_messages` 与 `tcp_bundle_dispatch::dispatch_messages`
  各加 `p_message_filter: Option<&dyn MessageFilter>` 参数（对应处理器取一次
  `pChannel->pMessageFilter()` 后在循环内使用），投递点换成 match 两臂。
- `TcpChannel::process_received_frame` 取自己的槽传进去
  （`tcp_channel.rs`，对应 `channel.pMessageFilter()`）；UDP 侧的调用方是
  `udp_bundle.rs` 的测试链，仍传 `None`（`UDPBundleProcessor` 取的是
  `pChannel->pMessageFilter()`，端口在通道上装好后同样可传）。

### 2. `PacketFilter` 的**通道级**臂 — 按包首过（已启用）

原版 `processPacket` 对**找到通道**的包另有一条臂
（`packet_receiver.cpp:471-474`）：

```cpp
if (pChannel->pFilter() && !pChannel->hasRemoteFailed())
    return pChannel->pFilter()->recv(*this, addr, p, pStatsHelper);
```

这一臂之前只在 `udp_channel.rs:243-244` 记着「filters land in BW-02-05」，
本项落地：

- `UdpChannel` 加 `p_filter` 槽（`Option<Rc<dyn PacketFilter>>`）、
  `p_filter()`、`set_p_filter(channel: &ChannelPtr, ...)`。
- `set_p_filter` 合并了原版构造函数的 critical
  （`udp_channel.cpp:128-133`，`pFilter_ && id_ != CHANNEL_ID_NULL` →
  `CRITICAL_MSG`）：端口把过滤器装到已构造的通道上，所以 indexed 通道拒绝
  装过滤器时 panic（`#[should_panic]` 测试锚定）。
- 装过滤器时把 `maxSpareSize()` 写进 `UdpBundle` 的 `extra_size`
  （对应 `UDPChannel::newBundle` 的 `pFilter_->maxSpareSize()` 实参，
  `udp_channel.cpp:1525-1531`）；`UdpBundle::set_spare_size` / `spare_size`
  新增，`spare_size()` 挂在 `UdpChannel` 上便于观测。
- `NetworkInterface::process_packet` 的找到通道分支新增该臂：先读
  `(p_filter(), has_remote_failed())`，`!failed` 时 `return filter.recv(self, addr, packet)`；
  `recv` 的默认实现仍落回 `process_filtered_packet`，故未覆盖 `recv` 的过滤器
  等价于直通。

## 过滤顺序（对照原版 `processPacket` 445-498）

```
1. 限流 / 死通道检查（BW-02-05-04，未做）
2. findChannel(addr)
   ├─ 找到：onPacketReceived 更新接收时间
   │         → 【新】通道级 PacketFilter 臂（!hasRemoteFailed 才走）
   │         → 没有臂则落 5
   ├─ 未找到且 external+dead：丢弃
   └─ 未找到：offChannel 过滤器（BW-02-04-03 已做）
3. 【新】通道级 PacketFilter 的裁决即返回值（不回退到 5）
4. offChannel 过滤器的裁决即返回值
5. processFilteredPacket：包校验（BW-02-04-02 已做）
```

臂**顺序**与**短路语义**均按原版：通道臂只在有通道且未失败时走；有通道时
offChannel 臂**不**走（原版是 `else if`，端口用 `if / else if` 保留）。

## 锚定测试（先红后绿）

写测试时先只加测试不加实现，`cargo check -p mercury --tests` 报
`error[E0432]: unresolved import 'crate::message_filter'`（4 处）与 arity
不匹配 —— 红。补齐生产代码后转绿。

新增 12 个测试（376 → 388）：

**过滤顺序与短路**
- `network_interface::a_channel_filter_decides_a_found_channel` — 有通道的包
  被通道过滤器裁决（`Reason::NoSuchPort`），包**不**进 `process_filtered_packet`
  （sink 计数 0）。
- `network_interface::a_failed_channel_skips_its_filter` — 通道 `hasRemoteFailed`
  时跳过过滤器，落到接收路径；接收路径里「远端已失败」的守护
  （`packet_receiver.cpp:700`，`REASON_GENERAL_NETWORK`）丢弃该包，
  sink 计数 0 —— 与原版行为一致。
- `network_interface::a_channel_filter_sits_before_the_off_channel_filter` —
  通道臂在 offChannel 之前。

**校验失败结果一致**
- `network_interface::a_failed_channel_skips_its_filter` 同时锚定了校验/失败
  分支的返回码与副作用（sink 计数）。

**MessageFilter 两臂**
- `bundle_dispatch::a_message_filter_takes_the_delivery` /
  `a_filter_that_does_not_forward_swallows_the_message`
- `tcp_bundle_dispatch::` 同名两测试（UDP 与 TCP 处理器形状一致）
- `tcp_channel::a_channels_message_filter_takes_the_delivery` /
  `a_channels_message_filter_can_swallow_the_delivery`（端到端：帧→通道→派发）

**通道级 PacketFilter**
- `udp_channel::an_indexed_channel_rejects_a_packet_filter`（should_panic）
- `udp_channel::a_packet_filter_installs_and_reports_its_spare_size`
- `udp_channel::a_message_filter_installs_and_reads_back`

## 有意偏离

1. `PacketFilter` 的**发包臂**（`packet_sender.cpp:245-255`）未移植：原版的
   `PacketSender::sendRescheduledPacket` 在发送前取 `pChannel->pFilter()`，
   有则交 `pFilter->send`。端口尚未实现 `PacketSender`（发包侧整体未完成），
   所以该臂无处可挂，留给 BW-02-05 的发包项。
2. `MessageFilter` 在原版是 `ReferenceCount` 派生；端口用
   `Option<Rc<dyn MessageFilter>>`，不引入引用计数层级。
3. `TCPChannel` 侧 `pMessageFilter` 与 `pFilter` 共用同一基类成员，
   端口分列于两个通道类型的独立字段（原版同一基类）。

## 门禁（本项）

| 门禁 | 结果 |
|---|---|
| `cargo test -p mercury` | **388 passed / 0 failed**（新增 12 个） |
| `cargo clippy -p mercury --all-targets -- -D warnings` | 0 finding（仅工作区既有 `python-host` 提示） |
| `rustfmt --edition 2024 --check`（本批 8 个文件） | 0 diff（幂等） |
| `cargo llvm-cov --package mercury --lib --summary-only` | crate 行覆盖 **98.29%**（≥98% 达标） |

本项新增的 12 个测试中，`NoMessageFilter::filter_message` 的空体仅被安装
未被调用（1 行未覆盖，属测试替身，非生产路径）；其余新增生产路径
（两个通道的存取器、两处派发臂、通道级 PacketFilter 臂、`maxSpareSize`
传递）均有测试触达。

## 状态

已验收：过滤顺序（通道臂 → offChannel → 校验）与校验失败结果
（通道臂裁决不回退、失败通道走接收路径的远端失败丢弃）一致；
`MessageFilter` 在 UDP 与 TCP 两条派发链上的投递点均可截获并自主转发。