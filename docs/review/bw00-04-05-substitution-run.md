# BW-00-04-05 单组件替换运行

> 证据面（BW-00-04-05 验收：报告注明被替换角色）。

由 `tools/substitution_run.py` 执行：同一角色集只换**一个**角色——被替换角色由新版组件 `boundless-server <role>` 启动，其余角色保持原版侧。报告注明被替换角色。

| 项 | 值 |
| --- | --- |
| 被替换角色 | **`baseapp`**（由 boundless-server (crates/server-runtime app::run)） |
| 保留原版角色 | `dbappmgr`, `cellappmgr`, `baseappmgr`, `dbapp`, `cellapp`, `loginapp`, `serviceapp` |
| 原版提交 | `4b637c8d` |
| 新版提交 | `fbb44a7` |
| 捕获时间 | `2026-11-23 03:04:25 +0800` |
| 模式 | `live`（原版集群为他人所有，只读旁听；替换组件本地运行） |
| 原版稳态 | full_steady=true，轮询 0.0s |

## 被替换角色（baseapp）的运行

- `baseapp` 由 `boundless-server baseapp` 启动：接受=True，rc=0，启动行 `[baseapp] starting (boundless minimal entry)`。
- 其余 7 个角色（dbappmgr/cellappmgr/baseappmgr/dbapp/cellapp/loginapp/serviceapp）仍由原版二进制承载，各自到达稳态。

## 替换后果

- **替换后能否保持全栈稳态**：否。（被替换角色接受输入并启动，但只到达进程启动；以替换角色运行的全栈无法维持稳态——新版入口的更深 init 链由 BW-01-04 落地。）
- 记录本身即产出：替换路径、被替换角色、两侧各自到达深度——这正是本条目要求写下来的内容。

## 参考侧逐角色（旁听观测）

| 角色 | 进程在位 | 稳态标记 |
| --- | --- | --- | --- |
| `baseapp` | 是 | 找到 |
| `baseappmgr` | 是 | 找到 |
| `cellapp` | 是 | 找到 |
| `cellappmgr` | 是 | 找到 |
| `dbapp` | 是 | 找到 |
| `dbappmgr` | 是 | 找到 |
| `loginapp` | 是 | 找到 |
| `serviceapp` | 是 | 找到 |
| `baseapp(2)` | 是 | 找到 |

## 可追溯性

- 输入：`reference/bw00_04_05_input.json`（角色集 + `substitute`=`baseapp`）。
- 报告：`reference/bw00_04_05_substitution.json`。

生成：`substitution_run.py` + `substitution_doc.py`。
