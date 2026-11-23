# BW-02-04-05 核查其余已启用传输方式 — 复核证据

- **任务编号**: BW-02-04-05（todo.md 行 401）
- **验收**: 每种已启用传输方式有混跑夹具或阻塞记录。
- **Boundless 模块**: `crates/mercury/src/{tcp_channel.rs,tcp_bundle_dispatch.rs,network_interface.rs}`
- **平台**: Linux x86_64

## 原版启用了哪些传输方式

原版构建（`server/baseapp/baseapp.cpp`）在 Mercury 层启用四种：

| # | 传输方式 | 启用条件 | 参考锚点 |
|---|---------|----------|----------|
| 1 | UDP 数据报 | 恒启用（内外部接口） | `network_interface.cpp` `UDPBundleProcessor` |
| 2 | TCP 裸帧 | 恒启用（客户端 `CLIENT_HEADER` 握手） | `tcp_channel.cpp:29-30,149-263` |
| 3 | WebSockets | `BWConfig::get("shouldUseWebSockets", true)`，**默认开** | `baseapp.cpp:228-231` |
| 4 | Blowfish 会话加密 | 登录时携带 `encryptionKey` 则启用，否则回落明文 | `baseapp.cpp:1751-1752`、`proxy.cpp:600-639` |

## 每种的方式

### 1. UDP 数据报 — 混跑夹具

已移植（`udp_bundle.rs`/`bundle_dispatch.rs`）。混跑见 #3。

### 2. TCP 裸帧 — 混跑夹具（本项新交付）

已移植并接线。`TCPChannel::on_data`（`tcp_channel.rs`）驱动 `handleInput` 的数据臂：
帧化（`TcpFrameParser::feed`）→ 每整帧交给 `read_header` 门或 `process_received_frame`。
`process_received_frame`（对应 `tcp_channel.cpp:360-378`）经
`NetworkInterface::interface_table()`（对应 `network_interface.hpp:148,326` 的
`pInterfaceTable_`）取到消息表，交 `tcp_bundle_dispatch::dispatch_messages`
走完整一帧。

### 3. 混跑夹具（UDP + TCP 同一张表）

`tcp_channel::tests::one_table_serves_both_a_tcp_frame_and_a_udp_bundle`：
一张 `InterfaceTable` 服务两个 id，TCP 帧经 `tcp_bundle_dispatch::dispatch_messages`
走、UDP 体经 `bundle_dispatch::dispatch_messages` 走，两次都 `REASON_SUCCESS`，
两个 recorder 各收各的消息。两条遍历线共享同一张表即证明同一套
`InterfaceTable` 同时供两种传输方式派用。

同批另有接线夹具：`a_server_channel_dispatches_a_bundle_after_the_header`
（握手头帧过门 + 其后的 bundle 帧经接口表派发到 handler）。

### 4. WebSockets — 阻塞记录（已启用，未移植）

原版默认启用：`BWConfig::get("shouldUseWebSockets", true)` 为真则
`pStreamFilterFactory_.reset(new ClientStreamFilterFactory)`（`baseapp.cpp:228-231`）。
工厂的 `createFor(channel)` 把每个进来的客户端 TCP 通道包一层
`WebSocketStreamFilter`（以工厂自身为 `WebSocketHandshakeHandler`），
并 `shouldAcceptHandshake` 调整 emscripten 子协议后返回 true
（`server/baseapp/client_stream_filter_factory.hpp:29-48`）。
过滤器自己做 HTTP 升级握手与 `Sec-WebSocket-Key` 应答，再把 WebSocket 帧
重排成 `TCPChannel` 看到的 `[len][payload]` 帧。

**阻塞原因**：需要移植整层 `StreamFilterFactory`/`WebSocketStreamFilter`
（`lib/network/websocket_stream_filter.cpp`、`websocket_handshake_handler.hpp`）：
HTTP 升级请求/应答解析、`Sec-WebSocket-Key` 的 SHA-1+GUID 应答、
emscripten 子协议协商、WebSocket 帧到裸帧的重排。属应用层握手协商，
不在 Mercury 传输帧层范围内。

**端口当前行为**：端口没有过滤器层，通道只读裸 TCP 帧。此时原版的
`handleInput` 嗅探（`tcp_channel.cpp:194-223`）就是它的原意——注释自述
"only if there is a protocol mismatch where the remote side is sending
WebSockets frames and the local side is expecting raw TCP frames"。
端口照搬该嗅探：服务器侧等头帧时先攒字节，≥4 字节后若头部为 `GET `
则 `destroy()` 断开（参考 :215-223 的 ERROR_MSG+destroy）。

夹具：`the_websockets_sniff_drops_a_get_request`、
`the_websockets_sniff_waits_then_drops_a_split_request`。

### 5. Blowfish 会话加密 — 阻塞记录（已启用，未移植）

原版启用点：登录数据携带 `encryptionKey`（`baseapp.cpp:1751-1752`）→
`completeReLogOnAttempt(...)` → `Proxy::attachToClient` 里
`SymmetricBlockCipher::create(encryptionKey_)` 后
`pChannel->setEncryption(pBlockCipher)`（`proxy.cpp:600-639`）；
密钥为空或非法则 ERROR_MSG 并回落明文连接。
`TCPChannel::setEncryption`（`tcp_channel.cpp:578-589`）把通道已有的
`pStreamFilter_` 再包一层 `EncryptionStreamFilter::create(*pStream,
pBlockCipher)`，即过滤器栈上叠一层。

**阻塞原因**：
(a) 会话密钥来自登录握手（`Proxy`/`completeReLogOnAttempt`），属
BW-02-05「包过滤和加密互通」的会话加密协商，本项不产；
(b) `EncryptionStreamFilter`/`SymmetricBlockCipher`（Blowfish 块加密）
未移植；
(c) 过滤器栈层（同 WebSockets）未移植。

**端口当前覆盖**：过滤器栈在通道上的两个接缝已照搬并测试——
`shut_down()` 的 `didFinishShuttingDown` 门（`set_filter_owes_shutdown`）
与 `flush_send_buffer()` 的排空后 deferred `doShut_down`，
正是 `EncryptionStreamFilter` 入栈后参与的同一对门
（`tcp_channel.cpp:596-620,850-854`）。密钥层到了 BW-02-05 才能叠上。

## 状态

已验收：UDP 与 TCP 各由混跑夹具覆盖（同一张 `InterfaceTable` 供两条遍历线），
WebSockets 与 Blowfish 各有阻塞记录并指名阻塞项（WebSockets 阻塞于未移植的
流过滤器层，Blowfish 阻塞于 BW-02-05 的会话加密协商与同一过滤器层）。
