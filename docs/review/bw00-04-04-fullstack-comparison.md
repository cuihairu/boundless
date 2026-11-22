# BW-00-04-04 — 原版与新版全栈对照运行

验收：同一输入产生两份证据。

本任务把"同一输入喂给原版全栈与新版全栈，各自留下证据"做成一条可重复
运行的对照 lane（`ci/fullstack-compare.sh`），并把一次真实运行的结果
固化在此。实体定义场景**两份证据都现场产出**：参考 `process_defs` 二进制
与原版同一棵输入 res 树，新版 `process_defs`（`crates/entitydef`）与
同一棵输入 res 树，各自落盘一份 JSON 描述，逐字节相等。协议与回调场景
记录参考侧证据（BW-00-04-01/02 采集的夹具），新版侧登记缺失的生产者
（不虚勾）。

- 对照 lane：`ci/fullstack-compare.sh`（运行 + `self-test` 自证）。
- 接线：`ci/changes.sh` 新增 `fullstack` lane（跟随
  `docs/review/fixtures/*`、`docs/review/reference/*`、
  `ci/fullstack-compare.sh`）；`.github/workflows/ci.yml` 新增
  `fullstack` job 与 harness job 的 `ci/fullstack-compare.sh self-test`。
- 记录：`$CI_OUT/fullstack/{entitydef,protocol,callbacks}.tsv`。
- 运行环境：x86-64 Linux，参考树 `/home/cui/workspaces/BigWorld`。

## 1. 实体定义场景（两份证据现场产出）

输入是仓内提交的三棵 fixture res 树（git 可复现，同一路径喂给两侧）：

| fixture | 输入 res 树 | oracle（BW-04-04-03 采集） |
|---------|-------------|-----------------------------|
| full_def | `docs/review/fixtures/full_def/res` | `docs/review/reference/full_entitydef.json` |
| service_def | `docs/review/fixtures/service_def/res` | `docs/review/reference/service_entitydef.json` |
| entity_index | `docs/review/fixtures/entity_index/res` | `docs/review/reference/entity_index_entitydef.json` |

**原版侧**（参考栈）：`game/bin/server/el7/tools/process_defs`
（`4657a17cf10d55e2fbebf258f96da96a3dbe0c22`）以
`-m process_defs_dump -f dump -p docs/review/tools -r <fixture> -r <RES_ROOT>
--use-stdout <out>` 运行，`-p` 定位 dump 回调模块，第二个 `-r` 提供内嵌
Python 的引导资源（`scripts/common/bw_site.py`、`scripts/common/Lib` 标准库、
`scripts/server_common/lib-dynload-el7` 扩展）。

**新版侧**（Boundless）：`cargo build -p entitydef --bin process_defs` 后以
`process_defs -r <fixture>` 运行（参考兼容选项，dump 写 stdout）。

**结果**（一次真实运行，2026-11-22，逐字节）：

| fixture | ref sha256 | new sha256 | oracle sha256 | 判定 |
|---------|-----------|-----------|---------------|------|
| full_def | `cdbc9584e5c32ccc73b59cd0bc5db529425f2a8f9503157c3474a05c5c3d4fa5` | 同左 | 同左 | PASS |
| service_def | `f77b6b14d0b70ef22c9d13d65f7c842c429fcd5a110b1bdd106d51964e226efb` | 同左 | 同左 | PASS |
| entity_index | `621b37f0c97382dba38cb5bc6e8dbeb6bb42a99caaa338779f55f46923e3ffdc` | 同左 | 同左 | PASS |

三份均为 **ref == new == oracle**（逐字节相等），即原版栈与新版栈在同一
输入上产出同一证据，且与 BW-04-04-03 采集的 oracle 一致（oracle 可在本机
现场复现，非仅引用）。

### 已登记的原版工具行为（非缺陷）

参考 `process_defs` 在 dump 写盘**之后**的解释器 teardown 阶段以
SIGABRT/SIGSEGV 退出（full_def rc=139，service_def/entity_index rc=134），
伴随 `BWTwoWay`/`twisted.internet.defer` 可选模块导入失败与
`PyOutputStream.flush` 的 teardown 报错。证据（JSON）已完整写盘且逐字节
等于 oracle；lane 把该退出码记入 TSV `ref_rc` 列并注明
"reference teardown after evidence (registered artifact)"，**按证据字节判定**
而非按退出码。这是参考工具自身的 teardown 脆弱性，不是对照失败。

## 2. 协议与回调场景（参考侧记录，新版侧登记缺失）

| 场景 | 参考侧证据（BW-00-04-01/02 采集） | 新版侧 | 判定 |
|------|-----------------------------------|--------|------|
| protocol | `docs/review/fixtures/protocol/*.bin`（8 件，含 MANIFEST） | 缺失：mercury 线上字节生产者 | SKIP（点名） |
| callbacks | `docs/review/fixtures/callbacks/callback-trace.txt`（1 件） | 缺失：python-host 回调生产者（无 ScriptEventList） | SKIP（点名） |

`record_reference_only` 把每份参考证据的 sha256 记入 TSV，新版侧在 TSV
写明缺失生产者名（"the mercury wire producer does not exist yet" /
"the python-host callback producer does not exist yet"），不虚勾、不冒充
对照通过。对应生产者随 BW-00-04-05/06 及后续 lane 补齐后，这两个场景转入
双份证据模式。

## 3. 自证（self-test）

`ci/fullstack-compare.sh self-test`：
1. **植入偏差**：把新版侧换成产出损坏 JSON 的 stub（oracle 首字节改写），
   lane 自身断言判 FAIL（"a diverging new half is rejected"）——证明对照
   能看见错误替换，不会静默放行。
2. **干净对**：真实原版/新版对逐字节相等，判 PASS。
3. 植入的 FAIL 只写入 `$SANDBOX/selftest.tsv`（`emit=0`），不污染共享
   manifest，自证结束不留红灯。

## 4. 复用说明

- BW-00-04-05（单组件替换）以本 lane 的"原版栈 vs 新版栈"结构为骨架，
  把被替换角色写成参数；BW-00-04-06（差异报告）消费 `$CI_OUT/fullstack/`
  的两份证据做字段级注入/超时/顺序差异定位。
- protocol/callbacks 场景的新版侧生产者就绪后，把 `record_reference_only`
  替换为与实体定义同构的"双份证据 + 逐字节比较"路径即可，无需改 lane 骨架。
