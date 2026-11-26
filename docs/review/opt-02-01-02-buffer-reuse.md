# OPT-02-01-02 缓冲复用评估

对应 todo.md OPT-02-01-02：**评估缓冲复用。验收：报告分配与峰值内存。**

交付面：`crates/mercury/tests/buffer_alloc.rs`（一个集成 target，5 个测试），记录 TSV：`docs/review/fixtures/network-baseline/buffer-alloc.tsv`。

## 方法

计数全局分配器 + **指针配对台账**（thread-local `HashMap<usize, usize>` ptr→size）精确追踪活集：

- 每个 alloc 在台账登记 ptr→size，dealloc 按指针摘账——跨线程乱序释放（测试池化线程的外来分配）天然免疫，指针不配对的 free 被忽略，不会像运行和式那样下溢。
- 台账自身的生长（HashMap rehash）带重入门卫（`LEDGERING` 旗标），钩子里再分配不会递归计账；线程析构期访问用 `try_with` 守护（非 const 初始化的 TLS 有真实析构函数，钩子在 teardown 期会撞 AccessError）。
- 测量窗口 `measure(body)`：进窗前置零计数、基线取当时活集，出窗回报 `allocs/bytes/peak` 增量；LIVE 维护无条件、PEAK 只在窗内推进，所以窗外的确认释放照样落账（ack 后活集归零可断言）。
- `realloc` 保留默认 trait 实现（alloc+copy+dealloc），台账逐笔精确。

## 记录（2026-11-26 本机，Linux debug build，TSV 全文）

| 面 | 指标 | 值 | 判读 |
|---|---|---|---|
| Packet 单缓冲 | allocs / bytes | 1 / 1472 | `Packet::new()` 恰一次堆分配，满尺寸 1472B（`PACKET_MAX_SIZE`） |
| 收包循环 | allocs/报 | 1.00（256 报 256 次） | `PacketReceiver::next_packet` 经 `std::mem::take` 换新，**每报一个 1472B 新箱**；峰值每报恰 1472B |
| 可靠 RTT（32 条） | allocs/条 | 14.09（451 次 / 9840B） | 束装帧+Rc 窗槽+ack 面各记一份；峰值 9306B/条 |
| 不可靠批量（32 条） | allocs/条 | 6.03（193 次），峰值 5370B | 无确认窗，一半开销来自打包 |
| 未确认窗（256 槽） | allocs / 峰值 | 512 / 403456B | **每槽 1576B** = Packet 箱（1472）+Rc 头（~104）；ack 全部后活集归零（`window_drained_live_bytes=0`），峰值钉在全窗瞬间 |
| TCP 发送缓冲 | 稳态写 / 重排队 | 0 次 / 0 次 | **背衬复用证实**：64×64KiB 稳态写零分配；半收+整拒重排队循环第二圈零分配（容量一经拒绝生长后永久保留） |

## 复用结论

- **复用良好**：`TcpSendBuffer`（`Vec<u8>` 背衬）`drain(..n)`/`clear()` 都保容量——一次生长终身复用，稳态零分配。
- **每报翻箱**：接收路径每数据报换一个新 1472B 箱（`mem::take` 语义），发送侧 `Packet::new` 每包一次满尺寸分配。这是 UDP 面 2×1472B/报 的固定分配流——可复用池（回收箱而非丢弃）是唯一显性机会，收益 1 alloc/报 ×两路径；代价是生命周期管理（当前所有权单向传递，无悬垂风险）。
- **窗槽成本恒定**：未确认窗每槽 ~1576B，窗口大小线性外推（512 槽 ≈ 788KB），无隐藏超额分配（CircularArray 256 发不触发翻倍）。
- 记录为基线不设带（分配计数确定可复跑，TSV 供 OPT-02-01-03 批量收发改造前后对照）。

## 复跑命令（canonical）

```sh
BUFFER_ALLOC_OUT=<绝对路径> cargo test -p mercury --test buffer_alloc
```

分配计数族逐跑恒等（确定性路径）；整测套直跑不带 env 也全绿，仅夹具不落盘。

## 门禁（全绿后落账）

- `cargo test -p mercury`：377 lib + 7 network_baseline + 5 buffer_alloc，0 失败。
- `cargo clippy -p mercury --all-targets -- -D warnings`：0 错。
- `rustfmt --edition 2024 --check`：净。

## 坑集（复现与规避）

- **跨线程乱序释放毁运行和式**：池化测试线程把别处分配的内存放本线程释放，朴素活集和式下溢（subtract with overflow panic）——指针配对台账按 ptr 摘账，不配对的 free 直接忽略。
- **台账自身分配递归进钩子**：HashMap 插值触发 rehash 再进 alloc 钩子 → "thread panicked while processing panic" 死循环——重入必须在 `try_with` 闭包内 set/unset 旗标（RefMut 不能跨闭包逃逸）。
- **TLS 析构期钩子**：非 const 初始化的 thread_local 有析构函数，线程退出期分配会撞 AccessError——钩子内一律 `try_with`，失败静默放行（放行到 System）。
- **两份基线不能混**：`count.base` 在窗开时取，窗前再分配的缓冲（如 seq 向量）只进活集不进窗计数——所有窗口差值必须挂 `count.base`，另读一份 `live_bytes()` 当基线会把窗外缓冲算进差值（1024B 幻影差即此）。
- **TCP 缓冲的容量只在被拒绝时生长**：稳态直排臂（sink 全收）从不排队，`buf` 保持空——复用夹具的热身必须是完整拒绝循环（半收+WouldBlock 整块），否则窗内第一圈是首次分配、断言必炸。
- **`UdpChannel::new` 自动注册通道**：再造 `register_channel` 会 "Should not register a channel twice"；且通道地址即路由键，夹具里变量不再直读（`let _channel`），但必须活到排水结束。
