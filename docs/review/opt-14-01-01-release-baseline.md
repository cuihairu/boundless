# OPT-14-01-01 记录发布包尺寸及首次运行

Acceptance: **三端有基线**。本行只测量、不发布：产物是本地 tarball，版本号读自 `Cargo.toml`，不打 tag、不建 Release（仓库红线）。行保持 `- [ ]`，只登记已有证据面。

## 交付面

- `ci/release-baseline.sh` — 可复跑的测量入口：
  - `release-size-<platform>`：release 二进制字节数（`stat`）。
  - `release-archive-<platform>`：`ci/package.sh` 产出的 tarball 字节数（同一 `CI_OUT` 时可见）。
  - `first-run-<platform>`：从仓库根启动 `boundless-server baseapp`，轮询 `BaseApp is running` 行的时间到 0.05 s 分辨率，取到后立即停进程；缺失启动行判 FAIL，并有红/绿自测。
  - `CI_RELEASE_TARGET` 可让别的 runner 以自己的 target 追加本机的测量；脚本不会把没跑的腿报成 PASS。
- `docs/review/fixtures/opt-14-01-01/release-baseline.tsv` — 机器可读基线（14 次启动的 min/median/max、二进制/tarball 字节数、tarball sha256、两腿 PENDING 原因）。

## 基线数字（2026-11-26，本机 x86_64-unknown-linux-gnu）

| 面 | 值 | 判读 |
|---|---:|---|
| `target/release/boundless-server` | 116,126,768 B (~111 MiB) | release 默认带 debuginfo；`[profile.release]` 未设 `strip` |
| tarball `boundless-0.1.0-linux-x86_64.tar.gz` | 23,678,496 B (~23 MiB) | gzip 后；sha256 见 TSV |
| 到 `BaseApp is running`（14 次） | min 17 ms / median 26 ms / max 85 ms | 首启（build 后第一次）最慢；墙钟读数机器相关 |

**首次运行的真实缺口**：`ci/package.sh` 的解压 prefix 只含 `bin/`，没有 `res/server/bw.xml`。从安装前缀启动会打印 `Could not find server/bw.xml` / `No configuration section.` 但**仍打印** `[baseapp] starting` 与 `---- BaseApp is running ----`，因 `ci/package.sh` 的 smoke 要求 "starting + exit 0"，而进程不会自行退出，30s 超时 exit 124 → package-smoke FAIL。本行的 first-run 从仓库根（有 `res/`）测，即基线是可启动面的时间；安装前缀缺 res 属于 `BW-14-04` 安装布局的剩余面，证据页已记，不伪装成通过。

## 三端状态

- **Linux x86_64**：实测，上表。
- **Windows x86_64**：`cargo build --release --target x86_64-pc-windows-gnu -p boundless-server` 失败在 `mercury` 的 16 处（缺 `platform::interface`/`platform::socket`、`std::os::fd`、`event_dispatcher` 等 Windows 臂），属 BW-15-03 留白，不是本行可修。
- **macOS aarch64**：失败在 `libsqlite3-sys` 的 C 构建（`cc: unrecognized command-line option '-arch' / '-mmacosx-version-min=11.0'`）——本机没有 darwin SDK/跨 C 编译器。

所以本行**不勾**：三端基线尚不齐，两腿标 PENDING 写明卡点，待三端 runner 跑 `ci/release-baseline.sh` 后各自写入 TSV。

## 门禁

- `ci/release-baseline.sh self-test`：绿/红两腿 PASS（有启动行产出数值；缺启动行被命名拒绝）。
- 真机 measure：`release-size` / `release-archive` / `first-run` 三行 PASS，exit 0。
- 全量 `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace`：见交付 commit 记录，均绿后才推。

## 坑

- `ci/package.sh` 把归档放在 `$CI_OUT/package/`，测量脚本必须用同一个 out（本脚本 `CI_RELEASE_OUT` 覆盖，默认 `$CI_OUT`）；`target/release/../../package` 路径是错的。
- 启动行有两行：`[baseapp] starting` 早于 `---- BaseApp is running ----`，后者才是"进入游戏循环"的可观测面，以它计时。
- 二进制 111 MiB / tar 23 MiB 差距来自 debuginfo 的压缩率，不是丢内容。
