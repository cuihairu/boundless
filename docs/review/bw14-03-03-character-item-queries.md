# BW-14-03-03 移植角色与物品查询 — 复核证据

- **任务编号**: BW-14-03-03（todo.md 行 1760）
- **验收**: 样本列表和错误语义一致。
- **Boundless 模块**: `crates/server-runtime/src/web_pages.rs`（叠在
  BW-14-03-02 的 `web_session.rs` 之上）
- **平台**: Linux x86_64

## 原版形状

参考 `programming/fantasydemo/web/php/` 的角色/物品两族页面（服务侧
`web*` 方法不在参考树中——实体脚本是运行时数据，唯一契约就是 PHP 页面，
与 BW-14-03-02 的 `logOn` 同判）：

| 原版位置 | 形状 |
|---|---|
| Items.php:9-88 | `ItemType{id,name,stats,description,iconImgUrl,detailImgUrl}`；`ItemTypeManager` 按 id 注册；`renderItemType`（见偏离 3） |
| Items.php:90-206 + Constants.php:43-50 | 八个样例注册：Staff of Serpents(2)、Staff of Lightning(3)、Drumstick(4)、Spider leg(5)、Binoculars(6)、Sword(7)、Bastard Sword(9)、Goblet(17)，名字/属性表/描述/图标路径逐字 |
| Constants.php:88-93 | `$CHAR_CLASS_NAMES`：ranger→Ranger、female_warrior→Female Warrior、ped_male→Civilian、barbarian→Barbarian |
| Characters.php:22-100 | `initialise`：logout→`Login.php?logout=1`；无 account→"Unable to access account"+`Disconnect.php`；`entityType()=="Avatar"`→`CHARACTER_WELCOME_PAGE_URL`（News.php）；建角（空名 `InvalidFieldError("Character name is empty")`，异常进 "Could not create character: …"，**catch 后无条件 addStatusMsg("Character creation successful")**）；选角（`webChooseCharacter` 异常→`addExceptionMsgAndDisconnect`；成功→"Login successful"+`setEntityDetails(res[type],res[id])`+redirect News.php，**随后仍取角色列表**）；`webGetCharacterList` 异常→disconnect，成功→`res["characters"]` |
| Character.php:33-86 | `webGetPlayerInfo` 异常→disconnect；position 按 `%.01f` 三元组；direction 按 (roll,pitch,yaw) 解包但 **yaw 先印**，`radDegFactor=360.0/(2.*3.1415)`；统计框固定序 Class/Strength 23/Dexterity 15/Defense 21/Intelligence 18/Health `h&nbsp;/&nbsp;max`/Frags/Status，online 才追加 Position/Direction；标题=playerName |
| ViewItem.php:18-51, 54-84 | `item` 参数：空/非数值/负→哨兵 -1；`is_numeric` 语义（前导空白允许、尾随不许、inf/NaN 不算、`(int)` 截断）；目录查不到→redirect `ViewItem?referrer=…`（**原文无 .php 后缀**）；-1→"The item you have chosen is not valid."；命中→标题=物品名 |
| Inventory.php:29-60, 139-211, 275-278 | `webGetGoldAndInventory` 异常→disconnect；回复摊平成 serial 序表+serial 键映射；渲染跳过 `lockHandle != -1` 的行；`Gold: N` 行；`rowNum==0`→"Inventory is empty."；`view_item` serial→`ItemViewer::acquireInfo`（ItemViewer.php:27-57：名字/大图/描述/属性行）；`output` 有分辨率时走 `Image.php?path=…&max_width=0.75*width`（ItemViewer.php:62-73） |
| Page.php:140-177 | `getExceptionMsg`=exception message；`addExceptionMsg(e, info)`→`"info: msg"` 进错误 flash；`addExceptionMsgAndDisconnect`→再置 redirect `Disconnect.php`；`addStatusMsg`/`addErrorMsg` |

实体缝：页面经 `RemoteEntity` 调实体 `web*` 方法——传输边界，照 BW-14-03-02
的 `DbLogOn` 缝建模为 `WebEntityQueries` trait（create/choose/list/
player_info/gold_and_inventory 五法）。

## 端口语义

`web_pages.rs` 按上表落地；`characters_page`/`character_page`/`view_item`/
`inventory_page` 各自复刻 `initialise`+`renderBody` 的数据层语义与错误序。

**验收证据（样本列表一致）**：默认目录八项逐字（id/名/属性序/描述/图标路径，
Binoculars 空属性表、Goblet "+20"、Sword 四属性 "Accuracy 80%/Speed 3s/
Power 40/Type melee" 全钉测）；`CHAR_CLASS_NAMES` 四项钉测；角色列表
(name+charClass) 原序返回；金线 `Gold: 250`、锁定行原位跳过。
**验收证据（错误语义一致）**：空名→`Could not create character: Character
name is empty` 且成功状态仍加（原版 fall-through 怪癖，测钉）；无
account→`Unable to access account`+Disconnect；选角/列表/玩家信息/库存四腿
失败均 `addExceptionMsgAndDisconnect`→Disconnect；ViewItem 无效 id→原文
段落、未知 id→`ViewItem?referrer=…`（无 .php 后缀照抄）；全锁库存→
`Inventory is empty.`；选角成功→`Login successful`+token 换盖
`entities_by_id/Avatar/<id>`+redirect News.php 且列表仍取。

## 有意偏离

1. **XHTML-MP 渲染层未移植**（`xhtmlMp*` 函数族、`Page`/`AuthenticatedPage`
   的 HTML 壳、menubar、样式表选择）：纯表现层，无契约面；端口交付
   initialise/渲染的数据层语义与文案原串。
2. **`$GLOBALS['BW_ITEM_TYPES']` 参考树中从未定义**（ViewItem.php:39 只读
   不写，潜在 bug——原版该页只能永远 redirect）：端口按显然意图查同一
   [`ItemTypeManager`] 目录，缩略图取目录项 `icon_img_url`（原数组键
   `img_href`）。
3. **`renderItemType` 的 `in_array($itemType, $this->itemTypes)`** 把 id 与
   对象数组做宽松比较，PHP 下恒 FALSE：端口按键检查（显然意图）。
4. **PHP 的 `$_SESSION['err_msgs']` 与页内 `$this->errorMsgs` 双写**：端口
   归并为单一会话 flash（BW-14-03-02 已立的口径），键 `err_msgs`/
   `status_msgs`。
5. **无 token 会话上的选角**：PHP 会在 null 实体上 fatal；端口防御性跳过
   restamp 仍报成功并 redirect（测钉）。
6. **Inventory 的 `action:auction` 臂（`webCreateAuction`）未移植**——属
   BW-14-03-04 拍卖工作流（下一行），本项只交付查询半。

## 门禁（本项）

| 门禁 | 结果 |
|---|---|
| `cargo test -p server-runtime` | **1176 passed / 0 failed / 1 ignored**（新增 26 本项） |
| `cargo test --workspace` | **exit 0，4313 passed / 0 failed / 107 ignored**（109 个绿目标；连跑三次计数一致——上一项台账里的 3525 是当时管道截断的少计） |
| `cargo clippy -p server-runtime --all-targets` | 0 finding 落在 web_pages.rs（`RAD_DEG_FACTOR` 与测试方向字面量按原文 3.1415 精度保留，`#[allow(clippy::approx_constant)]` 注明） |
| `rustfmt --edition 2024 --check`（web_pages.rs） | 0 diff（幂等） |
| `web_pages.rs` 行覆盖 | **100.00%**（885 个 DA 行 / 0 missed） |

新增验收测试（26 条，全部锚定原版行号）：类名表、八项目录、空目录+重注册
替换、角色列表名字与类解析、空名拒建但成功状态仍加、实体侧建角失败仍加成功
状态、成功建角、选角换会话+redirect 后列表仍取、选角失败断连、无 account
断连、Avatar 欢迎页重定向、logout 前置、列表失败断连、统计框原序（含
`(yaw=90.0, pitch=30.0, roll=180.0) [deg]` 度数换算钉测）、离线省略
Position/Direction、玩家信息失败断连、PHP 数值强制九例（含 ` 6`/`6 `/`5.9`/
`inf`/`NaN`）、ViewItem 命中/未知 redirect（含无 referrer 与超 u32 id）/
无效、viewer 解析+0.75 分辨率缩放（800→600、333.5→250.125）、库存金线+
锁定跳过、全锁空单、库存失败断连、viewer 五例（命中/未知 serial/非数值
serial/无参数/类外 item_type）、无 token 选角。

## 状态

已验收：样本列表和错误语义一致——八项物品目录与四项类名表逐字落地，角色
列表/玩家统计/库存行按原版序与格式渲染，五腿查询的失败路径全部走原版
`addExceptionMsgAndDisconnect` 语义与原文案，ViewItem 的数值强制与未知 id
重定向按 PHP 原语义钉死。
