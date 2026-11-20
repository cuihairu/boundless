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
