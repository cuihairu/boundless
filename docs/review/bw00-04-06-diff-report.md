# BW-00-04-06 差异报告

> 证据面（BW-00-04-06 验收：注入字段、超时和顺序差异时分别可定位）。

`tools/diff_report.py` 是一个泛型记录序列差异器：回调夹具（`callbacks`）与全栈对照证据（`roles`）都归一化为`{key, order, value, ignorable, timed_out}`，因此一份差异器即可定位三类差异：

- `field`——记录的 `value` 不同且未标记可忽略；
- `ignored`——`value` 不同但记录标记为可忽略（两次合法运行应只差这些）；
- `timeout`——一侧超时（`timed_out` 不同）；
- `ordering`——记录双方都在但相对顺序不同。

每条差异携带记录 `key`，可逐条定位。

## 注入测试（验收）

以 BW-00-04-02 真实回调夹具为基线，每次只注入一类差异，断言差异器**恰好**定位该类、其余两类为 0。

| 注入 | 定位 field | 定位 timeout | 定位 ordering | 结果 |
| --- | --- | --- | --- | --- |
| field | 1 | 0 | 0 | 通过 |
| timeout | 0 | 1 | 0 | 通过 |
| ordering | 0 | 0 | 2 | 通过 |

三项注入全部通过：`all_ok=true`。

## 注入明细

### 注入 `field`

- `field`：`Proxy::addCreateBasePlayerToChannelBundle`，`'Creating player on client'` → `'Creating player on client (edited)'`（仅值不同，非可忽略）。

### 注入 `timeout`

- `timeout`：`ClientAvatar.__init__`，ref 超时=False，new 超时=True。

### 注入 `ordering`

- `ordering`：两回调互换位置，报告：
  - `Proxy::addCreateBasePlayerToChannelBundle`：ref#4 → new#3。
  - `Proxy::enableEntities`：ref#3 → new#4。

## 真实对照上的演示

把 BW-00-04-04 的原版证据与新版证据送入差异器（`reference/bw00_04_06_comparison_diff.json`）：

| 角色 | 原版 | 新版 |
| --- | --- | --- |
| `baseapp` | `running` | `started` |
| `baseappmgr` | `running` | `started` |
| `cellapp` | `running` | `started` |
| `cellappmgr` | `running` | `started` |
| `dbapp` | `running` | `started` |
| `dbappmgr` | `running` | `started` |
| `loginapp` | `running` | `started` |
| `serviceapp` | `running` | `rejected` |

7 个角色 `running` → `started`（深度差，预期内）；`serviceapp` `running` → `rejected`（新版尚不认识该角色）——差异器按角色逐条定位，不混淆。

## 可追溯性

- 注入测试：`reference/bw00_04_06_diff_selftest.json`（基线 `docs/review/reference/bw00_04_02_callback_fixtures.json`）。
- 演示：`reference/bw00_04_06_comparison_diff.json`。

生成：`diff_report.py` + `diff_report_selftest.py` + `diff_report_doc.py`。
