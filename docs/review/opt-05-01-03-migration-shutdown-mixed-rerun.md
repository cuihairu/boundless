# OPT-05-01-03 复跑迁移、关服和混部

对应 todo.md OPT-05-01-03：**复跑迁移、关服和混部**。**验收** 实体和进程状态正确。

定位：OPT-05-01-01（负载热点基线）/ OPT-05-01-02（调度调优）之后，本行对迁移、关服与混部三条既有面做一次全量复跑，验收=实体状态（mailbox 更新、ghost 与可见集、控制器恢复）与进程状态（关服阶段、清理顺序、混部协议字节）均正确，零失败。

## 复跑清单与结果（2026-11-26，本树 tip `970c650`）

### 迁移面（实体状态正确）

`cargo test -p server-runtime --lib -- migrat offload teleport` → **75/75 绿**，覆盖：

- `entity_creator`：迁移后 cell call 到达新 real、mailbox 地址更新无回调、无 cell/未知 base/None 地址各拒绝臂；
- `ghost`：跨 Space 传送可见集一致、迁移目标失联只 zombie 落单者、同 app 传送替换本地拷贝、onload 删除远端 ghost、offload 切换角色两侧恰一 real、offload 控制器在目的端恢复；
- `offload` / `ghost_routing` / `base_backup`：跨 cell 收集只迁越界 real、路由切换、死亡恢复下一 real、断链保留 home；
- `message_redirect` / `navigation_controller` / `entity_navigate`：切换中调用只到新目标一次、控制器重解析、活动行走迁移保持目标与回调。

### 关服面（进程状态正确）

`cargo test -p server-runtime --lib -- shutdown mixed` → **117/117 绿**（含上列迁移族重叠），覆盖：

- `shutdown`：阶段字符串/线值与参考 C enum 一致、请求/通知/即刻阶段记录、完成队列按序 drain、中途加入等下一 tick、stop_all 收尾；
- `server_app`：control event 走 staged shutdown、init 失败也跑 fini/prepare、shutdown 排空线程任务池；
- `space`：有实体 cell 保留 shutdown timer、空 cell 销毁并回收 space、多 cell 失 own cell 只报不关。

### 混部面（进程状态与 wire 字节正确）

`el7_mixed_deploy.rs` 忽略场景 `the_mixed_deploy_rerun_passes_all_legs` → **1/1 绿**（5 腿全过），环境：

```sh
BW_MACHINED_BIN=/home/cui/workspaces/BigWorld/game/bin/server/el7/tools/bwmachined2
BW_MESSAGE_LOGGER_BIN=/home/cui/workspaces/BigWorld/game/bin/server/el7/tools/message_logger
BW_RES_PATH=/home/cui/workspaces/BigWorld/game/res/bigworld
EL7_MIXED_OUT=/tmp/el7-mixed-output.txt
cargo test -p server-runtime --test el7_mixed_deploy \
  the_mixed_deploy_rerun_passes_all_legs -- --ignored --exact --nocapture
```

落盘证据（`EL7_MIXED_OUT`）：pid 查询 1、整机查询 1，推送路径 `logger/addTCP` + `logger/add` 双版本（2.7 TCP / 2.5 UDP），TCP 注册 22 字节、每记录帧 45 字节×4，UDP 记录帧×3，mldb 三标记串全中，`fabric_registry_entries=3`，协议版本 10——新 Rust fabric 与原版 EL7 二进制双向字节账一致。

### 附加状态面

- `cargo test -p server-runtime --test db_backend_isolation --test duplicate_auth`：4 + 5 全绿（后端隔离、重复登录/logoff 记录唯一）；
- `cargo test -p load --lib -- hurricane fleet`：28/28 绿（hurricane 固定 seed 可复现、fd 预算限速、重启重建同 fleet）；
- `cargo test -p load --test scenarios every_saved_scenario_loads`：1 绿；
- `cargo test -p server-runtime --lib`：**1700 passed / 0 failed / 1 ignored**。

## 结论

迁移（实体状态）、关服（进程阶段与资源回收）、混部（新旧二进制协议字节）三条面全量复跑零失败；对应验收的实体与进程状态均有逐项断言覆盖，本行完成，无代码改动。

## 坑

- `EL7_MIXED_OUT` 必须是**文件路径**，传目录会在落盘时 `IsADirectory` panic（第一次误传 `/tmp/el7-mixed-out` 目录，改为 `/tmp/el7-mixed-output.txt` 后绿）。
