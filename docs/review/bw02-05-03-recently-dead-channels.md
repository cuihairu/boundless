# BW-02-05-03 校验认证失败时的返回状态 — 复核证据

- **任务编号**: BW-02-05-03（todo.md 行 432）
- **验收**: 无残留已建立会话。
- **Boundless 模块**: `crates/mercury/src/recently_dead_channels.rs`（新增）、`crates/mercury/src/network_interface.rs`、`crates/mercury/src/lib.rs`
- **平台**: Linux x86_64

## 原版形状

`Mercury::RecentlyDeadChannels`（lib/network/recently_dead_channels.{hpp,cpp}）是
外部接口对**刚注销通道**的短期记忆，头文件注释写明存在理由：

> 外部接口用它记住最近注销的通道一会儿；来自那些地址的包被丢弃，避免处理
> 刚断开客户端（尤其使用通道过滤器）的包——通道没了以后这些包会被**当作
> 裸包**处理，从而产生虚假的损坏告警。

| 成员 | 原版位置 | 行为 |
|---|---|---|
| `add( addr )` | recently_dead_channels.cpp:33 | **若 map 非空，先取消第一个（地址最小）条目的计时器**，再 `addOnceOffTimer(60*1000000, this, NULL)` 并把 handle 存进 map |
| `handleTimeout( handle, user )` | cpp:48 | 找到存的 handle 相等的条目并擦除 |
| `isDead( addr )` | cpp:68 | `channels_.find(addr) != end()` |
| `~RecentlyDeadChannels` | cpp:19 | 逐个 `cancel` 所有计时器 |

`Channels` 是 `BW::map<Address, TimerHandle>`（有序），故 `begin()` 是地址
最小者——端口用 `BTreeMap` 保持同序。

**接线**：`NetworkInterface::attach`（network_interface.cpp:160-170）只在
`isExternal_` 时 `create`，并 `MF_ASSERT(!pRecentlyDeadChannels_)`；
`deregisterChannel`（cpp:487-497）对外部接口**无条件** `add`（放在
`pChannelMap_->remove` 之前，返回 true）；`isDead`（cpp:525-528）透传。

## 返回状态与"无残留会话"

`PacketReceiver::processPacket`（packet_receiver.cpp:440-498）的臂序是：
限流 → `findChannel` → 找到则过滤臂 → **`else if (isExternal() && isDead(addr))`
→ 返回 `REASON_SUCCESS`** → 否则通道外过滤器 → `processFilteredPacket`。

认证失败时应用层把会话拆掉（`Channel::destroy()` → `clearState` →
`setAddress(NONE)` → `deregisterChannel`），地址由此进入
`RecentlyDeadChannels`。此后该地址的数据报：

- 不进派发、不重认证（`Reason::Success` 直接返回，客户端视角是"对端已收到并
  接受"，不会触发重传）；
- 不会被当作裸包重放（原版注释里虚假损坏告警的来源）；
- 一分钟超时后从集合移除，地址恢复可认证。

这就是本项"返回状态 + 无残留已建立会话"的落点：残留会话的**通道**在
BW-02-04 已拆，这里补的是**残留地址**不再被当作既有会话处理。

## 有意偏离

1. **超时回调不走 `this`**：原版把自己当 `TimerHandler`（
   `addOnceOffTimer(..., this, ...)`），端口改为在接口上持
   `Weak<NetworkInterface>`，回调经
   `NetworkInterface::forget_recently_dead` 把过期的 handle 交回集合。这样
   `recently_dead_channels` 模块自成一体、无环，`Drop` 与计时器生命周期不
   相互纠缠。超时语义（一分钟、到期擦除该 handle 的条目）与原版一致。
2. **保留原版 `add` 的怪癖**：新 `add` 取消的是**第一个条目**的计时器，不是
   该地址自己旧条目（若有）的计时器。后果由测试钉死：加第二个地址时，第一个
   条目的计时器被取消 → 该地址**滞留**在集合里，第二个地址正常到期。原版
   如此，端口不改。
3. `RECENTLY_DEAD_MICROS` 用 `60_000_000`（60 * 1000000，cpp:38）原样；
   一次性计时器到期自动取消，故条目到期即消失，无需手 cancel。

## 门禁（本项）

| 门禁 | 结果 |
|---|---|
| `cargo test -p mercury` | **427 passed / 0 failed**（本项 +9，上项 418） |
| `cargo clippy -p mercury --all-targets -- -D warnings` | 0 finding（仅工作区既有 `python-host` 提示） |
| `rustfmt --edition 2024`（本批 3 个文件） | 0 diff（幂等） |
| `cargo llvm-cov --package mercury --lib --summary-only` | TOTAL 行覆盖 **98.75%**（≥98% 达标） |

新增测试（9 个）：

**recently_dead_channels（5）**：加后即记、59 999 999 µs 不失效 / 60 000 001 µs
失效；重复 `add` 续期（到期顺延到 90 s）；新 `add` 取消第一个条目计时器
（怪癖锚定）；`drop` 取消所有计时器；死地址数据报以 `Reason::Success` 丢弃
且不进 sink。

**network_interface（4）**：注销的外部通道地址立即变 recently-dead；内部
接口不留 recently-dead（`is_dead` 恒 false）；**验收测试**——`UdpChannel::destroy`
走完整拆除链后，同地址数据报返回 `Reason::Success` 且 sink 为空（无残留会话、
无重认证）；一分钟护栏到期后恢复处理（走连接过滤器 → `Reason::GeneralNetwork`）。

## 状态

已验收：认证失败拆除会话后，该地址进入最近失效集合并被接收路径静默丢弃
（`Reason::Success`），不进派发、不重认证、不产生虚假损坏告警；集合只存在于
外部接口，到期或接口析构即清。返回状态与"无残留已建立会话"与原版一致。