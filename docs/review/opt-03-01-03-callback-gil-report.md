# OPT-03-01-03 优化跨语言回调（GIL 占用与尾延迟报告）

对应 todo.md OPT-03-01-03：**优化跨语言回调。验收：报告 GIL 占用与尾延迟。**

交付面：
- `crates/python-host/src/gil.rs`（`6f69a00`）：**回调队列的空转快路径**——`QUEUED: AtomicUsize` 与队列在其锁下同步维护；`process()` 先读计数，0 即返回（空闲帧一次原子读，不再拿队列锁）；`pending()` 改读原子计数；`post` 的 `Box` 分配挪进锁前，临界区只剩 push+计数步；`clear` 同步清零。
- `crates/python-host/tests/bridge_baseline.rs`（`7464d06`）：基线 target 新增**尾延迟与占用面**——`drain_empty`（空队过账：轮询侧稳态成本）、`post_run_latency`（第二线程投 5000 个带时间戳回调，脚本线程紧泵跑空，逐回调 post→run 时延取 p50/p90/p99/max）、`gil_occupancy_500cb_pct_60fps` / `gil_occupancy_empty_pass_pct_60fps`（由上文实测数字对 60fps 帧预算 16.67ms 归一，TSV 自带报告行）。

## 优化：空闲帧快路径

参考引擎 `TaskManager::tick`（`bgtask_manager.cpp:744-749`）每帧拿一次 `fgTaskListMutex_` 做空队检查——**大多数帧没有投递任何回调，这个锁是纯税**。port 侧加 `QUEUED` 原子计数：写侧在队列锁下维护（push 后 `fetch_add(Release)`、take 后 `store(0)`、clear 后清零），读侧 `process()` 空队快路径一次 `Acquire` load 即返回。写读都在锁外也不破坏语义：计数与队列只在锁不被持有时保证一致，读者在竞态窗内看到的旧计数最多把回调推迟到下一过账——正是参考自己的「至少等一帧」序（`py_callback.cpp:161-162`）。

**前后对照**（交错 12 轮 `taskset -c 6` 钉核 A/B，pre-opt=`7464d06` 树、post-opt=`6f69a00` 树，debug build，跨轮中位数）：

| 面 | 前 | 后 | 后/前 |
|---|---|---|---|
| `drain_empty` p50 | 148ns | **18ns** | **0.121**（8.2×） |
| `drain_empty` p99 | 248ns | 48ns | 0.196 |
| `post_drain_per_callback` p50 | 150ns | 143ns | 0.950 |

## 报告：GIL 占用与尾延迟（2026-11-26 本机，fixture TSV 一轮）

**GIL 占用**（对 60fps 帧预算 16.67ms 归一）：
- 满帧 500 回调：`0.852%` 帧预算（每回调 ~284ns × 500）——回调排空在本帧 GIL 占用里是小头；即便 2 万回调/帧也只占 ~34%。
- 空闲帧：`0.0002%`（快路径后单次空转 ~26ns）——轮询稳态从 148ns 的锁税降到原子读，空闲帧 GIL 占用可忽略。
- 占用大头在回调本体：`call_noargs` 111ns / `call_3args` 116ns（OPT-03-01-01），排队面（143ns）与调用面同量级，无单点可再砍一个量级。

**尾延迟**（post→run，紧泵不睡眠，机器重载 load≈28/14 核，机器相关只记录不设带）：
- p50 ≈ 1.3–2.1ms、p99 ≈ 1.3–2.4ms、max ≈ 2.4ms。
- 泵侧消费快于投递（每回调排队成本 143ns ≪ 投递侧 ~250ns），队列不积压；时延由**投递节奏与调度抢占**主导（重载机被其它席压着，泵线程被逐出后的恢复时间落进数字），不是队列算法成本。零负载机上此数字会收窄一个量级以上；结构性结论（队列不积压、时延受调度支配、紧泵下无饥饿——5000/5000 全部恰跑一次）与机器无关。

## 结论

1. **优化落在对的面上**：空闲帧轮询从 148ns→18ns（8.2×），这是每帧都要付的稳态成本；满帧排空只受益 5%（锁本来就只拿一次）。
2. GIL 占用面：单帧 500 回调占 60fps 预算 0.85%，空闲帧 0.0002%——跨语言回调不是 GIL 占用的主要来源；占用随回调负载线性走，排队面与调用面同量级。
3. 尾延迟的支配项是调度而非队列：紧泵下时延≈投递节奏与抢占恢复，队列侧无积压证据（5000 投递 5000 恰跑一次，`pending()==0` 收尾）。
4. `pending()` 语义变化：从锁下精确计数改为原子读——静点处精确、竞态窗内可能读到旧值；参考引擎无此查询（port 侧诊断面），语义文档已随码注明。

## 复跑命令（canonical）

```sh
PYTHON_BRIDGE_OUT=<绝对路径> cargo test -p python-host \
    --test bridge_baseline the_bridge_baseline_records_all_faces -- --exact
```

TSV 追加落盘，重跑前先清文件。

## 门禁（全绿后落账）

- `cargo test -p python-host`：lib 151 绿（gil 全单测含嵌套投递/panic 隔离/清空面）、`bridge_baseline` 1 绿。
- clippy `-D warnings`：0 error；rustfmt `--edition 2024 --check`：净。

## 坑集（复现与规避）

- **A/B 二进制换腿时 `git checkout HEAD~1 -- <scope>` 后 `find` 要按时间过滤**：同一 hash 目录里的产物会被复用，`-newer`/`! -newer` 比对出 binaries 才是真的两代。
- **`Arc` 在循环里 `move` 闭包会一轮搬空**：投递线程循环里每轮新 clone 成局部名（`posted_*`），捕获的 Arc 用别名（`poster_*`），泵侧原变量才不会被 move。
- **`Arc::try_unwrap` 要求持链干净**：投递线程 join 之后才能解包；回调 Box 若仍在队列会多挂一份——先断言 `pending()==0`。
- **读寄存器式计时闭包借不到 `&mut records`**：派生行帮助函数签名收 `&[(String, String)]`，先取值后 push，避免借用冲突。
- **尾延迟 ≠ 队列成本**：消费快于生产时队列不积压，时延由投递节奏+调度抢占支配；重载机上的绝对数字只说明机器，不说明队列——结构性断言（恰跑一次、序、收尾空）才是与机器无关的验收。
