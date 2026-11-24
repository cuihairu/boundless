# BW-00-04-02 Python 回调夹具(顺序、参数、可忽略字段)

前置条件变化:原判「依赖 python-host crate 与 python/ 模块未建,随 BW-03 立项解锁」。
BW-03 车道已交付(pyhost,见 lane 记忆),且 BW-00-02-03/02-04 的全量集群+客户端登录
打通了**原版运行中的 Python 回调**采集:本清册从同一次登录运行的三端日志提取回调序列,
连同触发它们的脚本原件入档。夹具目录
`docs/review/data/bw00-04-02-callback-fixtures/live-2026-11-24/`,运行 2026-11-24。

## 1. 夹具清单

| 文件 | 内容 |
| --- | --- |
| `callback-sequence.txt` | 三端(baseapp/cellapp/client)回调序列,带日志行号 |
| `script-base-ClientAvatar.py` | base 侧脚本原件(回调定义与参数) |
| `script-cell-ClientAvatar.py` | cell 侧脚本原件(定时器/客户端方法) |

## 2. 回调序列(实测顺序)

一次客户端登录断连,服务端 Python 回调按以下顺序发生(日志行号为证):

```
baseapp:  Base::Base(3): ClientAvatar                     # base 实体构造
          ClientAvatar.__init__: {'prop1': 1} 0           # __dict__ 与 databaseID
cellapp:  ClientAvatar.__init__: {}                       # cell 实体构造(__dict__ 空)
client:   ServerConnection::logOn: status==LOGGED_ON      # 连接层回调
          ServerConnection::enableEntities                # 触发下一条
baseapp:  ClientAvatar.onEntitiesEnabled: 3               # → createCellEntity
cellapp:  RealEntity::enableWitness: adding witness for 2 # 视锥挂上
cellapp:  (onTimer 1s 一次性 → allClients.fromCell("Hello from the cell"))
cellapp:  (onTimer 周期 → allClients.cellTick(tick))      # 即线抓包里 2s 周期包
client:   (断连,进程退出)
cellapp:  RealEntity::disableWitness: 2
baseapp:  Proxy::onClientDeath: Client id 3 has disconnected (Requested by client)
          ClientAvatar.onClientDeath: 3                   # → destroyCellEntity
          Base::~Base(3): ClientAvatar                    # base 实体析构
```

## 3. 参数与可忽略字段

- `ClientAvatar.__init__`(base):参数仅 `self`;打印的 `{'prop1': 1}` 是
  `__dict__` 快照(默认属性),`0` 是 `databaseID`——本运行经
  `billingSystem/shouldAcceptUnknownUsers=true` 建未知用户,`databaseID=0`
  **属可忽略字段**(合法登录时应非零);`prop1` 为 defs 默认值。
- `ClientAvatar.__init__`(cell):`__dict__` 为空(属性在 C++ 侧,不入 `__dict__`)。
- `onEntitiesEnabled()` / `onClientDeath()` / `logOff()`:无参数,打印 `self.id`。
- `onTimer( timerId, userData )`:两参数;脚本以 `timerId` 区分
  `_timer1`(1.0s 一次性)与 `_timer2`(1.0s 起周期),`userData` 恒 0.0——
  **可忽略字段**。
- `onLoseCell()`:无参,不打印(只 `destroy()`)。
- 客户端方法 `fromCell("Hello from the cell")`(字符串参数)与
  `cellTick(tick)`(递增计数)即 baseapp→客户端 2s 周期 AoI 包的应用层来源
  (与 04-01 线抓包对应)。

## 4. 与源码锚点的对应

- 回调定义:`res/scripts/base/ClientAvatar.py`、`res/scripts/cell/ClientAvatar.py`
  (原件入档);defs 侧 `ClientAvatar.def` 的属性/方法声明(BW-00-05-04 清册)。
- 连接层回调链(`LoginRequest::onTransportConnect`/`handleMessage`/
  `onLoginAppLoginSuccess`/`onBaseAppLoginSuccess`)与 00-02-04 清册的序列一致,
  本夹具增加服务端 Python 侧的对应半边。
- 登记限制:挑战路径回调(`onLogOnFailure`/challenge 工厂)未在成功链路出现,
  失败侧留待有合法凭据拒绝场景时补;`Account.py`/`Guard.py`/`Space.py` 的回调
  未在本序列中触发(Guard 静止、Space 无 space 数据变更),脚本树清单见
  04-03 `resources-sha256.txt`。