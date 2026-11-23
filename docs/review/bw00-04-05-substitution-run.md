# BW-00-04-05 — 单组件替换运行

验收：报告注明被替换角色。

本任务在对照 lane（`ci/fullstack-compare.sh`）中新增**替换单个组件**模式
（`ci/fullstack-compare.sh substitute`）：一次只替换一个角色，其余角色保持
原版不动，并把**被替换角色写进每一行记录与报告**。实体定义检查器是当前
唯一同时有原版/新版实现的角色，替换它时其余角色（mercury 线上字节、
python-host 回调）无可替换的新半——按"原版证据 + 点名缺失"如实记录，
不冒充替换成功。

- 模式入口：`ci/fullstack-compare.sh substitute`（BW-00-04-05）。
- 角色表：`SUBSTITUTIONS="entitydef-checker"`（唯一双半就绪角色）。
- 记录：`$CI_OUT/fullstack/substitution.tsv`，首列为 `role`。
- 接线：`ci.yml` fullstack job 同跑 `run_all` + `substitute` 两种模式。

## 1. 实体定义检查器替换

**被替换角色**：`entitydef-checker`（原版 `tools/process_defs` →
新版 `crates/entitydef` 的 `process_defs`）。其余角色全部保持原版——协议与
回调两角色只有原版证据（BW-00-04-01/02），"保持原版"是唯一可能状态。

输入为仓内三棵 fixture res 树（与 BW-00-04-04 同一输入），替换后运行：

| fixture | 替换结果 | ref sha256 == new sha256 == oracle |
|---------|----------|-------------------------------------|
| full_def | PASS | `cdbc9584e5c32ccc73b59cd0bc5db529425f2a8f9503157c3474a05c5c3d4fa5` |
| service_def | PASS | `f77b6b14d0b70ef22c9d13d65f7c842c429fcd5a110b1bdd106d51964e226efb` |
| entity_index | PASS | `621b37f0c97382dba38cb5bc6e8dbeb6bb42a99caaa338779f55f46923e3ffdc` |

替换后原版证据与新版证据**逐字节相等**（与 BW-00-04-04 的
`ref==new==oracle` 一致）：把实体定义检查器从原版换成新版，产出不变——
替换是透明的，报告逐行注明 `substituted role: entitydef-checker`。

原版二进制在 dump 写盘后 teardown 以 SIGABRT/SIGSEGV 退出（rc 139/134），
同 BW-00-04-04 登记为参考工具行为，按证据字节判定。

## 2. 不可替换角色（点名缺失）

| 角色 | 状态 | 报告 |
|------|------|------|
| mercury-wire | SKIP | `substitution of mercury-wire needs a new wire producer (crates/mercury has no runnable byte entry point)` |
| python-callbacks | SKIP | `substitution of python-callbacks needs a new callback producer (crates/python-host has no ScriptEventList)` |

`SUBSTITUTIONS` 只列双半就绪角色；无可替换新半的角色单独 `ci_skip` 并点名
缺失生产者，不虚勾。

## 3. 自证（self-test）

`ci/fullstack-compare.sh self-test` 新增：
`selftest-fullstack-substitution`——植入错误的"新版实体定义检查器"
（oracle 首字节改写）替换实体定义检查器，断言替换记录首列为
`entitydef-checker` 且判 FAIL。自证通过即证明：报告**确实注明被替换角色**，
且替换对照能看见错误替换。
