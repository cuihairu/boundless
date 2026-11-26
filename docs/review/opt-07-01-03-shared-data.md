# OPT-07-01-03 优化 SharedData 传播

Acceptance: **量化消息量变化**。本行交付共享数据传播的**消息量台账**（结构性、钉测）+ 传播构造面的基准，量化结论：**消息量变化 = 0（协议地板）**。

## 消息量台账（结构性，`the_shared_data_message_volume_is_one_per_subscriber` 钉测）

一次 mutation 发出的消息数（N_base=就绪 BaseApp 数，N_cell=就绪 CellApp 数，E=已有共享条目数）：

| 路由 | 消息数 | 构成 |
|---|---|---|
| BASE_APP set（BaseAppMgr 权威） | **N_base** | 每 BaseApp 恰一条广播，无跨管理器跳 |
| CELL_APP（经 BaseAppMgr） | **N_cell + 1** | 1 forward + 每 CellApp 一条；不回 relay（BaseApp 看不见 cell 数据） |
| GLOBAL（CellAppMgr 权威） | **N_base + N_cell + 1** | CellApp 广播 + 1 relay（BaseAppMgr 镜像后广播） |
| GLOBAL_FROM_BASE_APP | **N_base + N_cell + 2** | GLOBAL 路由 + 1 初始 forward（唯一带双管理器 relay 的路由） |
| 新 app join-sync（finishedInit） | **E**（每新 app） | 管理器把权威 map 逐条递给迟入者 |

每条消息载荷 = data_type(1) + key(count+len) + value(len+count) + original_src(6 字节地址)。

## 量化结论（诚实判读）

- **消息量变化 = 0**：每个订阅 app 恰好收到一条，四条路由已是参考（`baseappmgr.cpp:2112`/`2146`/`2183`、`cellappmgr.cpp:1924`/`1985`）的最小路由形——任何减量都要么丢一个订阅者（行为破坏）要么丢权威镜像（恢复面破坏）。台账由钉测钉死，本行之后任何改动碰消息数都会被测试拦下。
- **可量化且可优化的面是构造 CPU，不是消息数**：广播每 app 分配 key/value 各一份字符串（协议形状决定，每 app 消息独立拥有载荷）。
- join-sync 面在 E 大、频繁重启握手时有 E×(新 app 数) 的放大，属参考行为（权威同步语义），量化记录不裁剪。

## 基准读数（2026-11-26 本机，`shared_data` bench，热机读数不入带）

| 面 | 消息数 | 中位 | 吞吐 |
|---|---:|---:|---:|
| `base_set_64` | 64 | 7.15 µs | 8.95 M msg/s |
| `base_set_512` | 512 | 81.0 µs | 6.32 M msg/s |
| `global_from_base_64` | 130 | 22.2 µs | 5.85 M msg/s |
| `global_from_base_512` | 1026 | 163.9 µs | 6.26 M msg/s |
| `join_sync_64` | 64 | 18.1 µs | 3.54 M msg/s |

构造吞吐 ~6–9 M msg/s、线性于 fleet 规模；join-sync 慢一档（BTreeMap 权威 map 迭代 + 注册表查找）。

## 门禁

- `cargo test -p server-runtime --lib`：**1703 passed / 0 failed**（新台账测含内）。
- `cargo clippy -p server-runtime --lib --benches -- -D warnings`：0 error。
- `cargo fmt -p server-runtime -- --check`：净。

## 坑

- 台账测试首写把「plain GLOBAL 经 CellAppMgr」当成零 relay——实际 `relay(GLOBAL)` 恒 Some（BaseAppMgr 镜像是权威语义的一部分），GLOBAL 路由天然带 1 条 relay；GLOBAL_FROM_BASE_APP 只比它多初始 forward。
- `CellAppRecord::new` 的 id 参是 `CellAppId=u32`，测试里 `i+1`（u16）要显式 cast。
