# OPT-14-01-02 优化包布局或首次启动

Acceptance: **报告体积和耗时差异**。本行只改 release profile 的符号剥离（不发布、不打 tag），A/B 两腿从同一棵树各自独立构建，记录二进制/tarball 体积与首次启动耗时。

## 改动

`Cargo.toml` `[profile.release]` 增 `strip = "symbols"`：`opt-level = 3` 与 `debug = true` 不变，编译出的代码逐字不变，只在链接后剥掉符号表/debuginfo。开发期诊断用 `cargo build`（debug profile）或 `cargo test`，不受影响。

## A/B 数字（2026-11-26，本机 x86_64-unknown-linux-gnu）

| 面 | baseline (`strip=none`) | candidate (`strip=symbols`) | 差异 |
|---|---:|---:|---:|
| `boundless-server` 二进制 | 116,126,768 B (~111 MiB) | 792,160 B (~774 KiB) | **146.60× 更小** |
| tar.gz（bin/ 布局，同 package.sh） | 23,678,579 B (~23 MiB) | 401,146 B (~392 KiB) | **59.03× 更小** |
| 到 `BaseApp is running` | median 25.99 ms（14 次） | 17–20 ms（3 次） | 同区间，无回归 |

基线二进制 111 MiB 中约 115 MiB 是 debuginfo：剥离不改变生成代码，只改变安装包的传输/落盘体积。首跑读数受启动时机分辨率（50 ms 轮询）和机器负载影响，17→26 ms 区间内不宣称提速。

## 复跑

```sh
CI_OUT=ci-out/opt14-ab ./ci/package-size-ab.sh
```

脚本把 baseline/candidate 输出到两个独立 `CARGO_TARGET_DIR`（避免复用产物），`--config profile.release.strip=...` 覆盖 profile，产出各自的 bin + 同布局 tar.gz，并输出体积与比值的 manifest 行。数值夹具：`docs/review/fixtures/opt-14-01-02/package-layout-ab.tsv`。

## 门禁

- `cargo build --release -p boundless-server` 两腿全绿（脚本内两腿均实编，非拷贝估算）。
- `cargo fmt --all -- --check` 净；`cargo clippy --workspace --all-targets -- -D warnings` exit 0；`cargo test --workspace` 全绿（见交付 commit）。
- 新脚本 `bash -n` 通过。

## 坑

- `profile.release.strip` 的合法值是 `none`/`debuginfo`/`symbols`，**不是** `false`——用 `false` 覆盖会让所有 crate 报 `incorrect value for codegen option strip` 并整棵构建失败。
- A/B 两腿必须独立 target dir；同一 `target/release` 复用会让 cargo 判 fresh 而不重编，测到同一产物。
- tarball 体积随 gzip 对 debuginfo 的压缩率变化，二进制缩小 146× 但 tar 只缩 59×，不要混用两个比值。
