# BW-16-03-01 布置按角色加载的 Python 模块

- **任务编号**: BW-16-03-01（BW-16-03 Python 布局与资源交付，todo.md 行 1882）
- **验收**: 角色之间公开 API 正确隔离。
- **结论**: 满足并勾行。交付由 BW-03 席位的 `crates/python-host` 完成
  （commit `9eb4d42`，`lane/bw03-pyhost`）；本席经 merge `f4f7392` 将该已完车道
  （21/21 勾账、CI 45151494279 绿）并入本 lane，并在本树复测验收全绿。

## 1. 交付面

`create_big_world_module` 按 `component`（角色名）挂每角色方法表——「按角色加载」
的布置即此：`BigWorld` 是原生模块，原版各构建以 `PY_MODULE_METHOD_LINK` 门控
（`MF_SERVER` 等）在 `script.cpp:598-600` 给 `BigWorld` 挂 `component` 属性、
按角色链方法表；端口同序：

| 角色 | 可见面（`dir(BigWorld)` 滤 dunder，恰等） |
|---|---|
| `base` / `cell` / `database` / `bot`（server） | `addFrequentTask, addTimer, component, delTimer, fetchURL, serverMode` |
| `client` / `editor` | `callback, cancelCallback, component, fetchURL` |
| 未知名 / 空名 | `component, fetchURL` |

`fetchURL` 无条件挂（原版 `common/py_urlrequest.cpp:318` 无 `MF_SERVER` 门控）；
`serverMode` 仅 server 组件（原版 `ScriptApp::initScript` `setAttribute`，端口写
默认 `"standalone"`——无配置源，delta 已登记）。

## 2. 隔离的双向检验

`assert_role_visible_surface(interpreter, component)` 每角色两路断言：

1. `manifest::check()` —— manifest 每角色面唯一真源，逐 Ported 行在活模块下
   present/absent 复核（scope 门控即注册门控）。
2. 解释器内以 Python probe 取 `dir(BigWorld)` 非 dunder 名单，与
   `manifest::ported_surface_for_role(component)` **精确相等**——
   在错误门控下注册的面、或 manifest 漏行的面都会在此报红。

角色间交叉泄漏（server 角色见到 `callback`、client 见到 `addTimer`）即测试失败：
「公开 API 正确隔离」由测试常驻钉死。

## 3. 实测（本树）

```
$ cargo test -p python-host --lib
test result: ok. 132 passed; 0 failed; 103 ignored
test interpreter::tests::every_scenario_exits_cleanly ... ok
```

- 8 个角色 scenario（`scenario_the_base_component_sees_the_server_surface` 等，
  base/cell/database/bot/client/editor + 未知/空名）各自 `#[ignore]` 单进程跑，
  由 `every_scenario_exits_cleanly` 一场景一进程驱动，本树全绿。
- scoped fmt 净（`cargo fmt -p platform -p python-host -p resources -p load --check`）；
  `test-support` 层门禁 `layering` 9 测绿。

## 4. 边界

本项验收只考「角色之间公开 API 隔离」。角色 `.py` 脚本树（`res/scripts/` 布置）
与 Demo 原样导入归 BW-16-03-02——FantasyDemo 树未入仓（`examples/fantasydemo/`
目标位，BW-00-05-04 台账 gap 归 BW-14），仍卡；构建/安装两侧 parity 已由
BW-16-03-03（a8758e3）覆盖。
