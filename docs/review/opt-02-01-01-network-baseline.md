# OPT-02-01-01 网络基线测量

对应 todo.md OPT-02-01-01：**测量网络基线。验收：保存 RTT、吞吐和丢包恢复记录。**

交付面：`crates/mercury/tests/network_baseline.rs`（一个集成 target，7 个测试），记录 TSV：`docs/review/fixtures/network-baseline/net-baseline.tsv`。

## 三族记录

| 族 | 腿 | 来源 | 记录指标 |
|---|---|---|---|
| RTT | UDP / TCP loopback | 真回环：一对 `mercury::endpoint::Endpoint`（UDP echo 线程、TCP listener/accept/echo 线程），逐 ping 单调钟计时 | p50/p90/p99/max/min 微秒 + 样本数 + 载荷 |
| 吞吐 | UDP / TCP loopback | 同上通道推 4 MiB（UDP 1200B 数据报、TCP 64 KiB 流块），全量回读 | 秒、MiB/s、数据报/s |
| 丢包恢复 | `UdpChannel` 重发机制 | 钉死时钟（`EventDispatcher::with_clock`，纯钟算无 socket），与 crate 单测同形 | 重发次数、恢复周期、双重扫描增发=0 |

两臂丢包恢复都确定性：

- **受控丢包**（`controlled_losses_are_recovered_exactly_once`）：16 轮，每轮一包 ack 永不来，对端 irregular（按龄重发），过 `max(2*rtt, inactivityResendDelay)`=1.0s 恰好重发一次；同刻复扫零增发；迟到的 ack 排空窗口。记录 `loss_redeliveries=16/16`、`loss_recovery_clock_us_per_round=1500000`。
- **越序 ack 穿越包**（`a_passed_ack_recovers_the_packet_it_passed`）：8 包在窗，seq 3 的 ack 丢失，seq 4 的 ack 越过它——regular 通道零龄即重发被穿过的包，无龄积累。记录 `loss_passed_ack_redeliveries=1`。

钉死钟腿的每个数字都是钟算，逐跑恒等；真回环腿的墙钟数字只记录不紧断言（唯一结构断言：全部数据报/字节原样返回、RTT 上界 500ms 防线由坏传输或被压垮的 runner）。

## 基线记录（2026-11-26 本机，Linux debug build）

```
udp_rtt_p50_us 1355.518   p90 4747.641   p99 9311.228   max 20776.326   min 1092.103  (200 样本 64B)
udp_tput 4194000 B / 0.151122 s = 26.469 MiB/s（3495 报 1200B，23127 报/s）
tcp_rtt_p50_us 1169.085   p90 5744.149   p99 8731.362   max 10183.030   min 109.925   (100 样本 64B)
tcp_tput 4194304 B / 0.039164 s = 102.136 MiB/s
loss_rounds 16 / loss_redeliveries 16 / 恢复周期 1.0s / 双重扫描增发 0 / 越序 ack 恢复 1 次
```

**机器相关**：回环墙钟数字随宿主负载漂移（同二进制三连跑 p50 在 1.1–1.4ms 间），只作量级基线不作带；丢包恢复族逐跑恒等。后续 OPT-02-01-02/03 的对比复跑取同通道记录对照即可。

## 复跑命令（canonical）

```sh
NET_BASELINE_OUT=<绝对路径> cargo test -p mercury --test network_baseline \
    the_network_baseline_runs_all_legs -- --exact
```

runner 腿串起全部五腿，记录按腿序落盘。TSV 行格式与负载 harness 一致（`metric<TAB>value`）。

## 门禁（全绿后落账）

- `cargo test -p mercury`：377 lib + 7 集成，0 失败（含本 target 3 连跑 7/7）。
- `cargo clippy -p mercury --all-targets -- -D warnings`：0 错。
- `rustfmt --edition 2024 --check`：净。

## 坑集（复现与规避）

- **cargo 以 crate 根为测试二进制 CWD**：`NET_BASELINE_OUT` 传相对路径会写进 `crates/mercury/` 下不存在的目录，六腿全部死在夹具写盘 NotFound（断言其实全过）——夹具路径必须绝对。
- **UDP 全发后全收必丢**：3495 个数据报先全发再收，客户端内核接收缓冲溢出，内核静默丢报（曾卡在 92–160/3495）。改在途窗口（≤64 未回报）推收交错后全数到达。
- **流 send 部分写**：`Endpoint::send` 对 SOCK_STREAM 可只收部分字节（65536 只收 63575/39124），`assert_eq!` 直断会炸；游标推进循环补发。
- **双向全双工死锁**：4 MiB 先发完再收会让收发两端同时堵死（echo 端 send 缓冲满→不再读→客户端 send 缓冲满），吞吐腿必须收发交错。
- **TCP 端口是线上序**：`Endpoint::connect` 收原样端口，把 `local().port` 过 `ntohs` 再喂回去每次 connect 都 ECONNREFUSED——端点层全程 wire order（crate 自有 TCP 测试同形可证）。
- **`num_outstanding_packets()` 不是未确认槽数**：它混打包束单元与发送窗占用；判"仅 seq 3 活着"要用 `is_unacked(seq)`。
