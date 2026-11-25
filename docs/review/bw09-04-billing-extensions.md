# BW-09-04-02/03/04 Python Billing、自定义扩展与重复认证 — 复核证据

- **任务编号**: BW-09-04-02（todo.md 行 1378）、BW-09-04-03（行 1379）、
  BW-09-04-04（行 1380）
- **验收**: 接口及异常与原脚本契约一致 / 按目标验证可用与依赖缺失 /
  并发请求不污染登录记录
- **Boundless 模块**: `crates/server-runtime/src/py_billing.rs`（04-02）、
  `crates/server-runtime/src/custom_billing.rs`（04-03）、
  `crates/server-runtime/src/relogon.rs` 新增并发场景测试（04-04）
- **平台**: Linux x86_64

## 原版形状

| 原版位置 | 形状 |
|---|---|
| py_billing_system.cpp:18-95 | `PyBillingResponse`：七个方法，恰一响应；`pHandler_` 置空即锁 |
| py_billing_system.cpp:193-218 | `loadEntity`：**dbID==0 先查**（`ValueError "0 is an invalid database id"`），类型未知→`ValueError "Invalid entity type '%s'\n"`（保留尾换行），发 `onGetEntityKeyForAccountSuccess` |
| py_billing_system.cpp:221-260 | `loadEntityByName`（6 参，`shouldCreateUnknown`/`shouldRememberUnknown` 缺省 false）、`createNewEntity`（`shouldRemember`）→`LoadFromUsername`/`CreateNew` |
| py_billing_system.cpp:263-282 | `getEntityTypeID`：**已发送先查**（`ValueError "Response already sent"`），再查类型 |
| py_billing_system.cpp:285-318 | 三个命名失败（68/67/71）+ `failure(int)`：`status.okay()`（`NOT_SET`/`LOGGED_ON`）或截断（`statusAsInt != status.value()`）→`ValueError "Invalid failure status %d"`；`okay()` 只认 0/1（log_on_status.hpp:88-98） |
| py_billing_system.cpp:29-43 | 析构器：未发送→`failureDBError("No response from script")`（71 通用失败） |
| py_billing_system.cpp:347-430 | `PyBillingSystem::getEntityKeyForAccount`：`getMember` 缺方法→ERROR 日志 + `LOGIN_REJECTED_DB_GENERAL_FAILURE`（空 errorMsg）；脚本异常→`PyErr_PrintEx` **不调 handler**，由析构器兜底 |
| py_billing_system.cpp:437-480 | `setEntityKeyForAccount`：缺 setter→ERROR + `onSetEntityKeyForAccountFailure(ekey)`；参数 `(username, password, entityDesc.name(), dbID)`；异常→Failure(ekey)，成功→Success(ekey) |
| py_billing_system.hpp:49-51 | `isOkay()` 返回 `getEntityKeyFunc_` 真值：**只看 get 方法存在** |
| custom_billing_system.cpp:21-39 | `CustomBillingSystemCreator`：`typeName() == "custom"`，静态注册 |
| custom_billing_system.cpp:70-93 | 构造缓存 `entityTypeID_ = entityDefs.getEntityType("Account")`；`getEntityKeyForAccount` 恒发 `CreateNew(entityTypeID_, shouldRemember=false, "", password)`——密码不验，交给 base 实体（即 `authenticateViaBaseEntity` 行为） |
| custom_billing_system.cpp:95-114 | `setEntityKeyForAccount` 无存储，恒 `Success(ekey)`；`isOkay()` = `entityTypeID_ != INVALID_ENTITY_TYPE_ID`（**Account 缺 = 依赖缺失**） |
| billing_system_creator.cpp:38-79 | `createFromConfig`：`billingSystem/type` 缺省 `"standard"`；集合空→WARNING "No billing systems registered."；按 typeName 分发；建出后 `!isOkay()`→ERROR "Billing system is not configured correctly" 并删除；成功→INFO "Using billing system: %s" |

## 端口语义

- `py_billing.rs`：`EntityDefs`（名字↔id 双向）、`PyBillingResponse`（七法 +
  `send_failure` 锁）、`PyEntityKeyOutcome`/`PySetEntityKeyOutcome`（回调
  词表）、`BillingScript`/`PyGetEntityKeyFn`/`PySetEntityKeyFn`/`PyBillingSystem`
  （成员缺失、脚本异常、未发送三分支）。
- `custom_billing.rs`：`BillingSystem` trait（三法）、`BillingSystemCreator`
  trait（typeName/create）、`CustomBillingSystem`、`CustomBillingSystemCreator`、
  `builtin_billing_system_creators()`（本构建编译进的扩展集 = "按目标"）、
  `create_from_config()` 五路结果（`Created`/`NotConfigured`/`CreatorFailed`/
  `NoSuchType`/`NoCreatorsRegistered`）。
- `relogon.rs`：复用 BW-06-02-03/04 的 `LogOnRecords` + `on_duplicate_login`
  + `RelogonAttempt`，新增一条双客户端交错生命周期场景测试。

**验收证据**：

- *04-02（接口及异常一致）*：四条 `ValueError` 逐字钉死
  （`"0 is an invalid database id"`、`"Response already sent"`、
  `"Invalid entity type 'Ghost'\n"` 含尾换行、`"Invalid failure status 0/1/256/-1"`）；
  检查顺序钉死（dbID==0 先于已发送检查——`loadEntity` 的零 id 在已发送响应上
  仍报零 id）；七方法的回调载荷逐字段比对（Success 的 key+两串、
  LoadFromUsername 六字段、CreateNew 四字段、三分支命名失败状态码 68/67/71）；
  `failure(94)`/`failure(69)` 透传原始数值；缺 get 方法/脚本异常/未发送三分支
  全部落 `LOGIN_REJECTED_DB_GENERAL_FAILURE("No response from script")`；
  set 三分支（成功载荷四参逐字、缺 setter、脚本异常）。
- *04-03（按目标可用与依赖缺失）*：本构建编译进 `custom` 一个扩展（参照三
  creator 中 standard/bwauth 属其他项）；有 `Account` 定义→`Created` 且
  `is_okay`；无 `Account`→`isOkay()` false→注册表 `NotConfigured`（参照删除
  语义），同时钉死残废系统仍会以 `INVALID_ENTITY_TYPE_ID` 应答（拦它的是注册
  表）；缺省类型名 `"standard"` 在只有 custom 编入的构建上 `NoSuchType`；
  空集合 `NoCreatorsRegistered`；creator 造不出 `CreatorFailed`。
- *04-04（并发不污染）*：`concurrent_logins_for_one_account_leave_one_record`
  交错钉死——A 的 checkout 占住实体时 B 的 checkout 被拒；B 认证走重复登录
  仲裁、第三人堆积被拒且不动 pending；base 拒绝后 A 的记录原样；等待销毁期间
  旧实体登出→记录擦除、登录重跑；B 重登后**恰好一条**记录（B 的 proxy）；
  B 登出后记录集回到空，checkout/pending 全清。既有 29 条仲裁测试（第二次
  重登拒、pending 槽至多一条、迟到事件 Ignored 等）为逐门旁证。

## 有意偏离

1. **脚本面建模为闭包**（`FnMut(…, &mut PyBillingResponse) -> Result<(), String>`）：
   参照的 `PyObject*` 管线属 BW-13 pyhost 面；本项契约是**方法集与异常**，
   闭包即脚本。
2. **失败状态以原始数值承载**（`status: i32`）：脚本 `failure(n)` 可透传本树
   `LogOnStatus` 枚举无名分的值（如 94..243 空档）；参照 `LogOnStatus` 包任意
   int，数值即契约。三个命名失败方法仍走 68/67/71 字面值。
3. **`statusAsInt != status.value()` 截断守卫**：参照的 `Status` 是 int 背书
   枚举，该守卫恒假（死码）；端口按 u8 线域建模（2..=255 之外拒），行为上
   拒绝集一致（okay 两值 + 出域值）。
4. **注册表只编入 custom**：`standard`（personality 钩子）与 `bwauth` 是
   参照的另外两个 creator；04-01 的标准 XML 后端已另行交付，bwauth 不在本
   项验收内。`create_from_config` 接受任意编入集合，默认类型名照抄
   `"standard"`。
5. **参照的 `ERROR_MSG`/`INFO_MSG` 日志行**（"No such method %s"、"Using
   billing system: %s" 等）以结果枚举承载，不落日志面。

## 门禁（本批）

| 门禁 | 结果 |
|---|---|
| `cargo test -p server-runtime --lib` | **1242 passed / 0 failed / 1 ignored**（新增 27：py_billing 18 + custom_billing 8 + relogon 场景 1） |
| `cargo test --workspace` | **exit 0，4379 passed / 0 failed / 107 ignored**（109 个绿目标） |
| `cargo clippy -p server-runtime --all-targets` | 0 finding 落在 py_billing.rs / custom_billing.rs / relogon.rs |
| `rustfmt --edition 2024 --check`（三文件 scoped） | 0 diff（幂等） |
| `py_billing.rs` 行覆盖 | **100.00%**（431 个 DA 行 / 0 missed） |
| `custom_billing.rs` 行覆盖 | **100.00%**（137 个 DA 行 / 0 missed） |

## 状态

已验收：Python Billing 的方法集与四条异常逐字同原脚本契约，检查顺序与三分支
兜底钉死；自定义扩展按编入目标验证可用（Account 在）与依赖缺失（Account 缺，
注册表拒）；重复认证交错生命周期下登录记录恰好一条、无残留 checkout/pending。
