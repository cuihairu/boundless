# BW-14-04 可安装发布物——安装布局与 CLI 比对

**验收** 为三端提供运行时、Python 模块/标准库、配置样例、工具与资源安装布局；比对公开配置和 CLI；测试干净安装、升级保留数据、启动与卸载。

**结论** 安装布局（bin + lib/python + res + data）、归档、干净安装、升级保留数据、卸载均已在 Linux 落地并通过 lane 自检与实测（`ci/install-layout.sh`）。模块导入无需开发环境。比对原版公开配置与 CLI 逐项给出。Windows/macOS 两端的干净安装烟测与服务器进程首启消费 bw.xml 登记为卡点。

## 1. 安装布局（本移植）

由 `ci/install-layout.sh` 布置，`/opt/boundless`（等价于原版 `/opt/bigworld` 惯例；lane 实测前缀为 `ci-out/install-layout/install-smoke`）：

| 路径 | 内容 | 归属 |
|---|---|---|
| `bin/boundless-server` | 服务端角色二进制（CLI：`boundless-server <role>`，8 角色） | BW-00-03-05 / BW-15-02-01 |
| `bin/process_defs` | entitydef CLI（CLI：`process_defs [-r res] …`，参考兼容选项与退出码） | BW-04-04-01 |
| `lib/python/boundless.so` | CPython 模块（cdylib 的安装改名 `libboundless.so → boundless.so`，即 `crates/python-module/tests/import.rs` 钉死的 CPython 发现名） | BW-00-03-03 |
| `res/server/bw.xml` | 服务器配置（`BWResource` 链首默认） | BW-01-04 / BW-14-01 |
| `res/scripts/…` | 样例实体定义与脚本（21 项生成资源镜像） | BW-14-01 |
| `data/` | 用户数据目录——**不在归档内**：install 创建、upgrade 不动、uninstall 保留 | 本项 |

归档只携带包属树（`bin lib res`）。`data/` 在归档之外，这正是 upgrade/uninstall 能保留用户数据的机制。

## 2. 原版公开配置与 CLI 比对

| 面 | 原版（EL7 RPM / package-daily.sh） | 本移植 | 结论 |
|---|---|---|---|
| 安装前缀 | `/opt/bigworld/14.4/<role>` + `/opt/bigworld/current` 符号链接（bwmachined RPM：`opt/bigworld/14.4/bwmachined/{sbin,init.d}`、`etc/bwmachined.conf`、`etc/sysctl.d/bwmachined2.conf`、`usr/lib/systemd/system/bwmachined2.service`） | `/opt/boundless/{bin,lib/python,res,data}`（单一前缀，无版本化子目录——包名带版本而非路径） | 前缀规范保留、版本化路径改为文件名版本（偏差，已注） |
| 服务端 CLI | `bwmachined2`（machined 守护进程；`-r/--res` 资源路径、`-uid` 等） | `boundless-server <role>`（`baseapp`/`baseappmgr`/`cellapp`/`cellappmgr`/`dbapp`/`dbappmgr`/`loginapp`/`reviver`；`CI_RUNNERS_TIMEOUT` 预算自停，`--res`/`-r` 与 `BW_RES_PATH` 由 `resources::init_from_args` 消费） | 角色选择替代按 app 的二进制切分；`-r` 资源路径语义对齐（BW-01-04-01） |
| 工具 CLI | `process_defs`（`tools/process_defs`） | `process_defs`（`-r/-f/-m/-p/--use-stdout/-v/-h`，用法与退出码逐项对齐参考） | 一致 |
| 配置 | `server/bw.xml` + `server/bw_<user>.xml` 个性化 + `<parentFile>` 继承链 | 同（BW-01-04-02 逐键对齐） | 一致 |
| 版本来源 | `programming/bigworld/vcpkg.json` `version`，发布名 `+daily<YYYYMMDD>.<sha>`；**无 tag、无 GitHub Release**（repo 规则） | workspace `Cargo.toml` `version`，归档名 `boundless-<ver>-<platform>.tar.gz`；**无 tag、无 Release**（全局红线） | 一致（不发版、每日构建面） |
| 交付格式 | tar.gz/.deb/.rpm（`package-daily.sh`；devel/web-integration RPM payload 被裁为占位，见 BW-14-03-01） | tar.gz（lane 产物；deb/rpm 面未做——Rust 侧无对应 EL7 打包需求，随 BW-15-02 平台面） | 偏差（已注） |
| 嵌入 Python | Python 3.13 内嵌（vcpkg，静态链） | 稳定 ABI 模块载入宿主解释器（`libboundless.so` 任意 CPython 3.x 可载） | 模块面等价；**标准库随宿主解释器交付**，私有 stdlib 打包随内嵌宿主（python-host / BW-03）落地 |

## 3. 干净安装、升级保留数据、启动与卸载（实测）

`ci/install-layout.sh`（self-test 5 项 + 实测全绿；manifest 见 lane 输出）：

- **install-build**：`cargo build --release -p boundless-server -p entitydef -p python-module` 产三件套。
- **install-stage**：前缀含 `bin/`（两命令）、`lib/python/boundless.so`、`res/`（bw.xml + 样例脚本），**无 `data/`**（设计如此）。
- **install-archive**：`boundless-0.1.0-linux-x86_64.tar.gz`（8.4M）。
- **install-clean**：清空前缀后解包 + 建 `data/`。
- **install-smoke-server**（01/04 Linux 半）：装后 `boundless-server baseapp` 在 `CI_RUNNERS_TIMEOUT=3` 预算内启动（`starting`→`running`）并自停（`has shut down.`），exit 0。
- **install-cli**（01）：装后 `process_defs --help` 输出用法，exit 0。
- **install-cli-res**（03 工具半）：装后 `process_defs -r <prefix>/res` 定位并加载安装资源树（含定义摘要）。
- **install-module**（02）：`PYTHONPATH=<prefix>/lib/python` 下用裸解释器 `import boundless`（`__version__`、`version()` 一致），无 cargo/无 target 目录/无 venv。
- **install-layout**：配置、样例资源、两二进制、模块、`data/` 齐全。
- **install-upgrade**（05）：篡改包属文件 `res/server/bw.xml` → 升级解包将其还原为打包字节，种子用户数据 `data/db/user.db` 保留。
- **install-uninstall**（05）：卸载删除 `bin/ lib/ res/`，`data/db/user.db` 保留。

### 自检（self-test，均绿）

绿色 stub 全链路（stage→archive→install→launch→import→retention）；红色各臂——启动失败以退出码命名、CLI 失败以退出码命名、坏模块导入失败、缺实体定义资源树加载失败——均按预期咬合。

## 4. 卡点

- **BW-14-04-03 服务器首启半**：`app::run`（apps/server 最小入口）尚未消费 bw.xml——资源根初始化（`resources::init_from_args`）与配置链（BW-01-04）已在库层落地，**接入 `app::run` 属 BW-15-02-01 剩余**（该行已记「bw.xml/接口初始化（BW-01-04）」）。本项已由装后 `process_defs -r res` 验证安装资源树可定位可加载；服务器进程首启消费 bw.xml 待 BW-15-02-01。
- **BW-14-04-04 三端**：Linux 干净安装烟测绿；Windows/macOS 无 runner（BW-15-03/04 启动目标前，`signals.rs` 用 Linux 专用信号 API），三端启动/停止烟测随 BW-15-03/04 落地。

## 5. 结论

01（装后命令可执行）、02（无开发环境可导入）、05（升级/卸载保留用户数据）验收满足并勾行；03 布局与工具侧定位满足、服务器首启消费 bw.xml 卡 BW-15-02-01；04 Linux 半满足、三端卡 BW-15-03/04。
