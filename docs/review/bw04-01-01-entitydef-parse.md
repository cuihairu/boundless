# BW-04-01-01 解析实体和别名文件 — 复核证据

- **任务编号**: BW-04-01-01（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 350）
- **原版构建目标/源码**: `programming/bigworld/lib/entitydef/`（entity_description.cpp、entity_description_map.cpp、data_type.cpp、data_description.cpp、method_description.cpp、entity_method_descriptions.cpp、member_description.cpp、base_user_data_object_description.cpp）；`programming/bigworld/lib/network/basictypes.{hpp,cpp}`（ExposedMethodMessageRange）；`programming/bigworld/tools/process_defs/main.cpp`（dump 契约）。参考二进制：`game/bin/server/el7/tools/process_defs`（`-m process_defs_dump -f dump`）。
- **Boundless 模块**: `crates/entitydef`（constants / data_type / data_description / member / method / flags / lod / volatile / diagnostics / digest / entity_description / entity_map / dump）+ `crates/entitydef/tests/differential.rs` + `docs/review/tools/process_defs_dump.py`（采集 harness）。
- **平台**: Linux x86_64（el7 参考二进制；Rust 工具链 1.100.0，llvm-cov 0.9.1）。
- **输入夹具**:
  - `/home/cui/workspaces/BigWorld/programming/fantasydemo/res`（完整示例树）
  - `/home/cui/workspaces/BigWorld/programming/bigworld/examples/client_integration/python/simple/res`
  - 单元测试夹具：内存 resolver 的 entities.xml / entity_defs / service_defs / component_defs 迷你树（见 `crates/entitydef/src/*` 内 `#[cfg(test)]`）。
- **原版结果**: 参考二进制 dump 落盘为 JSON oracle：
  - `docs/review/reference/fantasydemo_entitydef.json`（MD5 `18D00DC69AC756433D091F537DA7030E`）
  - `docs/review/reference/client_integration_entitydef.json`（MD5 `2190C88D54E86906A59405B3B3ABF83D`）
  - 内容含实体 ID（加载序）、别名解析结果、缺项错误、属性/方法索引、定义摘要与四个 maxima。
- **新版结果**:
  - `cargo test -p entitydef --offline`：149 个单元测试 + 2 个差分测试全部通过。
  - 差分（`BW04_FD_RES`/`BW04_CI_RES` 指向上述两棵 res 树）：Rust `dump_map` 输出与参考 JSON **逐字节相等**（实体 ID、别名、clientIndex、属性声明序与方法表、exposed 索引、digest、maxima）。
  - 覆盖率：`cargo llvm-cov -p entitydef` 行覆盖 **99.41%**（门槛 ≥98%，llvm-cov 0.9.1）。
  - clippy `-D warnings`：entitydef 0 findings（依赖 crates/resources 存在 32 条既有 findings，属其他席位 lane，按隔离清单 `-A` 放行；crates/foundation 的 2 条已在本次顺带修复）。
- **证据链接**: `docs/review/reference/*.json`（oracle）、`docs/review/tools/process_defs_dump.py`（采集）、`crates/entitydef/tests/differential.rs`（差分）、`crates/entitydef/src/`（实现与测试）。
- **状态**: 已验收（差分逐字节相等；覆盖率与门禁全绿）。

## 备注（登记在案的已知差异 / 待验项）

1. `FIXED_DICT <implementedBy>` 的自定义类路径（digest 中 module/instance 两段）未移植 —— 无 Python 宿主时参考实现同样走空路径；`crates/entitydef/src/data_type.rs` 内有注释登记。
2. `MAILBOX` 类型在 client 构建侧的 `UnsupportedDataType` 分支未移植（server 侧契约不受影响）。
3. `initCompressionTypes`（NetworkCompression）未移植 —— 与压缩流格式相关，属后续 lane。
4. `member.rs` 的错误串统一采用 "Property iface.name" 形态；参考在个别路径使用 "Property iface.name" 之外的形态（不影响解析结果）。
5. `EntityDescription::checkExposedMethods` 的「base 方法带 ALL_CLIENTS」分支无法由 XML 构造（参考解析器对 base 拒绝 ALL_CLIENTS），该分支保持未覆盖；其余角色分支均已测。
6. 覆盖率残余未覆盖行均为 `unreachable!` 臂、登记在案的延迟项或花括号区域伪影。

---

# BW-04-01-02 合并父类和接口 — 复核证据

- **任务编号**: BW-04-01-02（todo.md 行 351）
- **原版构建目标/源码**: `lib/entitydef/entity_description.cpp`（`EntityDescription::parse` 的 `<Parent>` 递归、`parseInterface`）、`lib/entitydef/base_user_data_object_description.cpp`（`parseImplements`：`getDefsDir() + "/interfaces/" + name + ".def"`）、`entity_method_descriptions.cpp`（同名不同签名的 `init` 合并即冲突）、`volatile_info.cpp`（`parse(NULL)` 保留父值）。
- **Boundless 模块**: `crates/entitydef::entity_description`（`parse` / `parse_interface` / `parse_implements`）、`crates/entitydef::volatile`。
- **平台**: Linux x86_64，Rust 1.100.0。
- **输入夹具**: 内存 resolver 迷你树（`Parent.def` + `Child.def` + `interfaces/Combat.def` + `interfaces/Base.def` + `scripts/base/*.py`）。
- **原版结果**（源码核对）:
  - 父类先解析、子类同名属性就地覆盖并**复用原 index**（含 client-server 全索引）；
  - `<Implements>` 从 `entity_defs/interfaces/<name>.def` 加载，其成员先于本实体自身成员并入；
  - 同名方法签名不同时 `addImplementingComponent`/`init` 报 "already defined with a different signature" 并失败；
  - `<Volatile>` 缺省时保留父值（`VolatileInfo::parse(NULL)` 直接返回 true）。
- **新版结果**:
  - `a_conflicting_parent_method_fails_the_child` / `an_interface_method_conflicts_with_the_entity_method`：解析失败并给出上述错误串；
  - `volatile_priorities_inherit_from_the_parent`：父 `<position> 10` → 子继承 100.0（平方规则），子自行声明 `<position> 3` → 9.0；
  - `interface_chains_merge_deep_first`：`Combat` 实现 `Base` → 属性序 `stance, attack, armour`，索引 0/1/2；
  - 差分 oracle（fantasydemo / client_integration）仍逐字节相等 —— 参考树本身含父类与接口继承。
  - `cargo test -p entitydef --offline`：153 单测 + 2 差分通过；clippy 隔离门禁 0 findings；行覆盖 99.42%。
- **证据链接**: `crates/entitydef/src/entity_description.rs`（上述四个测试）、`docs/review/reference/*.json`。
- **状态**: 已验收。

---

# BW-04-01-03 处理 Service 定义 — 复核证据

- **任务编号**: BW-04-01-03（todo.md 行 354）
- **原版构建目标/源码**: `lib/entitydef/entity_description_map.cpp`（`parseServices`、`ServiceDescriptionHasPythonScript`、`scripts/services.xml` 加载与 `activeOnServerModes`）、`lib/entitydef/entity_description.cpp`（`parseService`、`parse` 的 Distribution 读取、`parseInterface` 服务分支只走 `parseImplements`、`parseMethods` 的 `isService_ ? "Methods" : "BaseMethods"`、"Service '%s' does not have a service script."）、`lib/entitydef/constants.hpp`（`services.xml` / `scripts/service_defs` / `scripts/service`）。参考二进制：`game/bin/server/el7/tools/process_defs`。
- **Boundless 模块**: `crates/entitydef::entity_map`（`parse_services`、`ServiceScriptDecider`）、`crates/entitydef::entity_description`（`parse_service`、`parse_interface` 服务分支、`parse_methods` 的 `<Methods>` 分派、`EntityDistribution`）、`crates/entitydef::dump` + `tests/differential.rs`。
- **平台**: Linux x86_64（el7 参考二进制；Rust 1.100.0，llvm-cov 0.9.1）。
- **输入夹具**: `docs/review/fixtures/service_def/res`（自建 res 树：`entities.xml` 两实体 + `services.xml` 两服务 + `service_defs/Cleaner.def`（`<Implements>` 接口链 + 自带 `<Properties>` + `<Methods>` 含 exposed 与双向方法）+ `service_defs/interfaces/Sweep.def` + `service_defs/Repairman.def`（`<Distribution><Base>true` 且无服务脚本）+ `scripts/service/Cleaner.py` 与 base/cell 脚本）。参考树自身无任何 services.xml，故夹具为本项专门构建。
- **原版结果**: 参考二进制对该夹具的 dump 落盘为 oracle `docs/review/reference/service_entitydef.json`（MD5 `012FC1ABBB978756B24344B27AE5B8`）。要点：服务在全部实体之后加载（Cleaner index=2、Repairman=3）；公开方法清单 = `<Implements>` 接口方法在前 + `<Methods>`（vacuum, sweep, tally），`sweep` exposedIndex=0；服务自带 `<Properties>` 被忽略（allProperties 空）；canBeOnCell/canBeOnClient=false、canBeOnBase=true、isPersistent=false、clientIndex=65535；Repairman 靠 `<Base>true` 标签在无脚本时成立；maxima(1,1,0,1)；digest `8A225AFC889A748A5E310F49862A4A43`。
- **新版结果**:
  - 差分测试 `service_dump_matches_the_process_defs_oracle`（夹具在仓内，故无条件执行）与参考 JSON **逐字节相等**；另两棵参考树差分不受影响。
  - 单测 `a_service_lists_its_public_methods_from_the_methods_section` 钉住清单语义（vacuum/sweep/tally、exposed 位、无 cell/client 半边、属性丢弃）。
  - 夹具暴露并修复一处原版对齐缺陷：`EntityDistribution` 标签名原为 `clientTag/baseTag/cellTag`，现对齐参考的 `readBool("Client"/"Base"/"Cell")`，且 `<Distribution>` 存在时三角色全部决定（缺 tag 即 false）——`distribution_tags_override_script_detection`、`resolver_references_forward_and_client_tags_win` 相应更新。
  - `cargo test -p entitydef --offline`：154 单测 + 3 差分通过；clippy 隔离门禁 0 findings；行覆盖 **99.55%**（llvm-cov 0.9.1）。
- **证据链接**: `docs/review/fixtures/service_def/res/`（夹具）、`docs/review/reference/service_entitydef.json`（oracle）、`crates/entitydef/tests/differential.rs`、`crates/entitydef/src/entity_map.rs`、`crates/entitydef/src/entity_description.rs`。
- **状态**: 已验收（服务路径差分逐字节相等；Distribution 标签缺陷已修复）。

---

# BW-04-01-04 分配属性和方法索引 — 复核证据

- **任务编号**: BW-04-01-04（todo.md 行 355）
- **原版构建目标/源码**: `lib/entitydef/entity_description_map.cpp`（`parseInternal` 的两段 ClientServer/ServerOnly 加载与索引盖章、`setExposedMessageIDs`）、`lib/entitydef/entity_description.cpp`（属性 `index`/`clientServerFullIndex` 分配、`EntityMethodDescriptions::init` 的 internal/exposed 索引）、`lib/entitydef/method_description.cpp`（`<Exposed>` 的 component 语义：client 方法使用 `<Exposed>` 即报错；client 方法默认即对 client 公开）、`lib/network/exposed_message_range.hpp`。参考二进制：`game/bin/server/el7/tools/process_defs`。
- **Boundless 模块**: `crates/entitydef::entity_map`（`parse` / `parse_internal` / `set_exposed_message_ids`）、`crates/entitydef::method`（`EntityMethodDescriptions::init`、exposed 排序）、`crates/entitydef::data_description`、`crates/entitydef::dump` + `tests/differential.rs::compare_fixture`。
- **平台**: Linux x86_64（el7 参考二进制；Rust 1.100.0，llvm-cov 0.9.1）。
- **输入夹具**: `docs/review/fixtures/entity_index/res`（`ClientServerEntities` = Alpha、Beta（`<Parent> Alpha`）；`ServerOnlyEntities` = Gamma；Alpha 带 base/cell/client 各半边方法与混合定长/变长参数；Beta 追加 `shield` 属性与 `repair`（带返回值）/`ping` 方法）。
- **原版结果**: 参考二进制对该夹具的 dump 落盘为 oracle `docs/review/reference/entity_index_entitydef.json`（MD5 `6FAD1D7207CFF792E172B34C0C2E1FA5`）。钉住的分配规则：
  - 实体 ID 按加载序：Alpha=0、Beta=1、Gamma=2；server-only 的 clientIndex=65535；
  - 属性 `index` 按并入序（父的 pid/hp/label/spin 0..3，子自有 shield=4）；
  - `clientServerFullIndex` 只给 client-server 属性，且按类型/流长重排：Beta = shield 0 / pid 1 / label 2，其余 -1；
  - base 半边 internal 0..3（`repair` 排在继承方法之后），exposed 重排为 `repair 0 / damage 1 / shout 2`，非 exposed 的 `retire` = -1；
  - cell 半边 `explode 0`、非 exposed `vanish -1`；client 半边 `ping 0 / blink 1 / wave 2`（定长先、变长后）；
  - 四个 maxima：clientServerPropertyCount=3、exposedBase=3、exposedCell=1、exposedClient=3；digest `3F009C3EC546D8F26E7836134C4CC74A`。
- **新版结果**:
  - 差分测试 `entity_index_dump_matches_the_process_defs_oracle` 与参考 JSON **逐字节相等**（该项无需改实现，首轮即绿）；
  - 单测 `a_child_reuses_the_parent_indexes_and_exposed_ids_sort_per_half` 把上述三条索引序列写成可读断言（属性名序与 full index 序列、base/cell/client 三边 exposed 序列、server-only 的 clientIndex）；
  - 顺带钉住参考事实：client 方法不接受 `<Exposed>`（参考报 "Unable to use <Exposed> tag in client method"），故夹具的 client 方法只靠默认公开。
  - `cargo test -p entitydef --offline`：155 单测 + 4 差分通过；clippy 隔离门禁 0 findings；行覆盖 **99.58%**。
- **证据链接**: `docs/review/fixtures/entity_index/res/`、`docs/review/reference/entity_index_entitydef.json`、`crates/entitydef/tests/differential.rs`、`crates/entitydef/src/entity_map.rs`。
- **状态**: 已验收（Base/Cell/Client 三边索引与原版逐字节一致）。
