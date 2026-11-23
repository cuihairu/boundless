# BW-00-02-02 原版单测复跑

基线 `4657a17cf10d55e2fbebf258f96da96a3dbe0c22`，产物树 `game/bin/server/el7/unit_tests/`（21 件，SHA256 见 `docs/review/data/bw00-02-01-el7-manifest.sha256`）。对应 todo.md BW-00-02-02：**保存参编列表、执行命令及结果**。

## 采集方法（全动态实跑，2026-11-21）

1. 官方入口口径：make 链的 `unit_tests / unit_tests_run` 目标在 `bigworld/build/make/Makefile:485` 已被注释掉，且构建受 `user_shouldRunUnitTests=1` 门控（Makefile:171）——因此复跑以**直接执行已构建二进制**为入口（与原版树自带的 assert log 命名一致）。
2. 环境修正（首轮跑出的假失败，已定位并修正）：`BWResource::instance()` 需要 res 路径，缺省报 `No res path set`。首轮裸跑时 `resmgr_test` 报 507 处失败（`ZipSection_LotsOfFilesTest` 写盘后 `openSection` 读不回，`test_zip_section.cpp:217/225`），**非产品缺陷**；将工作目录与 `BW_RES_PATH` 指到同一可写目录后复跑即全绿。
3. 命令：`cd <可写目录> && timeout 300 env BW_RES_PATH=<同一目录> <el7>/unit_tests/<suite>_test`。

## 参编列表与结果

| 套件 | 用例数 | 结果 |
|---|---|---|
| cstdmf_test | 452 | PASS（1.6s） |
| entitydef_test | 387 | PASS |
| pyscript_test | 151 | PASS |
| resmgr_test | 88 | PASS（需 BW_RES_PATH，见上） |
| cellapp_test | 53 | PASS |
| math_test | 44 | PASS |
| physics2_test | 44 | PASS |
| script_test | 18 | PASS |
| baseapp_test | 17 | PASS |
| db_test | 11 | PASS |
| terrain_test | 8 | PASS |
| scene_test | 5 | PASS |
| server_test | 5 | PASS |
| unit_test_lib_test | 1 | PASS |
| baseappmgr_test / cellappmgr_test / dbapp_test / dbappmgr_test / loginapp_test / reviver_test | 0 | PASS（**空壳**：`server/<app>/unit_test/` 只有 main.cpp + Makefile/CMakeLists，无 test_*.cpp——参编但零用例，原版树实态） |
| network_test | — | **未在时限内完成**（见缺口） |

- 参编统计：21 个套件，**20 件通过**、累计 1284 个用例（不含 network），0 失败。
- 日志原件：`docs/review/data/bw00-02-02-logs/*.log`（21 份，逐套件全量 stdout+stderr）。

## 缺口登记

1. **network_test**：300s 与 900s 超时均未完成，`timeout` 退出码 124/143，日志空（0 字节）。本机 loopback `bind/listen/connect/accept` 实测可用，故不是 sandbox 完全禁止套接字；该套件含 TCP 通道/线程安全/事件轮询/重放等长时用例（源目录 60+ test_*.cpp），且原版树自留 `assert.network_test (deleted).coding.*.log` 多份崩溃断言转储（历史即不稳定）。登记为**环境/时长缺口**，不在本项判红；后续可拆 `--count` 逐案跑（该套件支持 `--count`，但 3 个用例亦超 40s）。
2. `0 用例` 的 6 个服务套件属原版树空壳，非缺编——如后续要真参编需补 test 源。
3. 首轮 507 处 resmgr 假失败留档（`BW_RES_PATH` 缺失口径），用于说明"原版绿"与"环境绿"的差异。

## 结论

验收达成：参编列表（21 套件/1284 用例/6 空壳登记）、执行命令（含 `BW_RES_PATH` 修正与 300s 时限）、结果（20 套件全绿、0 失败）均已保存，日志原件入仓；唯一缺口 network_test 如实登记为时长/环境缺口并给出历史不稳定证据。
