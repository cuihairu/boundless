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
