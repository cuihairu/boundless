<<<<<<< HEAD
# BW-00-04-02 — 采集 Python 回调夹具

验收：保留顺序、参数与可忽略字段。

本任务把参考栈调用 Python 回调时的**顺序、参数与返回值**固化为基准样本。
回调机制用的是参考栈自身的 `ScriptEventList` / `ScriptEvents`
（`lib/pyscript`）：注册的 Python 监听器按 level 顺序（同 level 稳定保持
插入序）被触发，每个监听器收到触发时的参数元组，返回值落入 results 列表。
全部监听器是参考内嵌 Python 3.13 执行的真正 Python 可调用对象，采集到的
顺序/参数/返回即原版行为，不是重实现。

- 参考栈：BigWorld 4657a17，`programming/bigworld` 预编译静态库
  （`build/cmake-entitydef/libscript.a`、`libpyscript.a`、内嵌
  `bwpython313/libpython3.13.a`），x86-64 Linux。
- 采集工具：`docs/review/tools/capture_callbacks.cpp`，编译命令见文件头注释
  （`--root` 传给 `BaseResMgrUnitTestHarness`，与参考单元测试一致）。
- 原始输出：`docs/review/data/bw00-04-02-logs/capture.txt`。
- 基准样本：`docs/review/fixtures/callbacks/callback-trace.txt`。
- 确定性：两次运行 stdout 完全一致。

## 场景

三个监听器注册到同一个 `ScriptEventList`：

| 监听器 | level | 行为 |
|--------|-------|------|
| a | -1 | 记录 `('a', arg)`，返回 `'A-return'` |
| b | 0 | 记录 `('b', arg)`，返回 `None` |
| c | 0 | 记录 `('c', arg)`，返回 `arg`（回显） |

b、c 同 level 0，c 后注册，稳定序为 a(-1) → b(0) → c(0)。

## 各用例

### int-arg — `triggerEvent((42,))`

- **顺序** `a b c`（level 序，同 level 稳定插入序）。
- **参数**：每个监听器收到 `42`。
- **返回**：`['A-return', None, 42]`（b 返回 None，c 回显参数）。

### str-arg — `triggerEvent(('hello',))`

- 顺序 `a b c`；参数 `'hello'`；返回 `['A-return', None, 'hello']`。

### tuple-arg — `triggerEvent(((1, 2, 3),))`

- 顺序 `a b c`；参数 `(1, 2, 3)`；返回 `['A-return', None, (1, 2, 3)]`。

### dict-arg — `triggerEvent(({'k': 7},))`

- 顺序 `a b c`；参数 `{'k': 7}`；返回 `['A-return', None, {'k': 7}]`。

### none-arg — `triggerEvent((None,))`

- 顺序 `a b c`；参数 `None`；返回 `['A-return', None, None]`。

### timestamp-arg-IGNORABLE — `triggerEvent(({'ts': <float>},))`

- 顺序 `a b c`；参数 `{'ts': 0.123456789}`；返回
  `['A-return', None, {'ts': 0.123456789}]`。
- **可忽略字段**：`ts` 值是运行时浮点（此处为固定注入值，真实运行随时间
  变化）。差异报告须把 `ts` 标记为可忽略（IGNORABLE），不参与比较——
  事件名带 `-IGNORABLE` 后缀，样本中以 `<float>` 占位。

## 不变式（差异报告应断言的）

1. **顺序**：任何一次运行，监听器触发起序均为 `a b c`，不得颠倒。
2. **参数**：每个监听器收到的参数等于触发时传入的参数（按位置）。
3. **返回**：results 与监听器一一对应，位置一致（`None` 表示无返回值或
   抛异常）。
4. **可忽略**：`ts` 字段不比较。
=======
# BW-00-04-02 Python 回调夹具

> 证据面（BW-00-04-02 验收：保留顺序、参数与可忽略字段）。

由 `tools/python_callback_fixture.py` 从真实登录捕获（BW-00-02-04，`reference/bw00_02_04_client_login.json`）派生，不再重复拉起集群。源数据 `reference/bw00_04_02_callback_fixtures.json`。

| 项 | 值 |
| --- | --- |
| 参考仓提交 | `f3f53898` |
| 捕获时间 | `2026-11-23 01:46:38 +0800` |
| 登录命令 | `/home/cui/workspaces/BigWorld/game/bin/server/el7/examples/simple_python_client -server 127.0.0.1 -user MyAccount -password a -autoDisconnect` |
| 实体 | proxy_id `4805`，客户端源 `192.168.5.188:55283` |
| 回调数 | 12 |
| 可忽略字段 | `entity_id`, `source_port`, `address` |

## 回调序列（顺序保留）

`order` 是合并客户端与两个服务端日志后的全局序号，跨站点不重排：客户端请求/应答在前，随后 BaseApp 回调，最后 CellApp 回调——与捕获记录的先后一致（登录 → 使能实体 → 客户端断开 → 实体销毁）。

| 序 | 站点 | 回调 | 参数 | 可忽略 |
| --- | --- | --- | --- | --- |
| 1 | 客户端 | `LoginHandler::onLoginAppLoginSuccess` | `BaseApp@192.168.5.188:51079 over UDP (# requests: 10, retry interval:1.0s, timeout interval: 8.0s)` | address |
| 2 | 客户端 | `LoginHandler::onBaseAppLoginSuccess` | `Got reply from BaseApp channel udp:192.168.5.188:51079` | — |
| 3 | BaseApp | `LoginHandler::login` | `4805 attached from 192.168.5.188:55283. Attempt 1` | entity_id, source_port |
| 4 | BaseApp | `Proxy::enableEntities` | `4805` | entity_id |
| 5 | BaseApp | `Proxy::addCreateBasePlayerToChannelBundle` | `Creating player on client` | — |
| 6 | BaseApp | `ClientAvatar.onEntitiesEnabled` | `4805` | entity_id |
| 7 | BaseApp | `Proxy::onClientDeath` | `Client id 4805 has disconnected (Requested by client)` | entity_id |
| 8 | BaseApp | `BaseApp::removeProxy` | `Removing proxy id 4805 addr udp:192.168.5.188:55283` | entity_id, source_port |
| 9 | BaseApp | `ClientAvatar.onClientDeath` | `4805` | entity_id |
| 10 | BaseApp | `Base::~Base` | `ClientAvatar` | — |
| 11 | CellApp | `ClientAvatar.__init__` | `{}` | — |
| 12 | CellApp | `Cell::entityDestroyed` | `ClientAvatar 4805 has been destroyed.` | entity_id |

## 参数拆分

每条回调按名称（脚本方法或引擎入口点）与参数（余下文本）拆分，使差异工具可逐字段比较；原始行保留在 `raw`。两类拆分规则：

- `name(4805)` 形式：圆括号内是名称自身的实参表，参数取 `):` 之后的意义文本（如 `Proxy::enableEntities(4805)` → 参数 `4805`）。
- `name: detail` 形式：冒号后为参数（如 `ClientAvatar.onEntitiesEnabled: 4805` → 参数 `4805`）。

## 可忽略字段

两次运行同一登录流程，合法地只差三类字段；夹具按实体记录哪些字段应被忽略，使两次运行的差异恰好忽略这些、不再多忽略。

- `entity_id`：每次运行分配的实体 ID 不同（本夹具为 proxy_id 4805）。
- `source_port`：客户端 UDP 源端口随运行变化（本夹具为 55283）。
- `address`：回复中命名的跳转地址（BaseApp@<addr>:<port>）随监听端口变化。

## 可追溯性

- 源捕获：`reference/bw00_02_04_client_login.json`（BW-00-02-04，同一 `proxy_id` 与客户端源端口）。
- 夹具：`reference/bw00_04_02_callback_fixtures.json`。

生成：`python_callback_fixture.py` + `python_callback_fixture_doc.py`。
>>>>>>> origin/lane/bw19-docs
