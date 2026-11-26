# OPT-06-01-03 优化下载

Acceptance: **比较传输时长及内存**。本行交付下载装配面的内存优化 + 两面的对照数字。

## 交付面

- `crates/server-runtime/src/data_download.rs` — 装配从 `segments.concat()` 改为 `assemble()`：先按总长 `with_capacity` 一次分配，然后按值消费每段（`for segment in segments` 逐段 move）拷入。`concat` 在调用返回前完整结果缓冲与全部段并存（峰值活数据 = 2× 流长）；按值消费让「已写入 + 未消费段」恒等于流长（峰值活数据 = 1× 流长）。多个并发下载同刻完成时，进程峰值 RSS 随此减半。
- 新测 `the_assembly_is_pre_sized_and_consumes_the_segments`：钉 `data.capacity() == 6`（精确一次分配、无中途扩容）与空段臂；其余 7 个行为测不动——装配语义（到达序拼接）不变。
- `crates/server-runtime/benches/download_assembly.rs` + `[[bench]]` — criterion 基准：`complete_64/512`（512 字节/段的流，喂完前 n−1 段，测最后一段触发完成路径）。

## 对照数字（2026-11-26 本机，taskset -c 6 钉核，交错 6 轮独立编译二进制）

| 面 | baseline（concat）中位 | candidate 中位 | 比 | 判读 |
|---|---:|---:|---:|---|
| `complete_64` | 4.80 µs | 4.40 µs | 0.918 | 轮间交叉，噪声内 |
| `complete_512` | 48.47 µs | 40.94 µs | 0.845 | 中位向好但 round 0/4 交叉 |
| 装配峰值活数据 | 2× 流长 | **1× 流长** | **0.5** | **结构性**（按值消费，代码可证，capacity 测钉死一次分配） |
| 传输时长（线面） | `ceil(payload/BUNDLE_BYTES)` mini-tick | 同 | 1.0 | 协议地板，分块契约不动 |

诚实判读：**墙钟传输时长无可靠收益**（≈5–15% 中位向好但轮间不单调）；收益在内存——装配峰值活数据结构性减半，这是「比较传输时长及内存」验收里内存半边的实际改进。传输时长半边的结论是线面地板由 bundle 分块契约钉住（`ServerDownloads::add_to_bundle` 每 mini-tick 一 bundle），不动契约就降不了——这一条与参考一致，记录为对照结论而非优化项。

## 门禁

- `cargo test -p server-runtime --lib`：**1701 passed / 0 failed**（8 data_download 含新测）。
- `cargo clippy -p server-runtime --lib --benches -- -D warnings`：0 error。
- `cargo fmt -p server-runtime -- --check`：净。

## 坑

- criterion 输出在终端窄幅下把基准名和 `time:` 分成两行（`download_assembly/complete_64` 一行、缩进 `time:` 下一行），按单行正则解析会全部漏配——解析器要跨行记住最后见到的基准名。
- 临时 worktree 的 bench 产物路径 hash（`355d96a7171facbc`）与本树相同——hash 由包/基准元数据派生，不代表同代；判代只能 `cmp` 二进制字节。
