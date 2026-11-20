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

---

# BW-04-01-05 计算定义摘要 — 复核证据

- **任务编号**: BW-04-01-05（todo.md 行 357）
- **原版构建目标/源码**: `lib/entitydef/entity_description_map.cpp`（`parse` 尾部 `MD5 md5; addToMD5( md5 ); md5.getDigest( digest_ )`；`EntityDescriptionMap::addToMD5` 只把 `isClientType()` 的描述按加载序喂进 MD5）、`lib/entitydef/entity_description.cpp`（逐实体的 `addToMD5`）、`lib/cstdmf/md5.cpp`；`tools/process_defs` 的 `constants.digest`。参考二进制：`game/bin/server/el7/tools/process_defs`。
- **Boundless 模块**: `crates/entitydef::entity_map`（`digest` / `add_to_md5`）、`crates/entitydef::digest`（纯 Rust MD5 + 十六进制大写 quote）、`crates/entitydef::dump`（`constants.digest` 字段）、`crates/entitydef/tests/differential.rs`。
- **平台**: Linux x86_64（el7 参考二进制；Rust 1.100.0，llvm-cov 0.9.1）。
- **输入夹具**: 四棵 res 树 —— `fantasydemo/res`、`client_integration/.../res`（参考树）、`docs/review/fixtures/service_def/res`、`docs/review/fixtures/entity_index/res`（自建）。
- **原版结果**: 参考二进制 dump 的 `constants.digest`：`service_def` = `8A225AFC889A748A5E310F49862A4A43`、`entity_index` = `3F009C3EC546D8F26E7836134C4CC74A`；fantasydemo / client_integration 两份历史 oracle（MD5 `18D00DC6…` / `2190C88D…`）同样钉住该字段。
- **新版结果**:
  - 四个差分测试与参考 JSON **逐字节相等**，含 `constants.digest` 字段（同输入 → 同字节）；
  - 新测 `the_digest_is_deterministic_and_matches_the_reference_oracles`：同一夹具两次独立解析的 digest 相等，且等于上述参考常量；
  - 既有 `digest_covers_client_types_in_vector_order_only` 钉住"只计入 client 类型、按加载序"的原版规则；
  - `cargo test -p entitydef --offline`：156 单测 + 4 差分通过；clippy 隔离门禁 0 findings；行覆盖 **99.58%**。
- **证据链接**: `docs/review/reference/*.json`（四份 oracle）、`crates/entitydef/src/entity_map.rs`、`crates/entitydef/src/digest.rs`、`crates/entitydef/tests/differential.rs`。
- **状态**: 已验收（四份 oracle 的 digest 字段与原版逐字节一致）。

---

# BW-04-02-01 实现标量与默认值转换 — 复核证据

- **任务编号**: BW-04-02-01（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 365）
- **原版源码**: `lib/entitydef/data_types/integer_data_type.cpp`（`IntegerDataType::isSameType` 截断闸门、`setDefaultValue`）、`long_integer_data_type.cpp`、`float_data_types.cpp`、`string_data_type.cpp`、`unicode_string_data_type.cpp`、`blob_data_type.cpp`、`vector_data_types.{hpp,cpp}`（`SimpleStreamElement<VECTOR>`）、`simple_stream_element.hpp`、`lib/entitydef/data_type.cpp`（`buildDataType` 末尾 `setDefaultValue(pSection->findChild("Default"))`）、`lib/entitydef/data_description.cpp`（`DataDescription::pInitialValue` 三槽顺序）。
- **Boundless 模块**: `crates/entitydef::value`（新增：`PropertyValue` / `PropertyError` / `check_integer` / `truncate_integer` / `integer_bits` / `check_value` / `value_from_section` / `default_value` / `add_to_stream` / `create_from_stream` / `write_int` / `read_integer`），`DataType::{check_value, default_value, initial_value, value_from_section, add_to_stream, create_from_stream}` 与 `DataDescription::initial_value` 委托方法；`Kind::meta_name()` 改为 `DataType::meta_name()` 与 `value.rs` 共用。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**171 单元测试 + 4 差分测试**全部通过。
  - 数值边界：`integer_boundaries_match_the_reference_check` 钉住 `Script_intOverflow`（`pyscript/unit_test/test_conversion.cpp:54`）的四个边界 —— INT32 收 `0x7fffffff` 与 `-0x80000000`、拒 `0x80000000` 与 `-0x80000001`；UINT32 收 `0..0xffffffff`、拒负数。截断算法与 `IntegerDataType::isSameType` 的 `INT_TYPE value = INT_TYPE( intValue ); if (intValue != NativeLong( value ))` 逐句对应。
  - 异常对照：`error_text_matches_the_reference_messages` 把四条参考错误模板逐字节钉住（含结尾 `\n`）：`IntegerDataType::isSameType: {value} is out of range (truncated = {truncated}).`、`SimpleStreamElement: Not enough data on stream to read value`、`BlobStreamElement: Not enough data on stream to read value`、`UnicodeStringStreamElement::fromStreamToSink: Could not convert from UTF8.`。
  - 默认值顺序：`type_defaults_follow_the_reference_order` + `a_property_initial_value_follows_the_reference_order` 钉住 `pInitialValue` 的三槽顺序（`pInitialValue_` → `pDefaultSection_` 经 `createFromSection` → `getDefaultValue`），以及 `buildDataType` 对 `<Type>` 自带 `<Default>` 子节的 `setDefaultValue` 语义。
  - 字节布局：`scalar_round_trip_is_byte_exact` 覆盖 INT8/16/32/64、UINT8/16/32/64、FLOAT32/64、STRING、UNICODE_STRING、BLOB、VECTOR2/3/4 的写读往返，`add_to_stream`/`create_from_stream` 字节一致；有符号读回按 `INT_TYPE` 位宽符号扩展。
  - 16 MiB 上限：`an_overlong_scalar_reports_the_stream_element_message` 验证 `MAX_PACKED_INT = 16MiB-1` 的长度前缀上限触发参考的 stream element 报错。
  - clippy `-D warnings`：entitydef **0 findings**（依赖 `crates/resources` 的 32 条既有 findings 属其他席位 lane，按隔离清单 `-A` 放行）。
  - rustfmt：`crates/entitydef/src/value.rs`、`data_type.rs` 干净（仅对本次触碰文件检查，未做全包 fmt）。
  - 覆盖率：crate 行覆盖 **99.00%** 区域 / 行覆盖合计；`value.rs` 单文件 **99.09%** 行（门槛 ≥98%）。
- **证据链接**: `crates/entitydef/src/value.rs`（实现与全部单测）、`crates/entitydef/src/data_type.rs`（`Kind::meta_name`）、`crates/foundation/src/stream.rs`（`BinaryStream` 原语与 `MAX_PACKED_INT`）。
- **状态**: 已交付（源码级对齐 + 精确错误串/边界锚定）。

## 备注（登记在案的差异 / 待验项）

1. **无字节级参考 oracle**：本族没有可直接比对的参考二进制产物 —— `tools/process_defs` 不遍历属性流，本机构建内也没有带 `EntityDescription` 扩展的解释器能吐出属性字节。故证据为「参考 C++ 源码逐句对齐 + 错误串/边界/往返测试锚定」，不是「与参考二进制逐字节相等」。此点不虚勾，留待 BW-04-02-02..07 若有夹具再补字节级对照。
2. `PropertyError::NotUtf8`（`UnicodeStringDataType::createFromSection` 的 "Could not convert from UTF8."）在本实现中**不可达**：Rust 侧 XML 解析器（`datasection.rs`）用 `String::from_utf8_lossy` 先做有损归一，非法 UTF-8 无法在 `DataSection` 中表示。该错误通道按参考保留命名，但无测试覆盖。
3. `value_from_section` 中 `Kind::Vector` 的 `_ => unreachable!("vector element count")` 是唯一未覆盖行（99.09% 的残余）：解析器只会产生 2/3/4 分量，属解析器不变式。
4. 无符号整数从流中读回保持零扩展（不符号扩展），有符号宽度才符号扩展 —— 与参考把 `stream >> value` 读入 `INT_TYPE` 的行为一致。
5. `PropertyValue` 没有 bool 变体：BigWorld entitydef 的标量 meta 类型中不存在 BOOL（`data_type.cpp` 的类型表无此项）。

---

# BW-04-02-02 实现 ARRAY/TUPLE — 复核证据

- **任务编号**: BW-04-02-02（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 366）
- **原版源码**: `lib/entitydef/data_types/sequence_data_type.cpp`（`SequenceDataType::isSameType` / `createDefaultValue` / `createFromSection` / `streamSize` / `typeName`，以及 `SequenceStartStreamElement::fromSourceToStream`（长度前缀 + 定长校验）、`SequenceStartStreamElement::fromStreamToSink`（读前缀 + **先于分配**的恶意长度闸门）、`SequenceStreamElement` / `SequenceItemStreamElement`）、`array_data_type.{hpp,cpp}`（`isConst = isConst && element->isConst()`）、`tuple_data_type.cpp`（`startSequence` 传 `isConst=true`）、`sequence_meta_data_type.cpp:69,73`（meta 名 `ARRAY` / `TUPLE`）、`data_type.cpp:422`（`DataType::typeName()` 基类只返回 meta 名）。
- **Boundless 模块**: `crates/entitydef::value`（`PropertyValue::Sequence`、六个新 `PropertyError` 变体 + 各自逐字节复刻的 `Display` 文案、`check_value` / `value_from_section` / `default_value` / `add_to_stream` / `create_from_stream` 的 `Kind::Sequence` 臂、`check_sequence_capacity`）。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**181 单元测试 + 4 差分测试**全部通过。
  - **定长序列不写长度前缀**：`a_fixed_sequence_rejects_a_value_of_the_wrong_length` 与 `a_sequence_rejects_a_length_that_does_not_fit_the_stream` 一起钉住 `SequenceStartStreamElement::fromSourceToStream` 的分派 —— `size != 0` 时不写任何字节（参考仅在与 `varSize_` 不符时报错并改由数据类型定长），`size == 0` 时写一个 packed int 计数。`TUPLE size 2 of INT8` 的 `[1,2]` 恰好是 `[1, 2]` 两字节；变长 `ARRAY of INT8` 同值则是 `[2, 1, 2]`。
  - **嵌套逐字节一致**：`a_nested_sequence_round_trips_byte_exact` 钉住「变长 `ARRAY` of 定长 `TUPLE`」的完整线格式 `[2, 1, 0, 2, 0, 3, 0, 4, 0]`（外层计数 2，两层各写自己的前缀，INT16 小端）；`a_variable_element_sequence_round_trips_byte_exact` 钉住「变长 `ARRAY` of `STRING`」的 `[2, 1, 'a', 2, 'b', 'c']`。
  - **默认值**：`a_sequence_default_has_one_default_element_per_slot` 钉住 `createDefaultValue` 用 `size_` —— 变长（`size_==0`）默认**空**序列，定长 N 默认 N 个元素默认值。
  - **节读取**：`a_sequence_reads_its_item_sections_and_fills_the_gaps` 钉住 `createFromSection` 的 `size = countChildren()`（`size_ != 0` 时改取 `size_`），以及**尾部缺失的 item 取元素默认值**。
  - **类型校验**：`a_sequence_type_rejects_a_wrong_length_or_element` 钉住 `isSameType` 先查长度（`Wrong size %d instead of %d.`）再逐元素查，且报错文案中的类型名是**元素自己的** `typeName()`（基类 `DataType::typeName()` = meta 名 `INT8`），不是序列的 `ARRAY size 2 of (INT8)` 形态 —— 参考 `sequence_data_type.cpp:324` 调的是 `elementType_.typeName()`。
  - **分配前闸门**：`a_sequence_rejects_a_length_that_does_not_fit_the_stream` 钉住两条参考报错的**可达方向**（先检查、后分配）：变长元素 → `Invalid size on stream: 3 (0 bytes remaining)`，定长元素 → `Expected bytes on stream: 100 (2 bytes remaining)`（打印 `varSize_ * elementSize`）。
  - **参考源码的复制粘贴瑕疵如实复刻**：参考在**读**路径（`fromStreamToSink`）里三条报错全部误印成 `SequenceStartStreamElement::fromSourceToStream`。本实现逐字保留该瑕疵，并在 `PropertyError` 变体文档与本文件备注中登记。
  - `error_text_matches_the_reference_messages` 追加六个序列模板的逐字节锚定（含结尾 `\n`）。
  - clippy `-D warnings`：entitydef **0 findings**（依赖 `crates/resources` 的 32 条既有 findings 属其他席位 lane，按隔离清单 `-A` 放行）。
  - rustfmt：`value.rs` / `data_type.rs` 干净（仅对本次触碰文件检查）。
  - 覆盖率：`value.rs` **99.06%** 行、crate 合计 **99.46%**（门槛 ≥98%）。
- **证据链接**: `crates/entitydef/src/value.rs`（实现与全部单测）、`crates/entitydef/src/data_type.rs`（`Kind::Sequence`、`stream_size`、`type_name`）。
- **状态**: 已交付（源码级对齐 + 线格式/错误串/边界锚定）。

## 备注（登记在案的差异 / 待验项）

1. **仍无字节级参考 oracle**（同 BW-04-02-01 备注 1）：本项的「逐字节」证据是**与参考源码推出的线格式一致的自钉测试**，不是与参考二进制产物的比对。`TUPLE` 与 `ARRAY` 在参考里经 `beginTuple` / `beginArray` 两条路径，但两者线格式相同，故本实现合为一个 `Sequence` 变体 —— 这一点在 `Kind::Sequence { is_tuple }` 的注释中登记。
2. `check_sequence_capacity` 中 `remaining < 0` 分支在实现内不可达：`BinaryStream::remaining()` 为 `buf.len() - read`，`read` 在截断时被钳制到 `buf.len()`，恒非负。该判断保留是为了与参考的 `remaining < 0` 防御同形。
3. `value.rs` 残余未覆盖 3 行：`349` 是解析器不变式（同 BW-04-02-01 备注 3）；`521`/`522` 是定长/变长序列写路径上 `write_packed_int` 的 `map_err` 闭包 —— 只在 packed 计数超过 `MAX_PACKED_INT`（16 MiB）时触发，即需要 >1600 万个元素，单元测试不可行。
4. 元素 `streamSize() == 0` 时参考两条闸门都不取（`if (elementSize < 0)` / `else if (elementSize > 0)` 皆假），故直接通过。本实现保留同一空臂，并以此为由**不**把该情况当作非法。

---

# BW-04-02-03 实现 FIXED_DICT — 复核证据

- **任务编号**: BW-04-02-03（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 368）
- **原版源码**: `lib/entitydef/data_types/fixed_dict_data_type.cpp` —— 构造器 424（`streamSize_ = allowNone_ ? -1 : 0` 后逐字段累加，字段为 `-1` 即整体 `-1`）、`isSameType` 611、`getDefaultValue` 726、`streamSize` 769、`addToSection` 787、`createFromSection` 840、`getStreamElement` 1201（`index 0` = start，`index 2k+1` = `FixedDictEnterFieldStreamElement`，`index 2k+2` = `FixedDictLeaveFieldStreamElement`，`index size*2+1` = end）、四个 StreamElement 类 29/146/251/295/320；`lib/entitydef/data_types/class_data_type.cpp:588`（`Field::createDefaultValue`：先在本类型的 `<Default>` 里按字段名找子节，找不到才用字段类型默认值，失败报 "ClassDataType::Field::createDefaultValue: Failed to create %s from default section\n"）。
- **Boundless 模块**: `crates/entitydef::value`（`PropertyValue::FixedDict(Option<Vec<(String, PropertyValue)>>)`；`PropertyError::{NullSection, MissingNoneIndicator, FieldDefaultFailed}` 及各自逐字节复刻的 `Display`；`check_value` / `value_from_section` / `default_value` / `add_to_stream` / `create_from_stream` 五个 `Kind::FixedDict` 臂；`value_from_section` 新增「本类型的 `<Default>` 节」参数以支持缺字段的默认值查找）。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**190 单元测试 + 4 差分测试**全部通过。
  - **字段次序**：`a_fixed_dict_round_trips_its_fields_in_order` 钉住无 `<AllowNone>` 时的线格式 —— 无指示字节，字段按类型自身顺序直排（`UINT8 7` + `STRING "x"` = `[7, 1, 'x']`）；`FixedDictEnterField/LeaveField/EndStreamElement` 三者均不写字节（参考 `fromSourceToStream` 只做 `enterField`/`leaveField`/`return true`）。
  - **None 指示字节**：`an_allow_none_fixed_dict_writes_its_none_indicator` 钉住 `uint8( isNone_ ? 0 : 1 )` —— `None` 写 `[0]` 且到此为止，有值写 `[1]` 再接字段。
  - **缺省行为**：`a_fixed_dict_default_depends_on_allow_none` 钉住 `getDefaultValue` 的双分支（`allowNone` → `writeNone(true)` 即 None；否则逐字段一个默认值，按字段序）；`a_fixed_dict_reads_its_field_sections_and_fills_the_gaps` 钉住 `createFromSection` 按字段名开子节、缺失字段取字段类型默认值；`an_empty_section_is_the_none_value_of_an_allow_none_dict` 钉住 `countChildren() == 0` 即 None。
  - **`Field::createDefaultValue` 的查找顺序**：`a_failed_field_default_reports_the_reference_message` 钉住「缺失字段先在本类型 `<Default>` 里按字段名找子节，找不到才用字段类型默认值」，并在该子节构建失败时报参考的错误串。
  - **类型校验**：`a_fixed_dict_rejects_a_wrong_field_set` 钉住 `isSameType` 的静默失败（字段数不符、字段名不符、字段值不过字段类型校验、非字典值、以及 `<AllowNone>` 为假时给 None）；`a_fixed_dict_allowing_none_accepts_the_none_value` 钉住 `allowNone && isNone` 直通。
  - **错误串**：`error_text_matches_the_reference_messages` 追加三条 FIXED_DICT 模板的逐字节锚定（含结尾 `\n`），其中 `Missing None/values indicator` 一条钉住参考源码中两段相邻字面量的拼接形态（`fromStreamToSink` 后**无**冒号）。
  - clippy `-D warnings`：entitydef **0 findings**（依赖 `crates/resources` 的 32 条既有 findings 属其他席位 lane，按隔离清单 `-A` 放行）。
  - rustfmt：`value.rs` 干净（仅对本次触碰文件检查）。
  - 覆盖率：`value.rs` **99.27%** 行、crate 合计 **99.47%**（门槛 ≥98%）。
- **证据链接**: `crates/entitydef/src/value.rs`（实现与全部单测）。
- **状态**: 已交付（源码级对齐 + 线格式/错误串/缺省行为锚定）。

## 备注（登记在案的差异 / 待验项）

1. **仍无字节级参考 oracle**（同 BW-04-02-01/02 备注 1）：本项的「逐字节」证据是**与参考源码推出的线格式一致的自钉测试**，不是与参考二进制产物的比对。
2. `PropertyError::NullSection`（`FixedDictDataType::createFromSection: Section is NULL.`）在本实现中**不可达**：`value_from_section` 取 `&DataSection` 引用，无法为 null。该错误通道按参考保留命名与文案，仅测试其 `Display`。
3. 参考 `getStreamElement` 的 `isPersistentOnly` 分支（`getNumPersistentFields` / `getFieldIndexForPersistentFieldIndex` 的持久化投影）属 **BW-04-02-07**，本项未实现；本项的 `stream_size` 已含持久化口径（`allowNone` → `-1`）。
4. 自定义类路径（`<implementedBy>` + `FixedDictStreamedStreamElement` 的 `readCustomType`/`writeCustomType`）未移植 —— 无 Python 宿主，参考在同构建下同样走空路径。
5. `value.rs` 残余未覆盖 3 行：`405` 是解析器不变式（同 BW-04-02-01 备注 3）；`634`/`635` 是变长序列 `write_packed_int` 的 `map_err` 闭包 —— 只在 packed 计数超过 `MAX_PACKED_INT`（16 MiB）时触发，即需要 >1600 万个元素，单元测试不可行（同 BW-04-02-02 备注 3）。

---

# BW-04-02-04 实现 USER_TYPE 与 MAILBOX — 复核证据

- **任务编号**: BW-04-02-04（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 369；验收：转换及序列化夹具一致）
- **原版源码**: `lib/entitydef/data_types/user_data_type.cpp` 60-435 —— `isSameType` 返回 `pObject() != NULL`（无 Python 宿主即静默假）；五个上报点：`addObjectToStream`（先 `stream << ""` 再报错）、`createObjectFromStream`（先读长度前缀字符串再报错）、`createObjectFromSection`、`addObjectToSection`、`pDefaultValue`，文案统一 `"UserDataType::<site>: Do not have %s.%s\n"`；`streamSize()` 恒 `-1`。`lib/entitydef/data_types/mailbox_data_type.cpp` 全篇 —— `EntityMailBoxRef` 12 字节（`network/basictypes.hpp`：`int32 id` + `uint32 ip` + `uint16 port` + `uint16 salt`）；`isSameType` 对 ip==0 报 WARNING `"MailBoxDataType::isSameType: Mailbox for entity %d has not been fully initialised yet (IP address is zero)\n"` 并返回假；`setDefaultValue` 报 ERROR `"MailBoxDataType::setDefaultValue: Default value for mailbox not supported\n"` 但类型照常构建，且不落 `pDefaultValue_`（不调基类）；`getDefaultValue` 返回全零 ref（`Address::NONE`）；`createFromSection` 读 `id`/`ip`/`etc` 三个具名子节，`etc = port | (salt << 16)`；`clientSafety()` = CLIENT_UNUSABLE；`addToMD5("MailBox")`。
- **Boundless 模块**: `crates/entitydef::value`（`PropertyValue::MailBox { id, ip, port, salt }`；`PropertyError::{MissingConverter { site, module, instance }, UninitialisedMailBox { id }}` 及 `Display`；`check_value` / `value_from_section` / `default_value` / `add_to_stream` / `create_from_stream` 的 `Kind::UserType` 与 `Kind::MailBox` 臂）；`crates/entitydef/src/data_type.rs`（`build_type` 尾部：MAILBOX 拒绝 `<Default>` 子节但类型照常构建且不落 `default_section`）。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**197 单元测试 + 4 差分测试**全部通过。
  - **USER_TYPE 无宿主的五个语义**：`a_user_type_reports_the_missing_converter_at_every_site` 钉住 `check_value` 静默 `TypeMismatch`（参考 `isSameType` 只返回假不打印）、`value_from_section` 报 `createObjectFromSection` 站点、`add_to_stream` **先写空字符串**（packed int 0，字节 `[0]`）再报 `addObjectToStream` 站点；`a_user_type_stream_read_consumes_the_string_before_failing` 钉住 `create_from_stream` **先消费长度前缀字符串**再报错 —— 字符串完整时得 `MissingConverter`，流截断时得 `StreamTruncated`（转换器根本未被询问），与参考读序一致。
  - **MAILBOX 线格式**：`a_mailbox_stream_is_the_raw_twelve_byte_ref` 钉住 12 字节小端 `{id, ip, port, salt}` = `[42,0,0,0, 0x7f,0,0,1, 0x34,0x12, 0xcd,0xab]` 并往返一致。
  - **零地址闸门**：`a_mailbox_rejects_an_uninitialised_address` 钉住 ip==0 → `UninitialisedMailBox { id }`、ip!=0 → 通过、非 MailBox 载荷 → `TypeMismatch { meta: "mailbox" }`；`a_mailbox_stream_write_rejects_a_foreign_value` 钉住写路径同名拒绝且不落任何字节。
  - **缺省行为**：`a_mailbox_default_is_the_zeroed_ref` 钉住 `getDefaultValue` 全零 ref；同一测试再用带 `<Default>` 子节的夹具钉住三件事 —— 诊断报参考的拒绝串、类型**照常构建**、零 ref 仍是默认值（参考 `setDefaultValue` 不落 `pDefaultValue_`）。
  - **节读取**：`a_mailbox_reads_its_id_ip_and_etc_children` 钉住 `id`/`ip`/`etc` 具名子节读取、`etc = port | (salt << 16)` 的拆包方向、子节缺失读 0（同 `readInt(tag, 0)`）。
  - **错误串**：`error_text_matches_the_reference_messages` 追加四处锚定：三个 USER_TYPE 站点串（`UserDataType::<site>: Do not have demo.Thing\n`）与 MAILBOX 零地址 WARNING（含结尾 `\n`）。
  - clippy `-D warnings`：entitydef **0 findings**（依赖 `crates/resources` 的 32 条既有 findings 属其他席位 lane，按隔离清单 `-A` 放行）。
  - rustfmt：`value.rs` / `data_type.rs` 干净（仅对本次触碰文件检查）。
  - 覆盖率：`value.rs` **99.08%** 行、全部 crate **≥98%**（门槛达成）。
- **证据链接**: `crates/entitydef/src/value.rs`（实现与全部单测）、`crates/entitydef/src/data_type.rs`（`Kind::{MailBox, UserType}`、`build_type` 的 MAILBOX `<Default>` 拒绝、`stream_size`/`client_safety`/`add_to_md5` 的 MAILBOX 臂，BW-04-01 系列已交付）。
- **状态**: 已交付（源码级对齐 + 线格式/错误串/缺省行为/读序锚定）。

## 备注（登记在案的差异 / 待验项）

1. **MAILBOX 流字节无参考 oracle（关键登记）**：`MailBoxDataType::to(mbr, BinaryOStream&)` 写的是 `stream << mbr`，但**全树不存在** `operator<<(BinaryOStream&, const EntityMailBoxRef&)`（`cstdmf/binary_stream.hpp/.ipp` 只有数值/字符串重载与 `insertRaw` 成员模板；`binary_stream.ipp:6` 注明无泛化模板）。即该路径在参考中**不产生任何字节**，本实现按 `streamSize()==12` 与 `insertRaw` 的 memcpy/本机小端口径推导为 12 字节裸 `{id, ip, port, salt}`，并如实登记为推导而非验证。
2. **USER_TYPE 默认值与参考有已登记差异**：参考 `pDefaultValue` 在报错后**继续写 Python None 并返回 true**（错误但成功返回）；本实现 `default_value` 签名返回 `PropertyValue`，USER_TYPE 落入 `_ => PropertyValue::Int(0)` 兜底臂。差异在于「报错后写 None」无法在无 Python 宿主的 `PropertyValue` 模型中表达（None 值仅 FIXED_DICT `AllowNone` 有），已如实登记，不虚称一致。
3. **仍无字节级参考 oracle**（同 BW-04-02-01/02/03 备注 1）：本项「转换及序列化夹具一致」的证据是与参考源码逐句对齐的自钉夹具测试，不是与参考二进制产物的比对。
4. `data_type.rs` 的 `Kind::MailBox` 基础臂（`stream_size`=12、`client_safety`=CLIENT_UNUSABLE、`add_to_md5("MailBox")`、`build_user_type` 的 `implementedBy` 拆分与两条诊断）为 BW-04-01 系列已交付，本项只新增 `build_type` 尾部的 `<Default>` 拒绝与不落盘。
5. `value.rs` 残余未覆盖 3 行：`452` 是向量解析器不变式 `unreachable!`（同 BW-04-02-01 备注 3）；`708`/`709` 是变长序列 `write_packed_int` 的 `map_err` 闭包 —— 只在 packed 计数超过 `MAX_PACKED_INT`（16 MiB）时触发（同 BW-04-02-02 备注 3）。

---

# BW-04-02-05 编码嵌套属性增量 — 复核证据

- **任务编号**: BW-04-02-05（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 372；验收：修改一个 FIXED_DICT 子字段时，更新切片的路径和负载与原版逐字节一致）
- **原版源码**: `lib/entitydef/property_change.cpp` + `.hpp` —— `Flags`（`FLAG_IS_SLICE=1<<0`、`FLAG_IS_NESTED=1<<1`）；`writePathSimple`（`uint8( path_.size() )` 后逐跳 `stream << iter->first`，`ChangePath::value_type` 是 `pair<int32,int32>`）；`SinglePropertyChange::addExtraBits(BinaryOStream&)` = `stream << leafIndex_`（int32）；`SlicePropertyChange::addExtraBits(BinaryOStream&)` = `uint32(start) << uint32(end)`；`addValueToStream` 交叶子（或逐元素）类型的 `addToStream(source, stream, false)`；`addInternalFlags`/`addToInternalStream`（**extra bits 只在嵌套路径上写**——`addSimplePathToStream` 仅当 `isNestedChange()`）；`isNestedChange( bool )` **忽略实参恒置真**（复制粘贴瑕疵）；`SinglePropertyChange::getRootIndex()`=`leafIndex_`、`SlicePropertyChange` 的为 0，`rootIndex()` 按 `isNestedChange()` 二选一。路径构造：`lib/entitydef/data_instances/intermediate_property_owner.cpp:13`（`getTopLevelOwner`：rpTopLevelOwner == 直接父级时索引进 `rootIndex_` **不进 path_**，否则 `addToPath( ownerRef_, getNumOwnedProperties() )`）——path **不含**实体自身属性索引；顺序更正见 BW-04-02-06 备注 5（`addToPath` 在递归返回后执行，流内是**下降序**：最近顶层属性的跳点先进流，头注释 "叶→根" 有误）；`lib/entitydef/data_types/fixed_dict_data_instance.cpp:338`（子字段修改用字段位次与字段数构 `changeOwnedProperty`）。信封证据：`server/cellapp/entity.cpp:5749-5754`（ghostedDataUpdate 先写 entity id + `int32( propertyIndex )` 再 `change.addToInternalStream`）；`:5093` 侧 `setPropertyFromInternalStream` 平路分支 `MF_ASSERT( !isSlice )` 证明平路切片不带 start/end。
- **Boundless 模块**: `crates/entitydef/src/property_change.rs`（新模块：`FLAG_IS_SLICE`/`FLAG_IS_NESTED`；`PropertyChange`（path + is_nested_change + root_index，`ChangeHop` 的 index_length 字段同参考保留）；`SinglePropertyChange::add_to_internal_stream` / `SlicePropertyChange::add_to_internal_stream`）；`crates/entitydef/src/data_type.rs`（新增 `field_type`=`FixedDictDataType::getFieldDataType`/`ClassDataType` 同名、`element_type`=`SequenceDataType::getElemType` 两个访问器）；`crates/entitydef/src/value.rs`（`add_to_stream`/`create_from_stream` 提为 `pub(crate)` 供本模块复用）。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**206 单元测试 + 4 差分测试**全部通过（本模块新增 9 测）。
  - **验收钉**：`a_fixed_dict_subfield_change_is_nested_with_an_empty_path` 钉住 `entity.myDict.ammo = 7`（myDict 为实体第 6 属性、ammo 第 0 字段、字典共 2 字段）的内流字节 `[2, 0, 0,0,0,0, 7]`——flags=2（嵌套非切片）、路径长 0（**实体自身索引 6 进 root_index 不进路径**）、leafIndex int32=0、负载是**字段类型**（UINT8）自己的 `[7]` 而非字典的 `[1,7]`。
  - **深层路径**：`a_deeper_change_records_the_hops_leaf_to_root` 钉住 `entity.arr[4].dict.ammo = 7` 的 `[2, 1, 4,0,0,0, 0,0,0,0, 7]`——路径长 1、跳点（下降序）4、leafIndex 0，实体索引仍在信封。
  - **嵌套切片**：`a_nested_slice_writes_start_end_then_one_payload_per_value` 钉住 `[3, 1, 1,0,0,0, 2,0,0,0, 5,0,0,0, 9, 8]`——flags=3、路径跳点 1、`uint32` start=2 end=5、每个新值一个元素负载。
  - **平路形态**：`a_flat_change_writes_its_flags_then_payload_only` 钉住平路单值 `[0, 7]`；`a_flat_slice_change_carries_the_slice_flags_only` 钉住平路切片 `[1, 5]`——**无 start/end**（参考平路不写 extra bits，且读侧 `MF_ASSERT( !isSlice )` 证明该形态不会被内部流使用）；`a_flat_slice_of_dicts_streams_each_full_dict` 钉住字典数组切片按字段序整流每个字典 `[1, 7, 1, 'x']`。
  - **负载走叶子类型**：`the_payload_of_a_single_change_is_the_leaf_types_own_form` 钉住 STRING 子字段的 packed 长度形态 `[2, 0, 0,0,0,0, 2, 'x', 'y']`。
  - **rootIndex 虚派发**：`the_root_index_follows_the_reference_virtual_dispatch` 钉住平路单值报 leafIndex、平路切片报 0、嵌套报信封索引，与参考 `rootIndex()` 的 `isNestedChange() ? rootIndex_ : getRootIndex()` 一致。
  - clippy `-D warnings`：entitydef **0 findings**（`crates/resources` 32 条既有 findings 属其他席位 lane，按隔离清单 `-A` 放行）。
  - rustfmt：`property_change.rs` / `value.rs` / `data_type.rs` / `lib.rs` 干净（仅本次触碰文件）。
  - 覆盖率：全部文件 **≥98%**（`property_change.rs` 100% 行、`value.rs` 99.08% 残 3 行同前备注）。
- **证据链接**: `crates/entitydef/src/property_change.rs`（实现与全部单测）、`crates/entitydef/src/data_type.rs`（`field_type`/`element_type`）、`crates/entitydef/src/value.rs`（`pub(crate)` 复用）。
- **状态**: 已交付（源码级对齐 + 内流字节/路径序/信封分离锚定）。

## 备注（登记在案的差异 / 待验项）

1. **仍无字节级参考 oracle**（同前四项备注）：process_defs 不遍历属性变更流；本项「逐字节一致」是**与参考源码逐句推出的内流形态一致的自钉测试**，不是与参考二进制产物的比对。
2. **外部（客户端）位压缩形态未移植**：`addCompressedPathToStream`/`addExtraBits(BitWriter&)` 需要 `BitWriter`/`BitReader`（`bitsRequired`），属 `client-server` 属性消息路径，本项只交付内流（服务器间）形态；`ChangeHop.index_length` 与 `leaf_size`/`original_leaf_size` 字段已按参考保留并在测试中钉住，供后续外部形态使用。
3. **信封不在本模块**：实体 id 与顶层属性索引（`ghostedDataUpdate` 的 `bundle << id; bundle << int32(propertyIndex)`）属消息层，`PropertyChange` 自身不含之，与参考 `rootIndex()` 只被调用方读取一致。
4. **`isNestedChange( bool )` 忽略实参**与**路径长 >255 静默截 uint8**两个参考形态均逐字保留（后者的前提：单条变更路径长度受限于定义深度，255 不可达）。
5. `changeOwnedProperty` 的 `canIgnoreAssignment`/`hasChanged`（同值短路）依赖 Python 宿主的 ScriptObject 比较，本模型由调用方用 `PropertyValue` 相等性自行短路，编码器不重复判断——与参考分层一致（编码发生在 `onOwnedPropertyChanged` 之后）。

---

# BW-04-02-06 应用嵌套属性增量 — 复核证据

- **任务编号**: BW-04-02-06（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 375；验收：接收原版更新切片后，只有目标字段改变，Python 通知携带正确旧值）
- **原版源码**: `lib/entitydef/property_change_reader.cpp` —— `PropertyChangeReader::readAndApply`（flags 分派；平路分支整属性 `createFromStream` 替换，嵌套分支走 `readSimplePathAndApply`）；`readSimplePathAndApply`（`uint8` 路径长 + 逐跳 `int32`，`getChildPropertyOwner` 逐级下降，`pOwner==NULL` 打印 `"PropertyChangeReader::readAndApply: pOwner is NULL. %d/%d.\n"`）；`getPyIndex`（FIXED_DICT → 字段名 PythonString，数组 → int）；`SinglePropertyChangeReader::apply`（`setOwnedProperty( leafIndex_ )`，返回**旧子值**而非整属性）；`SlicePropertyChangeReader::apply`（`setOwnedSlice` 仅数组可用，基类返回 NULL 时打印 `"Old value is NULL"` 不给旧值；逐元素读到流尽；`setSlice` 把 start 夹到 [0,origSize]、end 夹到 [start,origSize]，固定尺寸校验 `size != 0 && end-start != numToInsert` 打印 `"slice assignment would create array of wrong size"` **无尾换行**；通知路径元素 = `(start, end + newSize - oldSize)` **有符号**）。所有者资格：`childPropertyOwnerIsSimilarType` / `PyArrayDataInstance::getChildPropertyOwner`（ARRAY/TUPLE）/ `PyFixedDictDataInstance::getChildPropertyOwner`（FIXED_DICT，值为 None 视 0 子项）/ `PyClassDataInstance`（CLASS）；其余类型返回 NULL 打印 `"Bad index %d. size = %zd\n"`。叶索引越界：`fieldValues_[ref]` **无检查**（参考 UB）。
- **Boundless 模块**: `crates/entitydef/src/property_change.rs`（`PathElement::{Field,Index,Slice}`（Slice 端点带符号尺寸增量重定基）、`AppliedChange{old_value,path}`、`ApplyError` 九变体含参考原话 Display、`apply_internal_stream(root_type, root_value, stream)`、`apply_at` 下降器、`set_owned_property`/`set_owned_slice`）。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**222 单元测试 + 4 差分测试**全部通过（本模块新增 16 测）。
  - **验收钉**：`an_applied_dict_subfield_change_touches_only_the_target_field` 钉住 FIXED_DICT `[2, 0,0,0,0, 0,0,0,0, 7]` 应用后**只有 ammo 变 7**、label 原样、`old_value` = 旧子值 `Uint(5)`、通知路径 `[Field("ammo")]`（`getPyIndex` 的 FIXED_DICT → 字段名形态）。
  - **深路径与兄弟不动**：两跳 `[0, 0]` 穿数组入字典，兄弟字段与兄弟元素均原样。
  - **切片应用**：钉住 `[2,5]` 替换 `[4,9,2]` 后数组为 `[4, 7, 8, 2]`、旧值 NULL 语义、通知路径元素 `Slice(1, 2)`（end 按 +1 尺寸增量重定基，有符号）。
  - **固定尺寸校验**：`end-start != new.len()` → `WrongArraySize`，错误串含无尾换行的参考原话；尺寸 0 数组豁免。
  - **平路**：整属性 `create_from_stream` 替换、空路径；平路切片 flag 拒绝（`FlatSliceOnInternalStream`，对齐参考 `MF_ASSERT( !isSlice )`）。
  - **坏路径**：标量根上嵌套路径 → `OwnerIsNull{hop:1,size:1}` 值原样（参考 `getChildPropertyOwner` NULL 双打印收敛为单错误）；字典内坏跳 → `BadChildIndex`（`"Bad index %d. size = %zd\n"` 原话）；叶子越界 → `LeafIndexOutOfRange`（参考 UB，登记为显式错误）；切片落在非数组 → `OldValueIsNull`（`"Old value is NULL\n"` 原话）；截断流 → `Payload(StreamTruncated)`；BLOB 越界 → `UnableToCreateSlice`（原话）。
  - clippy `-D warnings`：entitydef **0 findings**（隔离清单放行 `crates/resources` 32 条既有 findings）。
  - rustfmt：仅触碰文件干净。
  - 覆盖率：全部文件 **≥98%**（`property_change.rs` 895/900 = 99.44%，残 5 行为显示分支不可达组合）。
- **证据链接**: `crates/entitydef/src/property_change.rs`（apply 家族与全部单测）、`docs/review/bw04-01-01-entitydef-parse.md` BW-04-02-05 节（编码形态，本项为其读侧对偶）。
- **状态**: 已交付（源码级对齐 + 应用语义/旧值/通知路径/错误串锚定）。

## 备注（登记在案的差异 / 待验项）

1. **仍无字节级参考 oracle**（同 BW-04-02-01..05 备注）：process_defs 不遍历属性变更应用；本项证据是与 `property_change_reader.cpp` 逐句对齐的自钉夹具测试，不是与参考二进制产物的比对。
2. **参考双打印收敛为单错误**：`getChildPropertyOwner` 打印 "Bad index" 后 reader 再打印 "pOwner is NULL"（两条都出）；Rust 以单个 `ApplyError` 表达同一失败，语义等价、打印次数不同，登记为差异。
3. **叶索引越界在参考是 UB**（`fieldValues_[ref]` 无检查），Rust 显式 `LeafIndexOutOfRange`；这是加严而非放松，登记在案。
4. **`endIndex < startIndex` 参考只打印 "Invalid values..." 并继续**（产物为空切片），未建为独立结果形态，已登记不虚勾。
5. **路径序更正随本 commit 落账**：`ChangePath` 头注释自称 "叶→根"，但 `IntermediatePropertyOwner::getTopLevelOwner` 的递归返回后才 `addToPath`，故**最近顶层属性的跳点先进流**——机制与 reader 都是下降序，注释是错的；BW-04-02-05 文档中的路径序表述已随本 commit 更正。
6. **切片端点重定基为有符号差**：`(new_size as i64 - orig_size as i64) as i32`（缩容为负），与参考 `end + newSize - oldSize` 一致；usize 直接相减在缩容时会下溢，已规避。
7. **平路切片 flag 是参考断言而非错误串**：`MF_ASSERT( !isSlice )` 在 release 构建是 UB；Rust 选择显式 `FlatSliceOnInternalStream` 错误，加严登记。

---

# BW-04-02-07 提取持久化投影 — 复核证据

- **任务编号**: BW-04-02-07（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 376；验收：数据库属性流与原版字节一致）
- **原版源码**: `lib/entitydef/data_types/fixed_dict_data_type.cpp` —— `getNumPersistentFields`（逐字段数 `isPersistent_`）；`getFieldIndexForPersistentFieldIndex`（投影位次 → 字段位次线性映射，越界 `MF_ASSERT( false && "Overran number of persistent fields" )` 后返回 0）；`addInstanceToStream( pInst, stream, isPersistentOnly )`（`isPersistentOnly && !it->isPersistent_` 则 `continue` 跳过该字段，其余按字段序逐个 `type_->addToStream( source, stream, isPersistentOnly )`——嵌套 FIXED_DICT 与序列元素继续透传该旗标）；`createInstanceFromStream( stream, isPersistentOnly )`（投影不含的字段用 `it->createDefaultValue( pDefaultSection_, sink )` 补缺，流里只读投影携带的字段）；`getStreamElement`（`isPersistentOnly` 时元素位次经 `getFieldIndexForPersistentFieldIndex` 映射回字段位次，None 指示字节与 enter/leave/end 元素不受影响）。旗标源头：字段小节 `<Persistent>`（`DataDescription` 同名旗标、实体级 `<Persistent>` 为另一层）；调用侧：`entitydef_script/py_component.cpp:66-96`（`delegate.getDataField( stream, descr, isPersistentOnly )` / `descr.addToStream( source, stream, isPersistentOnly )`）。属性变更路径显式传 false（`fixed_dict_data_type.cpp:397 /* isPersistentOnly: */ false`）。
- **Boundless 模块**: `crates/entitydef/src/data_type.rs`（`DataType::num_persistent_fields`、`DataType::field_index_for_persistent_field_index`）；`crates/entitydef/src/value.rs`（内部 `add_to_stream`/`create_from_stream` 增 `persistent_only` 形参并按参考透传：Sequence/FixedDict 递归臂透传、其余标量臂忽略；FixedDict 写臂 `continue` 跳过非持久字段、读臂用 `default_value` 补缺；公开面新增 `DataType::add_persistent_to_stream` / `DataType::create_persistent_from_stream`）；`crates/entitydef/src/property_change.rs`（六个负载点显式传 `false`，对齐参考 `/* isPersistentOnly: */ false`）。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**225 单元测试 + 4 差分测试**全部通过（本项新增 3 测）。
  - **验收钉**：`the_persistent_stream_carries_only_the_persistent_fields` 钉住三字段字典（ammo 持久 / temp `<Persistent> false </Persistent>` / label 持久）的数据库投影流 `[7, 1, 'x']`——**temp 的 99 不出现在流里**，而全字段形态是 `[7, 99, 1, 'x']`；字节形态与全字段流一致（None 指示与 enter/leave/end 元素均不变），差异只在字段过滤。
  - **读回补缺**：`a_persistent_read_fills_the_non_persistent_fields_from_their_defaults` 钉住投影读回后 temp 取类型缺省（对齐参考 `createDefaultValue` 补缺），ammo/label 从流恢复。
  - **投影记账**：`the_persistent_projection_counts_and_maps_only_the_persistent_fields` 钉住 `num_persistent_fields()==3`、投影位次 0/1/2 → 字段位次 0/2/3、越界 → `None`；非 FIXED_DICT 壳上两访问器为 0/`None`。
  - clippy `-D warnings`：entitydef **0 findings**；rustfmt 触碰文件干净；覆盖率全部文件 **≥98%**（value.rs 99.12%、data_type.rs 98.09%）。
- **证据链接**: `crates/entitydef/src/data_type.rs`（投影访问器与单测）、`crates/entitydef/src/value.rs`（持久流形态与单测）、`crates/entitydef/src/property_change.rs`（显式 false 透传）。
- **状态**: 已交付（源码级对齐 + 投影字节/补缺/记账锚定）。

## 备注（登记在案的差异 / 待验项）

1. **仍无字节级参考 oracle**（同 BW-04-02-01..06 备注）：process_defs 不产数据库属性流；本项「字节一致」是与参考源码逐句推出的投影形态一致的自钉测试，不是与参考二进制产物的比对。
2. **投影越界在参考是 `MF_ASSERT( false )`（debug 构建中止，release 返回 0）**，Rust 显式 `None`——把两种构建形态统一为可报告结果，登记为加严。
3. **`SequenceDataType` 的 `dbLen() > 0` 持久短路**（`sequence_data_type.cpp:59/243`：持久模式下序列以 `<DatabaseLength>` 定长截断/补齐）未建模——Rust 序列流形态无 dbLen 概念，登记待后续项；本项序列臂只透传旗标。
4. **`ClassDataType::getStreamElement` 也收 `isPersistentOnly`**（`class_data_type.cpp:510`），但 Rust 流形态没有 CLASS 臂（`PropertyError::Unsupported`），该映射未建模，登记在案。
5. **`readBool` 解析域**：参考 `DataSection::readBool`，Rust `as_bool` 只认大小写不敏感 true/false；夹具按此用 `<Persistent> false </Persistent>`。若参考对 `0/1/yes/no` 亦有解析（各 DataSection 实现而异），归 `crates/resources` 域，不在本席 lane 内改。

---

# BW-04-03-01 实现 exposed 方法可见性 — 复核证据

- **任务编号**: BW-04-03-01（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 380；验收：客户端只能调用允许方法）
- **原版源码**: `lib/entitydef/entity_method_descriptions.cpp/.hpp` —— 构造器 `maxExposedMethodCount_( -1 )`（未跑过 `setExposedMsgIDs` 前不可解析）；`init` 里 `isExposed()` 者进 `exposedMethods_`（内索引表），`exposedMethod( index )` 越界打印 `"Attempted to call exposed method id %u when only %zu methods are exposed\n"` 返回 NULL；`exposedMethodFromMsgID( msgID, data, range )` = `MF_ASSERT( maxExposedMethodCount_ != -1 )` → `range.exposedIDFromMsgID` → `exposedMethod( exposedID )`，且 `MF_ASSERT( pMethodDesc == NULL || component() != CLIENT )`（该变体只服务 BASE/CELL 入站，CLIENT 用 `exposedMethod` 直取）。暴露位语义（`method_description.hpp:176-199`）与 `exposedMethods_` 收集、按 streamSize 排序（固定尺寸在前）已在 BW-04-01 系列交付。
- **Boundless 模块**: `crates/entitydef/src/method.rs`（新增 `EntityMethodDescriptions::exposed_method_from_msg_id(msg_id, sub_index, range)`；**修复**：`EntityMethodDescriptions` 由 `#[derive(Default)]`（`max_exposed_method_count` 误为 0）改为手写 `Default`，`-1` 对齐参考构造器）。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**226 单元测试 + 4 差分测试**全部通过（本项新增 1 测）。
  - **验收钉**：`a_client_can_only_reach_the_exposed_methods` 钉住含 `<Exposed>` 的 ping/shout 与未暴露的 secret 三方法实体表——`num_exposed()==2`、每个暴露位次解析出的方法都**不是** secret、位次 2 拒绝；`set_exposed_msg_ids` 发出去的 msg id 经 `exposed_method_from_msg_id` 往返还原同方法（`exposed_index` 一致）；映射到表外的 msg id（200）拒绝；**赋号前**任何 msg id 都拒绝（构造态 `-1`）。
  - **本项连带修复一个潜在错误**：原 `#[derive(Default)]` 使未赋号状态的 `max_exposed_method_count` 为 0，而参考构造器是 `-1`——红测先抓住该差异再修复（TDD red→green）。
  - clippy `-D warnings`：entitydef **0 findings**；rustfmt 干净；覆盖率全部文件 **≥98%**（method.rs 1118/1122 = 99.64%）。
- **证据链接**: `crates/entitydef/src/method.rs`（`exposed_method_from_msg_id`、手写 `Default`、验收测）。
- **状态**: 已交付（源码级对齐 + 可达性/往返/拒绝锚定）。

## 备注（登记在案的差异 / 待验项）

1. **参考双形态收敛为 `None`**：越界 `exposedMethod` 打印 ERROR_MSG 返回 NULL、未赋号 `MF_ASSERT` 在 debug 构建中止——Rust 统一为 `None` 拒绝，登记在案。
2. **`component() != CLIENT` 的 `MF_ASSERT` 是调用方使用约束**（该变体只服务 BASE/CELL 入站调用，CLIENT 侧用 `exposedMethod` 直取），不是解析逻辑；Rust 解析器不加组件门，登记为使用约定。
3. **`sub_index` 形参替代流读**：参考从 `data` 流读子 id 字节，Rust 由调用方传字节（与 BW-04-01 已交付的 `exposed_id_from_msg_id(msg_id, sub_index, num_exposed)` 同构），流读取属消息层。
4. 无字节级 oracle（同前项备注）：process_defs 不走入站调用解析；证据为源码级对齐 + 自钉测试。

---

# BW-04-03-02 处理属性分发标志 — 复核证据

- **任务编号**: BW-04-03-02（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 381；验收：同一属性的四类接收方与基线一致）
- **原版源码**: `lib/entitydef/data_description.ipp:80-155` —— 四类接收方谓词全是分发位的直读：`isGhostedData()`=`DATA_GHOSTED`、`isOtherClientData()`=`DATA_OTHER_CLIENT`、`isOwnClientData()`=`DATA_OWN_CLIENT`、`isBaseData()`=`DATA_BASE`，`isCellData()` 是**否定式** `!isBaseData()`，`isClientServerData()`=三元并 `OTHER_CLIENT|OWN_CLIENT|REPLAY`；`isExposedForReplay()`=`DATA_REPLAY`。基线分类表 `s_entityDataFlagMappings`（`setEntityDataFlags` 查表）：`CELL_PRIVATE`=0 / `CELL_PUBLIC`=GHOSTED / `OTHER_CLIENTS`=GHOSTED|OTHER_CLIENT / `OWN_CLIENT` / `BASE` / `BASE_AND_CLIENT`=OWN_CLIENT|BASE / `CELL_PUBLIC_AND_OWN`=GHOSTED|OWN_CLIENT / `ALL_CLIENTS`=GHOSTED|OTHER_CLIENT|OWN_CLIENT / `EDITOR_ONLY`。解析期的位层叠（`<Persistent>`/`<Identifier>`/`<Indexed>`/`ExposedForReplay` 以 `isOtherClientData()` 为缺省、仅当 `isClientServerData()` 才生效）为 BW-04-01/02 系列已交付并钉测。
- **Boundless 模块**: `crates/entitydef/src/data_description.rs`（新增 `DataDescription` 六转发访问器 `is_ghosted_data` / `is_other_client_data` / `is_own_client_data` / `is_base_data` / `is_cell_data` / `is_client_server_data`，均落到已交付的 `EntityDataFlags` 位谓词）。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**227 单元测试 + 4 差分测试**全部通过（本项新增 1 测）。
  - **验收钉**：`a_propertys_four_receivers_match_the_baseline` 以参考命名表逐一取值跑真解析（`UINT8` 属性 + `<Flags>`），钉住四类接收方矩阵 `(ghost, other, own, base, cell-home)`：`ALL_CLIENTS`=(T,T,T,F,T)、`CELL_PUBLIC_AND_OWN`=(T,F,T,F,T)、`BASE_AND_CLIENT`=(F,F,T,T,F)、`BASE`=(F,F,F,T,F)、`CELL_PRIVATE`=(F,F,F,F,T)——`CELL_PRIVATE` 是参考表达「无接收方」的名字（空 `<Flags>` 是解析失败，见已交付测），且它的 `is_client_server_data()` 为假（不进客户端/服务器通道）。
  - 顺带钉住 `is_cell_data` 的否定式语义：base 属性为 cell-home 假、CELL_PRIVATE 为真。
  - clippy `-D warnings`：entitydef **0 findings**；rustfmt 干净；覆盖率全部文件 **≥98%**。
- **证据链接**: `crates/entitydef/src/data_description.rs`（转发访问器与四类接收方测）、`crates/entitydef/src/flags.rs`（位常量/映射表/位谓词）。
- **状态**: 已交付（源码级对齐 + 四类接收方矩阵锚定）。

## 备注（登记在案的差异 / 待验项）

1. **参考的 dev 断言未建模**：`data_description.cpp` 在解析期 `MF_ASSERT_DEV( !isBaseData() || (!isGhostedData() && !isOtherClientData()) )`（base 数据同时走 ghost/other-client 是矛盾配置）。Rust 解析不设此断言也不报错——登记为未建模，非行为差异（参考该断言在 release 构建不生效，在 dev 构建是提示性）。
2. **`CELL_PRIVATE` 而非空标志**：参考 `setEntityDataFlags` 查表，`CELL_PRIVATE` 名对应 0 位；空 `<Flags>` 在两侧都是解析失败（已交付测 `missing_flags_is_a_parse_failure` 钉住），故「无接收方」只能以该名字表达。
3. **分发位的解析层叠已在位**（`<Persistent>`/`<Identifier>`/`<Indexed>`/`ExposedForReplay` 缺省 `isOtherClientData()` 且仅 `isClientServerData()` 生效）——本项只补接收方谓词面向属性层的入口，不重复建模。
4. 无字节级 oracle（同前项备注）：分发标志不产生流字节；证据为源码级对齐 + 自钉矩阵测试。

---

# BW-04-03-03 验证参数与权限失败 — 复核证据

- **任务编号**: BW-04-03-03（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 382；验收：返回同类错误且业务回调未执行）
- **原版源码**: `lib/entitydef/method_description.cpp` `MethodDescription::areValidArgs( exposedExplicit, args, generateException )` —— `firstOrdinaryArg = exposedExplicit && isExposed() && component() == CELL`（暴露的 CELL 方法带隐式实体参数）后交 `MethodArgs::checkValid`；`lib/entitydef/method_args.cpp` `MethodArgs::checkValid` —— **先数个数**：`expectedArgs = args_.size() + firstOrdinaryArg`，不等即 `PyErr_Format( PyExc_TypeError, "Method %s requires exactly %d argument%s; %d given", name, expectedArgs, (expectedArgs == 1) ? "" : "s", args.size() )` 并返回 false（**在逐参数类型检查之前**）；`firstOrdinaryArg == 1` 时再验隐式参数是 None/id/带 `id` 的对象（`"Method %s requires None, an id, or an object with an id as its first argument"`）；逐参数 `isSameType` 失败报 `"Method %s, argument %d: Expected %s, ..."`（两分支，按 Python 类型对象取措辞）。权限侧（客户端只能调用暴露方法）由 BW-04-03-01 的暴露表解析门承担。
- **Boundless 模块**: `crates/entitydef/src/method.rs`（新增 `ArgCountError`（参考原话 Display，含 `expectedArgs == 1 ? "" : "s"` 单复数）与 `MethodDescription::first_ordinary_arg` / `expected_args` / `check_arg_count`）。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**228 单元测试 + 4 差分测试**全部通过（本项新增 1 测）。
  - **验收钉**：`an_argument_count_mismatch_rejects_the_call` 钉住暴露 CELL 方法 `jump(INT32)`——`first_ordinary_arg(true)==1`、`expected_args(true)==2`（隐式实体参数）、给 2 通过、给 1 拒：`"Method jump requires exactly 2 arguments; 1 given"`；基座方法 `spin(INT32)` 无隐式参数（`first_ordinary_arg==0`、期望 1），多给一个拒且**单数措辞**钉住三元：`"Method spin requires exactly 1 argument; 2 given"`。计数门在逐参数类型检查之前，故错误即返回、调用不外传。
  - clippy `-D warnings`：entitydef **0 findings**；rustfmt 干净；覆盖率全部文件 **≥98%**。
- **证据链接**: `crates/entitydef/src/method.rs`（`ArgCountError`、三个访问器与验收测）、BW-04-03-01 节（权限侧暴露门）。
- **状态**: 已交付（源码级对齐 + 计数门/错误原话/单复数锚定）。

## 备注（登记在案的差异 / 待验项）

1. **Python 宿主分支未建模**：参考 `PyErr_Format` 把错误设进 Python 异常状态、`generateException` 为假时 `PyErr_Clear()`；Rust 以 `Result<(), ArgCountError>` 表达同一决定（错误即拒绝、不执行后续），无 Python 宿主可清。
2. **隐式实体参数的类型检查未建模**：`firstOrdinaryArg == 1` 分支要求 arg0 是 None/`int`/带 `id` 属性的对象，其判定与错误串依赖 Python 对象身份，静态定义域内不可表达（参数计数与期望个数已完整建模）。
3. **逐参数类型不匹配消息未建模**：`"Method %s, argument %d: Expected %s, ..."` 两分支的措辞取自 Python 类型对象（`PyObject_Str` / `tp_name`），同属宿主依赖；参数类型本身的静态类型与流尺寸已交付。
4. **「业务回调未执行」的建模口径**：计数门在逐参数检查之前返回，与参考一致——错误一旦产生即终结调用路径，本 crate 无分派回调可执行，故以「错误结果即拒绝」等价表达，登记为口径说明而非行为差异。
5. 无字节级 oracle（同前项备注）：参数校验不产生流字节。

---

# BW-04-03-04 实现带返回值的方法应答 — 复核证据

- **任务编号**: BW-04-03-04（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 383；验收：响应字段和超时行为一致）
- **原版源码**: `lib/entitydef/method_description.cpp` —— `addReturnValues`（`returnValues_.addToStream( source, stream )`）；`createReturnValuesFromStream` / `createReturnValuesOrFailureFromStream`：应答帧以 `uint8 isError` 起头，置位则读两个字符串（`errorType` + 可 unpickle 的 `errorArgsStr`，空串视作 0 元组）并造错误对象，清零则按声明顺序逐返回值 `MethodArgs::createFromStream`。`lib/entitydef/method_args.cpp` `MethodArgs::addToStream`（逐个 `type_->addToStream( source, stream, false )`，**无计数无名字**；源里缺值时用该类型的 `getDefaultValue` 照样写入并记 `ERROR_MSG( "MethodArgs::addToStream: Insufficient argument values.\n" )`）与 `createFromStream`（逐返回值失败报 `"MethodArgs::createFromStream: Failed to get arg %d (of type %s) for method %s from the stream.\n"`，`sink` 侧另有 enterItem/write 三处失败文案）。
- **Boundless 模块**: `crates/entitydef/src/method.rs`（新增 `Response::{Values, Failure}`、`ResponseError::{InsufficientReturnValues, ReturnNotFromStream, TruncatedFrame, Property}`（Display 钉参考原话，`write_with_newline` 依 `writeln!` 改写）与 `MethodDescription::add_return_values_to_stream` / `create_return_values_from_stream` / `create_response_from_stream`；`foundation::stream::BinaryStream` 引入）。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**230 单元测试 + 4 差分测试**全部通过（本项新增 2 测）。
  - **验收钉**：`a_returning_method_carries_its_response_field` 钉住 `probe(INT32)` 声明 `Result: INT32` + `Flag: STRING` 的应答字段形态——两个返回值按声明序各自的流形态拼成 `[3,0,0,0, 2,'o','k']`（STRING 长度是 packed 编码，故为 2 而非 1），成功帧 `[0, ...]` 读回两个值；失败帧 `[1, "TypeError", ""]` 读回 `Failure { error_type, error_args }`；缺值时缺省照常入流 `[3,0,0,0, 0]` 并报 `InsufficientReturnValues`。
  - **措辞钉**：`the_reply_failures_carry_the_reference_wording` 钉住三条原话——逐返回值失败 `"MethodArgs::createFromStream: Failed to get arg 0 (of type INT32) for method probe from the stream.\n"`、帧截断（描述性文案，见备注 4）、缺值 `"MethodArgs::addToStream: Insufficient argument values.\n"`，且缺值帧仍写出缺省字节 `[0,0,0,0]`。
  - clippy `-D warnings`：entitydef **0 findings**；rustfmt 干净；覆盖率全部文件 **≥98%**（method.rs 1340/1349 = 99.33%）。
- **证据链接**: `crates/entitydef/src/method.rs`（应答帧类型/三访问器/两测）。
- **状态**: 已交付（源码级对齐 + 应答字段字节/帧分支/原话锚定）。

## 备注（登记在案的差异 / 待验项）

1. **「超时行为」在参考定义层无对应物（关键登记）**：验收句含「超时行为一致」，但参考 entitydef / delegate 层内**不存在**应答超时机制——`lib/entitydef_script`、`lib/delegate_interface`、`lib/entitydef` 全无 `timeout` 字面（已全树检索）；应答时限属消息/网络层（Mercury 侧），不在定义域内。故本项只对齐应答**字段**形态，超时无参考源码可移植，如实登记，不虚勾。
2. **`DataSource` 侧形态未建模**：参考 `MethodArgs::addToStream` 走 `source.beginSequence`/`enterItem`/`leaveItem`，逐处失败各有文案（`enterItem failed for arg %zu.\n` 等）；Rust 在静态 `PropertyValue` 序列上组合，同一无计数无名字形态，进入/离开元素的文案与失败位点不可复现。
3. **应答元组的隐式源实体 id 未建模**：参考 `MethodArgs::createFromStream` 在 `pImplicitSource != NULL` 时把源实体 id 写成元组第 0 项（`tupleSize = numArgs + 1`）——属 Python 元组形状，静态定义域内无对应物。
4. **`TruncatedFrame` 文案是描述性的**：参考该路径不打印任何消息（流错误只翻成 error 字节），Rust 以说明性文案表达同一拒绝，已登记为差异而非原话引用。

---

# BW-04-03-05 序列化 MailBox 引用 — 复核证据

- **任务编号**: BW-04-03-05（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 392；验收：原版可恢复实体位置与类型）
- **原版源码**: `lib/network/basictypes.hpp:322-365` —— `EntityMailBoxRef`：`EntityID id` + `Mercury::Address addr`；`Component` 枚举 `CELL=0, BASE=1, CLIENT=2, BASE_VIA_CELL=3, CLIENT_VIA_CELL=4, CELL_VIA_BASE=5, CLIENT_VIA_BASE=6, SERVICE=7`；`component() = addr.salt >> 13`、`type() = addr.salt & 0x1FFF`（`EntityTypeID` 为 `uint16`，basictypes.hpp:185）、`init( id, addr, c, t )` 打包 `addr.salt = (uint16(c) << 13) | t`。**线上形态无专属 operator**：`MailBoxDataType::from( BinaryIStream&, … )` 的 `stream >> mbr` 落到 `lib/cstdmf/binary_stream.ipp` 的小端泛型模板（`extractRaw` 整结构体 memcpy）——即 12 字节裸排：`id` u32 LE、`ip` u32 LE、`port` u16 LE、`salt` u16 LE（`Address` 自带 operator，basictypes.cpp:199-221，ip/port raw + salt LE，字节排布相同）；`streamSize() = sizeof( EntityMailBoxRef )`。节子项 `id` / `ip` / **`etc`**（端口从 `etc` 读，mailbox_data_type.cpp from/to Section）。`componentAsStr`（basictypes.cpp:237-256）：`cell / base / service / client / base_via_cell / client_via_cell / cell_via_base / client_via_base`。`hasAddress()`：`addr != Address::NONE`，而 `operator==` 只比 `ip` 与 `port`（basictypes.ipp），salt 不参与。参考单测锚 `test_streamsizes.hpp:143-157` `MailBoxDataTypeFixture`：id 2405、`Address( 0xc0180010, 0x4567 )`、component `BASE_VIA_CELL`、typeID 7（test_streamsizes.cpp:93-98 `CHECK_FIXED_TYPE_STREAMSIZE`）。
- **Boundless 模块**: `crates/entitydef/src/value.rs` —— 12 字节裸流形态与 `id`/`ip`/`etc` 节读法已于 BW-04-02 交付；本项新增 `MailBoxComponent`（8 变体按参考数值序）、`MAILBOX_COMPONENT_SHIFT` / `MAILBOX_TYPE_ID_MASK`、`MailBoxComponent::into_salt` / `as_str`、`PropertyValue::mail_box_ref`（`init` 打包）与读取器 `mailbox_component` / `mailbox_type_id` / `mailbox_component_name` / `mailbox_has_address`。
- **平台**: Linux x86_64，Rust 1.100.0，llvm-cov 0.9.1。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**232 单元测试 + 4 差分测试**全部通过（本项新增 2 测）。
  - **验收钉（恢复位置与类型）**：`a_streamed_mailbox_ref_recovers_the_entity_position_and_type` 以参考 fixture 值（id 2405 @ 0xc0180010:0x4567，BASE_VIA_CELL，typeID 7 → salt `0x6007`）写入 12 字节流再读回，id/ip/port/salt 逐项相等且 `mailbox_component()==BaseViaCell`、`mailbox_type_id()==7`。
  - **salt 语义钉**：`a_mailbox_salt_splits_into_the_component_and_the_type` 钉住 init 打包、全部 8 组件与 `componentAsStr` 8 名、13-bit 全宽 type id 往返、`hasAddress` 的 ip/port-only 语义（零地址仍带 component/type）、非邮箱值四读取器全 None。
  - clippy `-D warnings`：entitydef **0 findings**；rustfmt 干净；覆盖率全部文件 **≥98%**（value.rs 1473/1485 = 99.19%）。
- **证据链接**: `crates/entitydef/src/value.rs`（MailBox ref 语义节 + 两测）。
- **状态**: 已交付（流往返恢复位置与类型 + salt 位域/组件名/hasAddress 语义锚定）。

## 备注（登记在案的差异 / 待验项）

1. **无字节 oracle**：参考单测对 mailbox 只有 `CHECK_FIXED_TYPE_STREAMSIZE`（12 字节定长）与 None 转换检查，无逐字节断言；字节排布 id/ip/port/salt LE 由小端泛型 `extractRaw` 推出，登记为**推导**而非原版字节捕获。带宽 12 字节与 `streamSize()` 口径一致（data_type.rs `MAILBOX_STREAM_SIZE`）。
2. **TDD red 一次为测试期望自误**：首跑把 BASE_VIA_CELL 的 salt 期望写成 `0xA007`（那是 CELL_VIA_BASE=5），按参考枚举（BASE_VIA_CELL=3）改正为 `0x6007`；实现未动。
3. **`"<invalid>"` 分支不可达**：`componentAsStr` 的 fallback 只护 C++ 越枚举值；salt 的 3-bit 域 0..=7 全被枚举覆盖，Rust 匹配穷尽无 fallback——登记为等价加严。
4. **`PyEntityMailBox` 侧不在域内**：`reduceToRef`/`constructFromRef` 与「零 IP 信箱缓冲方法」属 Python 宿主层，静态 crate 无对应物；`isSameType` 的零 IP 拒绝（`UninitialisedMailBox` 原话报错）已于 BW-04-02 交付并有专测。
5. **`getStreamElement` 单元素拆分未另建**：参考 `MailBoxDataType::getStreamElement` 返回单个 `MailBoxStreamElement`（size=1），Rust 以 `MAILBOX_STREAM_SIZE` 整块口径覆盖同一形态，无逐元素拆分器。

---

# BW-04-04-01 兼容 process_defs 参数 — 复核证据

- **任务编号**: BW-04-04-01（lane BW-04 EntityDef、实体数据与 RPC 契约，todo.md 行 399；验收：帮助、退出码和选项有基准）
- **原版源码**: `tools/process_defs/main.cpp` —— `USAGE` 原文、`getOption`/`hasFlag` 扫描（"deliberately stop one short"：末位 flag 无值、默认值存活、盲取下一参、首个匹配即返回）、选项序 `module → function → help → verbose → use-stdout → (-r 走 BWResource::init) → path → 残余 argv`；`--help` 分支 usage 打到 **stderr** 且无条件 `EXIT_SUCCESS`；`EntityDescriptionMap::parse` 失败 → `"Failed to parse .def files\n"` + `EXIT_FAILURE`；`Script::init` 失败 → `EXIT_FAILURE`。`-r`/`--res` 扫描在 `lib/resmgr/bwresource.cpp` `BWResource::init`（同样停一位短、成对移除）。**活基准**（本机 stock 二进制 `game/bin/server/el7/tools/process_defs` 实测）：`--help`（带 `-r` res 根）→ exit 0、usage 全文到 stderr、stdout 空；坏 `.def` / 缺 `entities.xml` → exit 1 + `EntityDescriptionMap::parse: pSection is NULL` + `Failed to parse .def files`；无 res path → exit 1 + `Failed to initialise Python`；未知 flag `--bogus` → 本环境 SIGABRT(134)（`BWResource::init` 本身容忍未知参，abort 出自其后 Python stdout flush 路径）。
- **Boundless 模块**: `crates/entitydef/src/cli.rs`（`USAGE` 常量与 stock `--help` 逐字节一致、`CliOptions`、`get_option`/`has_flag`/`extract_res_paths` 三扫描器、`parse_options` 按 `main()` 序、`run()`：help → usage 到 stderr + 0；加载成功 → dump 到 stdout + 0；失败 → `"Failed to parse .def files"` + 1）+ `crates/entitydef/src/bin/process_defs.rs` 薄入口 + `lib.rs` `pub mod cli;`。
- **平台**: Linux x86_64（el7 参考二进制实测；Rust 工具链 1.100.0，llvm-cov 0.9.1）。
- **新版结果**:
  - `cargo test -p entitydef --offline`：**238 单元测试 + 4 差分测试**全部通过（本项新增 6 测）。
  - **端到端（Rust bin vs stock 实测）**：`--help` stderr 与 stock 输出**逐字节相等**且 exit 0、stdout 同为空；`-r docs/review/fixtures/service_def/res` stdout 与 oracle `service_entitydef.json` **逐字节相等**且 exit 0；无 `-r` → exit 1 + `Failed to parse .def files`。
  - clippy `-D warnings`：entitydef **0 findings**；rustfmt（仅触达文件）干净；覆盖率（`--lib`）cli.rs **163/163 = 100%**，全部文件 **≥98%**。
- **证据链接**: `crates/entitydef/src/cli.rs`、`crates/entitydef/src/bin/process_defs.rs`、`docs/review/reference/service_entitydef.json`（oracle）、本机 stock 二进制实测记录（本节）。
- **状态**: 已交付（帮助文本/退出码/选项扫描三基准：源码逐句 + stock 二进制活测 + Rust bin 端到端逐字节）。

## 备注（登记在案的差异 / 待验项）

1. **`-p` 默认值双口径**：代码默认 `"."`，usage 文案宣称 res 树 scripts 目录——参考自身如此，Rust 照抄两种口径并登记。
2. **`getOption` 盲取值已钉**：`-m -h` → module=`-h` 且 help 不触发；首个匹配即返回，第二次出现的 option 对残留进 `script_args`——均按参考行为建测。
3. **无 Python 宿主**：`-m/-f/-p` 照常解析并保留语义，但"回调"固定为确定性 JSON dump（即 `-m process_defs_dump -f dump` 契约）；help 分支不调用模块 `help` 函数（参考中该调用结果不影响退出码）。
4. **未知 flag**：Rust 保留进 `script_args` 并继续（`BWResource::init` 语义）；stock 二进制在本环境该场景 SIGABRT(134)，属其 Python 层伪影，差异登记。
5. **多 `-r`**：全部按序收集，首个作加载根（`load_and_dump` 单根），其余保留——与参考逐个 `addPaths` 的差异登记。
6. **bin 目标不计入 lib 覆盖率**（`--lib` 门禁）：其行为由端到端逐字节比对背书；`run()` 由泛型 `IntoIterator` 改收 `Vec<String>`，消除 llvm-cov 多实例化（23 实例仅 15 覆盖）导致的行覆盖聚合伪影（97.60% → 163/163）。
7. **stock `--help` 运行观察**：模块 `help()` 的 print 本次未见于 stdout/stderr（`ProcessDefsHelpMsgHandler` 脚本输出重定向链），如实登记。
