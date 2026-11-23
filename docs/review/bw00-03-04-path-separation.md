# BW-00-03-04 分离资源、配置、原版产物和测试路径 — 记录

- **任务编号**: BW-00-03-04（lane BW-00-03 Boundless 构建入口，todo.md 行 124）
- **验收**: 同机两套环境互不覆盖。

## 结论（四类路径各自独立、互不覆盖）

四类路径各自的独立来源，与参考侧同源（BW-00-02-01 已录原版运行环境）：

| 类别 | 独立路径 | 来源 |
|---|---|---|
| 本地资源 | 只读输入 | `--res`/`-r` 参数 → `BW_RES_PATH` env → `~/.bwmachined.conf`（`BwResource::init_from_sources`，`crates/resources/src/bwresource.rs`） |
| 配置 | 只读输入 | `server/bw.xml` 链，从资源根派生（`BwConfig::init_from_args`，`crates/server-runtime/src/config.rs`） |
| 原版产物 | 只读输入 | `CI_PARITY_REFERENCE` → `BW_REFERENCE` → `$HOME/workspaces/BigWorld`（`ci/parity.sh:49` 既有约定，本项把 `docs/review/tools/make_inventory.py` 对齐到同一链） |
| 测试夹具 | **可写** | `BOUNDLESS_TEST_BASE` env → 本检出 `target/boundless-test`（本项新增） |

只读输入（资源/配置/原版产物）被两套环境共享是安全的——它们不被写，互不覆盖成立。
可写面（测试夹具）按环境分离：两套环境要么各自设 `BOUNDLESS_TEST_BASE`，要么（默认）
各在本检出的 `target/boundless-test` 下——`target/` 已 gitignore、且属该检出独有，故两检出
**由构造**互不覆盖，无需任何环境变量。

## 修复的真实缺陷

1. **`make_inventory.py` 烘焙单机绝对路径**（原版产物路径未独立）：argparse `--root`
   `default="/home/cui/workspaces/BigWorld/programming"`——一台机器的路径写死在工具里，
   另一环境跑它会对不存在路径静默出空清单。改为 `default_reference_root()`：按
   `CI_PARITY_REFERENCE`/`BW_REFERENCE` → `$HOME/workspaces/BigWorld` 解析再拼
   `programming/`（与 `ci/parity.sh` 同一约定），`--root` 仍可全覆盖。实测：无 env →
   `~/workspaces/BigWorld/programming`；`BW_REFERENCE=/opt/bw2` → `/opt/bw2/programming`；
   `CI_PARITY_REFERENCE=/srv/bw3` → `/srv/bw3/programming`。
2. **`python-module` 测试固定临时名**（测试夹具互覆盖）：`tests/import.rs` 用固定
   `boundless-python-module-build`/`boundless-python-module-install`——两检出同机同时跑
   `cargo test -p python-module` 会共用同一 scratch target dir（互相抢构建锁、互写
   `boundless.so`）。改为按 `std::process::id()` 后缀，两进程天然隔离。实测两并发跑都
   `rc=0`，且各自用 `install-<pid>` 独立目录。
3. **测试夹具基座未按环境分离**：`test-support` 原先所有夹具共用全局 `/tmp/boundless-test`
   单一命名空间（仅靠 pid 唯一名保并发安全）。新增 `workspace::base_dir()`：
   `BOUNDLESS_TEST_BASE` env → 否则 `<检出>/target/boundless-test`；`TempWorkspace::with_base`
   让测试可钉死基座，`RunRoot`（logs/db/artifacts）挂到同一基座下 `runs/`。默认行为从
   `/tmp/boundless-test` 改为检出内 `target/boundless-test`（gitignore，两检出由构造分离）。

## 验收测试

`test-support` 新增 `workspace::tests::two_environments_do_not_overwrite_each_other`：两环境
各设基座，写**同名** `state.db`（不同内容），断言互不覆盖、根不相交、一环境清理不伤另一
环境。另 `allocation` 既有 `two_parallel_runs_never_overlap` 继续覆盖端口/库槽位分离。

## 测试与门禁

- `cargo test -p test-support`：**113 passed / 0 failed**（含新验收测试）。
- `cargo test -p python-module`：**3 passed / 0 failed**（unit 1 + integration 2）。
- 两并发 `cargo test -p python-module`（同机两进程模拟两检出）：双双 `rc=0`，安装目录按 pid 分离。
- `cargo fmt --check -p test-support -p python-module` 通过；`cargo clippy -p test-support
  --all-targets` 本 crate **0 警告**（余为 `server-runtime` 在途 `type_complexity`/`collapsible_if`
  两处，兄弟会话未提交改动，非本项）。
- `make_inventory.py` 用 env 链解析默认根，实测三档正确。
- **workspace 级 `cargo test --workspace` 仍红**：`platform` test `signal_call_sites`
  （`crates/platform/tests/`，兄弟会话在途未提交）E0596 编译错；且 committed members 含
  `crates/db` 但该目录未跟踪（兄弟会话债务，干净检出 workspace 无法载入）。两项均非本项引入；
  本项只触 test-support/python-module/make_inventory.py，各 crate 独立绿、按 pathspec 提交。

## 如实登记

- **`crates/mercury/src/network_utils.rs:96`** 有固定临时名 `boundless-max-buffer-test`（BW-02
  兄弟 crate 的单测辅助），同类「同机两检出互覆盖」风险——不在本 lane 的 crate，未动，留待
  BW-02 侧清扫。
- 参考侧无「环境路径」声明面；本项路径独立是**新版**约定（与 `BW_RES_PATH` 既有 env 同风格），
  不假造原版有对应物。

## 状态

- [x] 资源路径独立（`--res`/`BW_RES_PATH`，既有）
- [x] 配置路径独立（资源根派生，既有）
- [x] 原版产物路径独立（env 链，make_inventory.py 对齐）
- [x] 测试夹具路径独立（按环境基座，默认检出内 `target/`）
- [x] 同机两套环境互不覆盖（验收测试 + 两并发实测）
