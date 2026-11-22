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
