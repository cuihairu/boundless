# OPT-16-01-01 采集依赖与增量构建基线

Acceptance: **定位重编源及耗时**。本行零行为改动，交付可复跑的测量入口与首次基线。

## 交付面

- `ci/build-baseline.sh` — 两个模式：
  - `fanout`：`cargo metadata --no-deps` 统计每个 workspace crate 的**直接依赖者数**（改谁会触发多少下游重编），写 `build-fanout.tsv`。
  - 默认全量：fanout + 冷构建 + 增量构建（私有 `CARGO_TARGET_DIR`，不碰开发者的热缓存）。增量腿 `touch crates/foundation/src/lib.rs` 一个文件，从 cargo 的 `Compiling` 行数出实际重编 crate 链。
- `docs/review/fixtures/opt-16-01-01/build-fanout.tsv` — 扇出表 + 冷/增量耗时。

## 基线数字（2026-11-26，本机，`-p boundless-server`）

**重编源排序**（直接依赖者数，前 6）：

| crate | 直接依赖者 |
|---|---:|
| foundation | 19 |
| resources | 12 |
| physics | 6 |
| platform | 5 |
| entitydef / mercury | 3 |

**耗时**：

| 腿 | 冷 | 增量（touch foundation/src/lib.rs） | 重编 crate 数 |
|---|---:|---:|---:|
| release | 220.59 s | 96.25 s | 11 |
| debug | 53.47 s | 3.81 s | 11 |

重编链（release 与 debug 同 11 crates）：foundation → platform、resources、contracts、physics、waypoint、mercury、entitydef、db、server-runtime、boundless-server——即 server 二进制的全依赖闭包。**判读**：编辑 foundation 的一行 = 全链重编；debug 循环 3.8 s 可接受，release 增量 96 s 的大头是下游 LTO/优化码（release 增量省 56% 但绝对值仍大）。这是后续 OPT-16-01-02（拆分高耦合模块）的对照基线。

## 复跑

```sh
CI_OUT=ci-out/opt16 ./ci/build-baseline.sh            # 全量（约 5–6 min）
CI_OUT=ci-out/opt16 ./ci/build-baseline.sh fanout     # 仅扇出表（秒级）
```

## 门禁

- `bash -n ci/build-baseline.sh` 通过；两腿实跑全 PASS（manifest 见交付 commit 证据）。
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` 全绿后推（见交付 commit）。

## 坑

- 冷构建腿必须私有 `CARGO_TARGET_DIR`——共享 `target/` 时 cargo 判 fresh 不重编，冷腿测的是空转。
- `touch` 已能触发重编（mtime 变化即重编），不必改内容；增量腿的 11 crates 是闭包事实，非并行度伪影。
- debug 与 release 的增量差（3.8 s vs 96 s）来自 codegen 单位与优化，不是依赖图差异——两边重编的是同 11 个 crate。
