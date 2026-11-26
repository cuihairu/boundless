# BW-15-01-01 封装事件轮询和唤醒

Acceptance: **三端可停止空闲循环**。行保持 `- [ ]`（验收是三端；本席交 Linux 腿端到端实测——同 OPT-15-01-01 先例：Linux 实测+缺腿命名卡点，不虚报三端）。

## 交付面

- **原语（本 lane 既有，本席补端到端面）**：`crates/platform/src/poller.rs`（Linux epoll / macOS kqueue，同 `create`/`ctl`/`wait` 形状；windows 臂缺）+ `crates/platform/src/wakeup.rs`（unix 自管道 / windows loopback socketpair——`WSAPoll` 只收 socket 不收匿名管道）。
- **端到端**：`crates/platform/tests/idle_loop_stop.rs`（`#![cfg(unix)]`——poller 无 windows 臂，编不过即卡点诚实化），四面：
  1. **唤醒中断**：60s 预算阻塞在 `wait`，控制线程触发 wakeup——阻塞等待被中断而非超时到期（超时返回零事件，`wakeups==1` + 退出即中断证明）；
  2. **进环前已置停**：预算 0，首次迭代即出，零唤醒；
  3. **重臂**：两轮 trigger + `consume` 排空再入等待，同一会话两次中断；
  4. **预算到期**：全程零唤醒，10ms 预算 ×5 迭代后自然停——环不经唤醒也能停。
- 延迟只记录不断言（机器相关）；断言全是数据面（wakeups 计数、exit_reason、迭代数）。
- `PLATFORM_IDLE_OUT=<path>` 落 TSV；夹具 `docs/review/fixtures/bw-15-01-01/idle-loop-stop-linux.tsv`。

## Linux x86_64 实测（2026-11-26 本机）

| 面 | 读数 | 判读 |
|---|---:|---|
| 唤醒中断延迟（首轮冷） | 680 µs | trigger→wait 返回；含控制线程调度毛刺 |
| 重臂第 1 次 | 112 µs | 热路径 |
| 重臂第 2 次 | 46 µs | 热路径稳态——亚 100µs，tick 循环无感 |
| 中断面迭代数 | 1 | 阻塞中被唤醒即出，非超时扫过 |
| 预算面迭代数 | 5 | 10ms 预算 ×5 后出 |

## 三端状态

- **Linux x86_64**：上表，实测绿。
- **macOS**：kqueue 臂同代码形状（`poller.rs` `#[cfg(target_os = "macos")]`），无 darwin runner——待 BW-15-05 矩阵点亮跑同测试。
- **Windows**：`Wakeup` socketpair 臂已有（`cc6272e`），**poller 无 windows 臂**（`WSAPoll` 后端未实现）——测试 `#![cfg(unix)]` 门控即此卡点，臂落地后同测试可跑。

## 复跑

```sh
PLATFORM_IDLE_OUT="$PWD/docs/review/fixtures/bw-15-01-01/idle-loop-stop-linux.tsv" \
    cargo test -p platform --test idle_loop_stop -- --nocapture
```

## 门禁

- `cargo test -p platform --test idle_loop_stop` 1 passed / 0 failed。
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` 全绿后推（见交付 commit）。

## 坑

- `Record::add` 键是 `&str`，动态键须 `&format!(...)`（String 不自动 deref 成 `&str` 参数）。
- 控制线程先 spin 等 `entered` 标志再睡 20ms，保证等待真正阻塞在 `epoll_wait` 里才触发——不然首轮触发落在进入等待前，中断面退化成竞速。
- 中断证明的形状：`wait` 超时返回 0 事件、唤醒返回带 fd 的事件——`wakeups==1` + `iterations==1` 即「被中断」非「预算扫完」，不靠墙钟断言。
