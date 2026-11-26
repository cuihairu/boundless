# OPT-02-01-03 批量收发与消除复制评估

对应 todo.md OPT-02-01-03：**评估批量收发和消除复制。验收：报告吞吐与尾延迟。**

交付面：
- `crates/platform/src/socket.rs`：`sendmmsg`/`recvmmsg` 包装（Linux 独占 cfg）+ `SendScratch`/`RecvScratch` 保留式暂存——iovec/消息数组按批量宽度建一次、逐调用重填，稳态零分配；`recvmmsg` 的 `out` 结果向量也由调用者复用。
- `crates/mercury/tests/batch_io.rs`：一个集成 target，1 个 runner 测，三臂对照（批量臂/逐报对照臂/分配计数臂）。
- 记录 TSV：`docs/review/fixtures/network-baseline/batch-io.tsv`。

## 方法

回环一对端点（同线程自发自收），3495 个 1200B 数据报（≈4 MiB）两条路各跑一遍：批量臂 `sendmmsg`/`recvmmsg`（批宽 64，保留暂存+池化接收缓冲）、对照臂 `sendto`/`recvfrom` 逐报。在途窗压在 64（内核默认 rcvbuf 按 truesize 记账约容 88 个 1200B 报，全发后全收必丢——见 OPT-02-01-01 坑集）。每次收发系统调用单独计时：吞吐=墙钟；尾延迟=每数据报摊派收发成本 p50/p90/p99/max（批量臂按本批携报数摊派，不把整批时间重复记 64 次）。分配臂在计数分配器（指针配对台账，同 buffer_alloc.rs）下跑一个热身后批周期，断言稳态零分配。

## 记录（2026-11-26 本机，Linux debug build，TSV 一轮）

| 面 | 批量臂（批宽 64） | 对照臂（逐报） | 判读 |
|---|---|---|---|
| 发送系统调用 | **55** | 3495 | **63× 摊派**，逐跑确定（3495=54×64+39） |
| 接收系统调用 | **55** | 3495 | 同上（每轮整批+EAGAIN 探测各一次） |
| 吞吐 | 78–211 MiB/s | 112–187 MiB/s | 同量级——回环吞吐被 64 深在途窗的投递延迟压住，不是系统调用瓶颈 |
| 发送每报 p50 / p99（µs） | 5.0–5.4 / 58–3530 | 5.2–5.3 / 21–28 | **中位持平**；批量 p99 更肥=单次调用整批付费 |
| 接收每报 p50 / p99（µs） | 1.2–1.3 / 2–253 | 1.2–1.3 / 2.8–3.8 | 中位持平；批量尾肥同因 |
| 稳态分配 | **0 次/报**（`batch_cycle_allocs=0`） | 现行路径 1 次/报（OPT-02-01-02 记录） | 池化缓冲+保留暂存兑现消除 |

三连跑墙钟漂 78→211 MiB/s（回环投递延迟主导，宿主负载敏感），只作量级不作带；系统调用数与分配数逐跑恒等。

## 结论

1. **系统调用摊派是真实且确定的收益**：收发各 63×。回环空闲机上吞吐不受益（瓶颈在窗深投递延迟），收益兑现在系统调用昂贵处——高负载、多租户、核间切换、真实网卡路径。
2. **每报摊派成本中位不劣化**：批量不增加每报处理成本（p50 持平），代价是尾延迟形态——一次调用整批付费，p99 肥化。批宽是尾延迟的调节旋钮。
3. **复制/分配消除面**：现行接收路径本就零用户态载荷拷贝（`recv_from_endpoint` 直接收进 Packet 缓冲、`mem::take` 换箱指针），真正的churn是每报一个新 1472B 箱（OPT-02-01-02 实测 1.00 alloc/报）；池化+保留暂存原型实测 **0 alloc/报**。落地形态=PacketReceiver 换箱改池化环+接口收包面批量化，属引擎改动，本行只交评估与原型。
4. 记录为基线不设带（墙钟机器相关；结构性断言确定）。

## 复跑命令（canonical）

```sh
BATCH_IO_OUT=<绝对路径> cargo test -p mercury --test batch_io \
    the_batch_io_evaluation_runs_all_legs -- --exact
```

TSV 追加落盘，重跑前先清文件。

## 门禁（全绿后落账）

- `cargo test -p platform --lib`：64 绿（新增 socket 面无破坏）。
- `cargo test -p mercury`：377 lib + 7 network_baseline + 5 buffer_alloc + 1 batch_io，0 失败。
- `cargo clippy -p mercury/-p platform --all-targets -- -D warnings`：0 错（workspace manifest 的 blowfish 未用警告是继承债，非本交付引入）。
- `rustfmt --edition 2024 --check`：净。

## 坑集（复现与规避）

- **libc::sendmmsg 与 recvmmsg 签名不同**：`sendmmsg(fd, vec, vlen, flags)` 四参无 timeout，`recvmmsg` 五参带 `*mut timespec`——照 sendto 四参惯性给 sendmmsg 塞 null timeout 直接 E0061。
- **`&mut [&mut [u8]]` 的 iter 不出可变借**：`.iter()` 产出 `&&mut [u8]`，`as_mut_ptr()` 报 cannot borrow as mutable——`.iter_mut()` 产出 `&mut &mut [u8]` 后自动解引用可达。
- **批量臂每报样本不能重复整批时间**：把一次 recvmmsg 的调用时间 `repeat_n` 64 次会把每报成本虚标 64×（首跑 p50 记出 325µs 的假数）；必须按本批携报数摊派（除以 k 再重复）。
- **追加式记录夹具重跑要先清文件**：`BATCH_IO_OUT` 落盘是 append 模式，改断言后重跑会得到两轮拼接的 TSV（首轮旧口径污染记录）。
- **在途窗深不能超 rcvbuf**：1200B 报 truesize 记账下默认 212992 rcvbuf 约容 88 个，窗深 128 会静默丢报（OPT-02-01-01 同坑），批宽 64 恰好贴窗。
- **闭包参数化的批句柄**：`cycle` 闭包把 bufrefs/meta/scratch 收成一个 `BatchHandles` 参数传入，既避 clippy `too_many_arguments`（9/7），又让热身与计数窗共用同一份容量状态。
