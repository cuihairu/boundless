# OPT-06-01-03 优化下载 — 服务器侧排出面（平行交付）

Acceptance: **比较传输时长及内存**。本行已由 main 收编的客户端组装面勾账
（bw15-platform `6ffd9af` pre-sized consume-as-you-go assembly + `bdf1327`
bench + `c23987a` 证据，peer 面为 `data_download.rs` 客户端收集器：`Vec::concat`
双倍峰值 → 预 sizing 消费式组装，峰值 2×→1×）。本页是同一验收的**服务器侧补充面**，
lane-only 平行交付，不重复勾行：`server_download.rs` 的 per-download staging
消除，附带被测量否决的保留前提。

## 交付面

- `crates/server-runtime/src/server_download.rs` — `DataDownload::add_to_bundle`
  改为 `add_to_bundle_into(&mut Vec<ServerChunk>, &mut i32)`：分块直接写进
  `ServerDownloads::add_to_bundle` 的结果向量，不再为每个下载每次调用新建一个
  staging `Vec`。一次 stampede 排出（n 个下载）省 n 次分配 + n 次拷贝
  （`chunks.extend(per_download_vec)` 的 extend 复制消失）。队列循环、预算门
  （`remaining > 0`，耗尽后后面的下载头不被消费）、失败臂、完成臂、id 回收
  语义逐条保持原状（`data_downloads.cpp:105/112/117/191`）。
- `crates/server-runtime/benches/download_drain.rs` + `[[bench]]` — criterion
  基准：stampede 排出（64/2048 个一 fragment 下载，预算恰好 24B/个），双形状
  同二进制交错对比。
- `crates/server-runtime/benches/legacy_queue.rs` — `d741450` 队列机制的逐行
  孪生（per-download staging `Vec`），被 bench 与 alloc 测试 `#[path]` 共享。
  与新形状的输出逐 chunk `PartialEq` 全等断言（`the_legacy_twin_matches_the_into_chunks_shape`），
  保证 A/B 比较的是同一份工作。
- `crates/server-runtime/tests/download_drain_alloc.rs` — 计数分配器（沿用
  `tests/alloc_counts.rs` 契约：thread-local 追踪、给定工具链下逐字节可复现，
  双跑断言）扩 live/peak：两形状在同一进程同一夹具下测分配次数/字节/峰值驻留。
- 新测 ×2（`server_download.rs`）：`a_stampede_finishes_in_queue_order_and_reclaims_the_lowest_id`
  钉 64 下载单调用全完成、报告序=队序、id 回收 0；`an_exhausted_budget_leaves_later_headers_unsent`
  钉预算中途耗尽时后续下载的 header 不被消费、下一包从其 header 起。

## A/B 对照（2026-11-26 本机，taskset -c 6 钉核，3 轮独立进程；r3 受舰队并发构建污染单独注记）

### 传输时长（criterion median µs，into_chunks / per_download_vec）

| 面 | r1 | r2 | r3 |
|---|---:|---:|---:|
| `drain_256` | 62.211 / 87.911（**0.708**） | 59.968 / 83.313（**0.720**） | 64.548 / 93.829（**0.688**） |
| `drain_2048` | 566.12 / 636.87（**0.889**） | 557.16 / 648.32（**0.859**） | 785.97 / 722.54（1.088，污染轮） |

r1–r2 每轮候选全优、效应远超轮内 CI（±5% 上下界无交叉）；r3 机器上同时有
7–55 个他席 cargo/rustc 进程（load avg 16.9→38.7），2048 臂漂移，不采信。
效应量级与机制吻合：省的是每下载一次 ~50ns 级分配+拷贝，256 臂（短下载多）
收益比例（1.4×）大于 2048 臂（1.13–1.16×）。

### 内存（计数分配器，同进程同夹具，双跑逐字节复现）

| 面 | into_chunks | per_download_vec | 比 |
|---|---:|---:|---:|
| 排出窗 allocs（stampede_2048） | 2069 | 4117 | **0.503×**（恰少 2048 次） |
| 排出窗 bytes | 311,152 | 573,296 | **0.543×** |
| 排出窗 peak live | 191,560 | 191,704 | 1.00×（打平） |
| build+drain allocs | 10,961 | 13,009 | 0.842× |
| build+drain bytes | 940,976 | 1,203,120 | 0.782× |
| build+drain peak live | 381,010 | 381,010 | 1.00×（打平） |

峰值打平的机制：两形状的 peak 都由 `result.chunks` 倍增 realloc 瞬间决定
（128KB 新缓冲先分配、64KB 旧缓冲后释放 ≈192KB），该瞬态与 staging 无关；
staging 消除只削分配次数与累计字节，不动峰值。

## 被测量否决的前提（记录在案，勿再试）

本席先验假设「每完成一个下载 `VecDeque::remove(index)` O(n) 尾移 = O(n²)
排出」（fleet.rs OPT-06-01-02 同形），照此实现了 `retain_mut` 单遍压实版。
**测量否决**：顺序排出中每次 remove 都落在队头，`VecDeque::remove` 取较短侧
移位 = O(1)，旧形状并不退化；单遍压实版反而慢 ~15%
（drain_2048：771.81µs vs 668.79µs r1，已 revert）。教训：**同形算法教训不可
跨数据结构搬运，先 bench 后 ship**。

## 门禁

- `cargo test -p server-runtime --lib server_download`：11/11 绿（9 既有序钉 + 2 新）。
- `cargo test -p server-runtime --test download_drain_alloc`：2/2 绿（孪生全等 + 可复现）。
- `cargo test -p server-runtime`：1737 passed / 0 failed。
- `cargo clippy -p server-runtime --lib --benches --tests -- -D warnings`：
  0 error（仅既有基线 blowfish unused-workspace warning）。
- `cargo fmt -p server-runtime -- --check`：净。

## 坑

- **criterion 二进制直跑必须带 `--bench` 旗标**：不带时 criterion 按
  `(bench=false, _) → test_mode` 走测试模式，输出只有 `Testing X / Success`、
  不产数字不落 baseline（criterion 0.8.2 lib.rs:960）。数字行只在 `--bench` 下出现。
- criterion `--save-baseline` 默认名 `base`；跨轮对比需显式 `--baseline`，
  否则第二轮起打印的是对上一轮的 delta 而非绝对值。
- benches/ 下共享模块（本例 `legacy_queue.rs`）会被 autobench 当独立 bench
  target 编译——`[package] autobenches = false` + 显式 `[[bench]]` 才干净；
  孪生的手写 `Default` 会被 `derivable_impls` 打回，改 derive。

## 关联

- peer 面（main 已勾）：`6ffd9af`/`bdf1327`/`c23987a`/`18618a9`（客户端组装，
  峰值减半）；`790332b`（fleet retry 队列 O(n²)→O(n)，drain_512 9.65×）。
- 本面 lane-only，行保持 main 勾账形态；后续主席可择、可并、可弃。
