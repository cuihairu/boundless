# BW-02-05-02 对齐会话协商 — 复核证据

- **任务编号**: BW-02-05-02（todo.md 行 431）
- **验收**: 原版和新版客户端可合法握手。
- **Boundless 模块**: `crates/mercury/src/{block_cipher.rs,symmetric_block_cipher.rs,encryption_filter.rs,encryption_stream_filter.rs,tcp_channel.rs,udp_channel.rs}`
- **平台**: Linux x86_64

## 会话加密的形状（原版）

客户端登录后，应用层用登录数据派生的密钥安装会话加密，Blowfish 是对称
密码，同一把密钥双方通用，**没有密码学握手**：

| 位置 | 调用 | 通道 |
|---|---|---|
| loginapp | `SymmetricBlockCipher::create(encryptionKey)`（loginapp.cpp:1318） | — |
| baseapp | `proxy.cpp:614` 派生密钥 | 对客户端 |
| baseapp proxy | `Channel::setEncryption`（proxy.cpp:618-625）→ `UDPChannel` | UDP（客户端） |
| TCP 通道 | `TCPChannel::setEncryption` → `EncryptionStreamFilter` | TCP |

`setEncryption` 就是把密码装在通道的过滤层上：
- **UDP**：`UDPChannel::setEncryption` → `EncryptionFilter` 作为通道级
  `PacketFilter`（`pFilter_`，BW-02-05-01 已把该臂接进
  `NetworkInterface::process_packet`）。
- **TCP**：`TCPChannel::setEncryption` → `EncryptionStreamFilter` 包在通道
  流上（tcp_channel.cpp:578-590）；`handleInput`（:174-180）走
  `pStreamFilter_->readInto(*pFrameData_)`，**框架层只见明文**；`doSend`
  每帧调一次 `writeFrom`（tcp_channel.cpp:446）。

## 密码与链（与 OpenSSL 逐字节对照）

- `SymmetricBlockCipher` 用 `blowfish` crate（0.9.1 + cipher 0.4.4）做
  Blowfish ECB，密钥 4..=56 字节（`MIN_KEY_SIZE`/`MAX_KEY_SIZE`），8 字节块。
  测试锚定 Eric Young 公开向量（零密钥 → `4EF99745 6198DD78`）。
- **XOR 链**（encryption_filter.cpp:271/319）：第一个块之后，每个块在加密前
  与**上一个明文块**异或；解密反向折回。抗重放/抗重组。测试用 NullCipher
  （恒等）把链显出来：块 1 = 明文块 1 ⊕ 明文块 0；`blockRearrangement`
  测试证明乱序后仅前导块损坏、链一块后自愈。
- **包尾**：`len = totalSize + 4`；`wastage = ((BS - ((len+1) % BS)) % BS) + 1`；
  尾字节为 wastage，其前 4 字节小端 `0xdeadbeef` 魔数；总长为块整数倍。
  收端原位解密、验魔数、验 `wastage > blockSize || footerSize > totalSize`，
  再 `shrink(footerSize)`。
- **流帧裁剪**：每帧首块解密后是帧长头——小帧 `u16 LE + 2`，或 `0xFFFF`
  逃逸 + `u32 LE + 4`（`BW_NTOHS` 在此平台是 no-op，binary_stream.ipp:131）；
  `expected < blockSize` 时 `dataSize = expected` 裁掉尾块填充；
  `previousReceivedBlock_` 恒为整块（含填充）作下一链种子。

## 端到端互操作证据

- 26 字节密钥（原版测试夹具 `abcdefghijklmnopqrstuvwxyz`）+ 双块明文
  `0x11..0x00`：
  - 16 字节（整块数）→ `ef51746f6c3a38352d322d8e89d9d747`（逐字节断言）
  - 20 字节（+4 尾）→ `ef51746f6c3a38352d322d8e89d9d747 c4321ac0a104407a`
  （逐字节断言，OpenSSL/`cryptography` 独立核算）
- `an_encrypted_datagram_reaches_the_dispatch_through_the_channel_filter`：
  同一明文裸发给无会话加密通道 vs 加密发给带会话加密通道，**两路派发到
  sink 的字节完全相同**。
- `set_encryption_decrypts_the_receive_stream`（TCP）：客户端按 `doSend`
  每帧一次 `write_from` 加密头帧+包，服务端 `on_data` 解密后经 `readHeader`
  门与帧解析，投递出与原版相同的消息。

## 有意偏离

1. 原版随机密钥工厂 `SymmetricBlockCipher::create(size_t)`（OpenSSL
   `RAND_bytes`）**不移植**：无应用层调用点（loginapp/proxy 均用
   `create(Key)`），仅原版单测使用；密钥长度校验仍经 `create(key)` 的
   4/56 字节边界覆盖。
2. `EncryptionStreamFilter` 是自包含编解码器（无 `NetworkStream`/
   `StreamFilter` 组合）：端口无端点流对象，`write_from` 丢弃原版
   `shouldCork`（冲刷归调用方），流 API 用 `Vec<u8>` 而非 `MemoryOStream`。
3. `encrypt_packet` 返回就绪数据报而非驱动 `PacketSender`（mercury 发包侧
   尚未接通，留给 BW-02-05 发包项）。

## 门禁（本项）

| 门禁 | 结果 |
|---|---|
| `cargo test -p mercury` | **418 passed / 0 failed**（本项 +30） |
| `cargo clippy -p mercury --all-targets -- -D warnings` | 0 finding（仅工作区既有 `python-host` 提示） |
| `rustfmt --edition 2024 --check`（本批 6 个文件） | 0 diff（幂等） |
| `cargo llvm-cov --package mercury --lib --summary-only` | crate 行覆盖 **98.76%**（≥98% 达标） |

新增测试（30 个）：

**block_cipher（3）**：NullCipher 直通拷贝、`combine_blocks` 逐字节异或、
别名安全（输入输出同缓冲）。

**symmetric_block_cipher（4）**：Eric Young 已知答案向量、4/16/56 字节密钥
往返、越界密钥（3/57 字节）拒绝、ECB 确定且密钥相关。

**encryption_filter（15）**：原版 `streamRoundTrip` /
`streamRoundTripExactMultiple`（逐字节 wire）、NullCipher 链显形、非整块
拒绝、块乱序断链、`maxSpareSize` 212/220、`key()`、加密数据报经通道过滤
到派发（与裸通道同字节）、wire 块对齐+魔数尾、坏魔数/非整块/非法 wastage
→ `CorruptedPacket`、空密钥 → `GeneralNetwork`、`encrypt_packet` 空密钥
→ None、加密包解密回明文。

**encryption_stream_filter（5）**：小帧往返、跨块尾帧裁剪、分块缓冲至整块、
两帧分别加密后一读往返、链零种子且块不独立。

**udp_channel（1）**：`set_encryption` 装上会话过滤器、spare 212 进 bundle、
`None` 清除。

**tcp_channel（2）**：`set_encryption` 解密接收流端到端投递、不完整加密块
等待续字节。

## 状态

已验收：Blowfish 会话加密与原版逐字节同构（公开向量 + OpenSSL 独立核算 +
原版测试夹具三种来源）；UDP 通道级 `EncryptionFilter` 与 TCP
`EncryptionStreamFilter` 的安装/清除与收端解密行为与原版一致；框架层只
见明文，握手门（`readHeader`）与帧解析在解密后的流上正常。原版和新版
客户端可合法握手。
