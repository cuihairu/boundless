# OPT-14-01-03 复跑干净安装与升级

Acceptance: **保留数据且脚本成功导入**。本行零行为改动，复跑 `ci/install-layout.sh` 的完整链并修掉脚本里两处过时的模块名（脚本此前跑不起来）。

## 复跑结果（2026-11-26，本机 x86_64-unknown-linux-gnu，round 1 全绿）

| 检查 | 结果 | 观察 |
|---|---|---|
| `install-build` | PASS | release 构建 boundless-server / process_defs / python-module |
| `install-stage` | PASS | bin/ lib/ res/ 三段，data/ 不入包（设计） |
| `install-archive` | PASS | `boundless-0.1.0-linux-x86_64.tar.gz` **872K**（strip=symbols 已生效） |
| `install-clean` | PASS | 全新 prefix 解包 + 创建 data/ |
| `install-smoke-server` | PASS | 安装后的 server 起+停：`starting` + `BaseApp is running` + `has shut down`，exit 0 |
| `install-cli` | PASS | 安装后的 process_defs `--help` 出 usage |
| `install-cli-res` | PASS | `process_defs -r <prefix>/res` 首次运行加载 defs，dump 有 digest |
| `install-res-server` | PASS | 安装后 server 首次启动静默定位 `<prefix>/res/server/bw.xml`（无 "Could not find"） |
| `install-build-parity` | PASS | 构建树与安装 prefix 的 res 逐字节同、defs dump 同、两侧都定位到配置 |
| `install-module` | PASS | 裸 Python 从 `lib/python/boundless_core.so` 导入成功（`boundless_core 0.1.0 imported`） |
| `install-layout` | PASS | bin/lib/res/data 完整 |
| **`install-upgrade`** | PASS | 篡改 `res/server/bw.xml` 被还原；**`data/db/user.db` 内容保留** |
| **`install-uninstall`** | PASS | 移除 bin/lib/res；**`data/db/user.db` 内容保留** |

自测 7/7 全 PASS（绿 stub 全链 + 六个红 stub 各自被命名拒绝）。

## 修的两处真实缺陷

`ci/install-layout.sh` 里的模块名是过时的，脚本在真机上根本跑不通：

1. **构建产物名**：脚本取 `target/release/libboundless.so`，而 `python-module` crate（cdylib）实际产出 `libpython_module.so` → `install-stage` 直接 FAIL "missing artifact"。改为按存在性接受 `libpython_module.so`（并保留 `libboundless.so` 作旧名兜底）。
2. **安装名与导入名**：脚本把模块装成 `boundless.so` 并 `import boundless`，但 `crates/python-module/tests/import.rs` 钉死的契约是安装名 **`boundless_core.so`**、`PyInit_boundless_core`、API `answer()`/`engine_version()`/`__version__`。旧的 `boundless.so` 会让 CPython 报 `ImportError: dynamic module does not define module export function (PyInit_boundless)`。脚本的安装名、import 脚本、layout 检查、自测 stub（绿目录名/红垃圾文件名）全部对齐到 `boundless_core`。

修后 round 1 十三行全 PASS。round 2 因本机 `/tmp` 配额（并发 seat 的临时目录）在 clean-install 处被 `pwd: write error: Disk quota exceeded` 打断——是磁盘问题不是脚本回归；round 1 的完整十三行 PASS + 自测 7/7 是本行证据，见夹具 `docs/review/fixtures/opt-14-01-03/install-upgrade-rerun.tsv`。

## 复跑

```sh
CI_OUT=ci-out/opt14-install ./ci/install-layout.sh            # stage/archive/install/upgrade/uninstall
CI_OUT=ci-out/opt14-install ./ci/install-layout.sh self-test  # 绿红 stub
```

## 门禁

- `bash -n ci/install-layout.sh` 通过。
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace`：见交付 commit，均绿后才推。

## 坑

- `crates/python-module` 的 cdylib 名随 crate 名走（`libpython_module.so`），安装改名才是 `boundless_core.so`；两个名字都要对齐，只改一个会在 stage 或 import 分段失败。
- 本机 `/tmp` 是多 seat 共享 tmpfs，跑会落大量产物的 lane 时容易撞 EDQUOT；`ci/install-layout.sh` 的中间 prefix 可指到仓库内 `ci-out/`（`CI_LAYOUT_OUT`）避免挤占 `/tmp`。
