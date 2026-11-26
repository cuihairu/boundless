# BW-15-01-03 封装进程控制及退出码

Acceptance: **三端可正常关服并回收子进程**。行保持 `- [ ]`（验收是三端；本席交 Linux 腿端到端实测——同 OPT-15-01-01 / BW-15-01-01 先例）。

## 交付面

- **原语（本 lane 既有）**：`crates/platform/src/process.rs`——`spawn`/`wait`（`waitpid` EINTR 重试）/`try_wait`（`WNOHANG`）/`signal` + `ExitStatus { code: Option<i32>, signal: Option<i32> }`（`WIFEXITED`/`WEXITSTATUS`/`WIFSIGNALED`/`WTERMSIG` 解码）；windows 臂 `9d89b22`（`signal` 为 stub）。
- **端到端**：`crates/platform/tests/process_exit_baseline.rs` 五面：
  1. **exit code 往返**：`sh -c "exit 7"` → `code==Some(7)`/`signal==None`；
  2. **零码干净退出**：`sh -c "true"` → `Some(0)`；
  3. **运行中回收**：`try_wait` 先 `None`（子还在跑）→ 轮询到 `Some`；
  4. **信号死**（unix 门控）：`sleep 30` 子进程被父 SIGTERM → `signal==Some(SIGTERM)`/`code==None`，等待在信号上返回而非子自身 sleep——关服路径；
  5. **批量回收**：16 子各带自己的退出码，按序 wait 全对上——零丢失零错配。
- 墙钟只记录不断言；断言全是数据面（code/signal 解码、None/Some 时序、16/16 匹配）。
- `PROCESS_EXIT_OUT=<path>` 落 TSV；夹具 `docs/review/fixtures/bw-15-01-03/process-exit-linux.tsv`。

## Linux x86_64 实测（2026-11-26 本机）

| 面 | 读数 | 判读 |
|---|---:|---|
| spawn | 262 µs | fork+exec 单子进程 |
| wait | 7857 µs | 首子（含 sh 启动开销） |
| reap 轮询 | 1 015 613 µs | `sleep 1` 子轮询 20ms 步进到回收 |
| 信号等待 | 247 µs | SIGTERM→waitpid 返回——关服及时性 |
| 批量回收 | 16/16 | 各码全对上 |

## 三端状态

- **Linux x86_64**：上表，实测绿。
- **macOS**：同 unix 代码臂（`waitpid`/`kill`），无 darwin runner——待 BW-15-05 矩阵跑同测试。
- **Windows**：spawn/wait/try_wait 臂已有（`9d89b22`），**`signal` 为 stub**——信号死面在 windows 编得过但杀不动，臂补齐后才有真关服面。

## 复跑

```sh
PROCESS_EXIT_OUT="$PWD/docs/review/fixtures/bw-15-01-03/process-exit-linux.tsv" \
    cargo test -p platform --test process_exit_baseline -- --nocapture
```

## 门禁

- `cargo test -p platform --test process_exit_baseline` 1 passed / 0 failed。
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` 全绿后推（见交付 commit）。

## 坑

- 耗时读数首版写成 `deadline - now()`（剩余预算）不是 elapsed——修为 `poll_start.elapsed()`，数字语义才对（首版 8 991 677µs 是预算余量，修正后 1 015 613µs 是真耗时）。
- 信号面不靠墙钟证明「死在信号上」：断言 `signal==Some(SIGTERM)`/`code==None` 是数据面，墙钟 5s 上界只防挂死。
- 批量面退出码取 `(i*15+3)%256` 避开信号字节语义（`exit` 码 0..255 全走 WIFEXITED）。
