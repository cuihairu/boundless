# OPT-08-01-03 评估空间并行

Acceptance: **报告跨边界时序及耗时**。本行交付空间并行可行性的原型基准（`spatial_parallel`，`0edba71`）+ 跨边界时序汇总。

## 结构结论：witness 面是状态不相交的，天然可空间切分

每个 client-backed 实体一个 Witness，各自拥有 AoI map、优先级队列、bundle 与 alias 池——**witness 之间零共享状态**；实体快照库是只读共享（读-读无锁）；输出按 client 分桶互不相交。因此按空间分片（连续网格行）驱动 witness 是**零锁并行**：分片=「AoI 完全落在分片内的 witness 集合」，唯一同步点是 AoI 边界变化。

## 跨边界时序（每实体跨越成本，2026-11-26 本机）

一次跨界迁移的三段时序（全部实测面，密度=全员在 AoI 的密集最坏形）：

| 段 | 耗时/实体 | 出处 |
|---|---:|---|
| 1. 跨界判定（`collect_offloads` BSP 决策扫） | ~6.6 ns/real @512 | OPT-08-01-01 |
| 2. witness 侧 rehome 波（leave→re-enter→create 三 flush） | ~1.2 µs/实体（64 实体波 78.7 µs） | OPT-08-01-02 后读数 |
| 3. 广播量代价（协议地板） | 24 wire 字节/实体（LeaveAoI+EnterAoI+CreateEntity） | OPT-08-01-01 钉测 |

并行设计的跨界规则：跨界实体的 witness 留驻其所属分片（不跨片加锁），实体真迁移时付 rehome 波（上表段 2）——即并行化的代价就是既有迁移面，无新增跨片同步原语。

## 耗时：fleet 级并行伸缩（`spatial_parallel` bench，taskset -c 4-7，载 13 的完整一轮）

每 pass = 全 fleet 各 witness 一个密集 tick（64 watched 全移动、全队排干）；吞吐单位=witness-pass/s（每 pass 含 64 watched）：

| fleet | serial | par2 | par4 |
|---|---:|---:|---:|
| 64 witnesses（~2.5 ms/pass） | 15.9 K/s（2.46 ms） | 1.32×（1.87 ms） | **2.72×**（0.90 ms） |
| 512 witnesses（~26 ms/pass） | 19.2 K/s（26.2 ms） | 1.79×（14.6 ms） | **3.41×**（7.68 ms） |

诚实判读：
- **伸缩对机器负载敏感**：同一基准在载 24 的一轮只读出 512-par4 1.62×、载 18 读出 2.74×、载 13 读出 3.41×——共享机上其他席位的编译负载直接压进读数；report 只取完整一轮（载 13）的全部六面，早两轮比值留此作敏感性记录，不作结论依据。
- **pass 越短并行越亏**：64-fleet（~2.5 ms/pass）par2 在高载轮曾读出 0.82×（线程 spawn/sync 开销吃掉收益）；512-fleet（~26 ms/pass）同步开销摊薄后 par4 3.41× 接近 4 核结构限（差距=内存带宽份额：harness 喂移动的每实体快照克隆是访存密集项，实测 pass 含此接缝成本，真实运行时在上游原地变更无克隆）。
- spawn 阈值：scoped-threads 每 pass 起 K 线程，pass 需 ≥ ~10 ms 才摊得开；tick 级（≤5 ms）并行化需要线程池常驻而非每 pass spawn——属运行时集成项，本评估不动。

## 报告结论（安全空间并行）

1. witness 面**可安全并行**（状态不相交+只读共享），分片界=AoI 边界，跨界代价=既有 rehome 波（无新增锁）。
2. fleet 级实测 par4 3.41×（512 witnesses、载 13 完整轮）；短 pass/高载下收益退化，集成需常驻线程池。
3. AOI 索引面（BSP 决策扫）无可并行收益（~6.6ns/real，扫描本身不构成瓶颈）。

## 门禁

- `cargo test -p server-runtime --lib`：**1704 passed / 0 failed / 1 ignored**。
- `cargo clippy -p server-runtime --lib --benches -- -D warnings`：0 error。
- `cargo fmt --check`：净。

## 坑

- 共享机上并行基准必须钉核+记负载：载 24→13 的三轮同基准比值漂了 2 倍多；只取同轮六面、负载记入证据页，跨轮不可混。
- `cargo bench` 复跑会与上一轮比对出 change%——那是上一轮（不同负载）的基线，不是 A/B 腿，勿误读为回归。
