# BW-14-03-04 移植拍卖工作流 — 复核证据

- **任务编号**: BW-14-03-04（todo.md 行 1764）
- **验收**: 提交和查询状态匹配。
- **Boundless 模块**: `crates/server-runtime/src/web_auctions.rs`（叠在
  BW-14-03-03 的 `web_pages.rs` 与 BW-14-03-02 的 `web_session.rs` 之上）
- **平台**: Linux x86_64

## 原版形状

参考 `programming/fantasydemo/web/php/` 的三族拍卖页面。服务侧 `web*` 方法
不在参考树中（实体脚本是运行时数据，唯一契约就是 PHP 页面，与 BW-14-03-02/03
的判同）：

| 原版位置 | 形状 |
|---|---|
| Inventory.php:72-133 | `action:auction` 臂：`item_select` 缺失→`addStatusMsg("No item selected for auctioning.")` **仍渲染**；`(int)` 强转；`auctionBidPrice <= 0`→"Invalid starting bid price."；**buyout 缺失/非正→静默 -1**（`Inventory.php:94-98`）；`auctionExpiry <= 0`→"Invalid expiry time."；`webCreateAuction` 异常→`addExceptionMsg` 不 redirect；**创建在前**（`:107-113`）、**名字解析在后**（`:122-124`，查不到即在 null 物品上 fatal）；成功→`addStatusMsg("Auction of <name> created.")`+redirect `PlayerAuctions.php?auction_id=<urlencode(id)>` |
| PlayerAuctions.php:34-92 | `initialise`：`webGetPlayerInfo`+`webGetGoldAndInventory`+`AuctionHouse.webGetTime` **三腿共一个 catch**→`addExceptionMsgAndDisconnect`；`doGetPlayerAuctions`：`webCreateSellerCriteria` 失败→disconnect；`webSearchAuctions` 失败→**只 `addExceptionMsg` 后 return**（`:83-87`，页面以空单渲染）；`webGetAuctionInfo` **无 catch**（`:88-91` 裸调）；空→`PLAYER_NO_AUCTIONS` 段；行内 `timeFloatToString(expiry - officialTime)`、`buyoutPrice == FALSE`（含 0）→`-` |
| SearchAuctions.php:436-521 | `initialise`：玩家信息失败→disconnect；金价查询裸调；`action` 为 `bid`/`buyout`→先处理**后无条件 redirect** 回检索参数；`doSearchAction` 结果按"我的竞拍"/"在售"两表拆分 |
| SearchAuctions.php:55-110 | `doBidAction`：`(int)$bidAmount <= 0`→"Bid amount is invalid."；`webBidOnAuction(auctionID, false, bidAmount)` 异常→`addExceptionMsgAndDisconnect`（`:75-79`）；`doBuyoutAction`（`:85-110`）以 `bidAmount=0`、`isBuyingOut=true` 提交 |
| SearchAuctions.php:114-373 | `doSearchAction`：`eregi` 目录名匹配→`webCreateItemTypeCriteria`（**唯一 disconnecting 腿** `:162-167`）；空串价→`-1` 未设（`:186-201`）否则 `(int)`→`webCreateBidRangeCriteria`，与已有条件 `webCombineAnd`；`webCreateBidderCriteria(playerDBID)`；**无类型条件时** `search_submit`→裸检索（全部）否则检索自己的竞价；**有**条件时 `webCombineOr(bidder, criteria)`；`webSearchAuctions`/`webGetAuctionInfo` 失败→`addExceptionMsg` 后 return（`:138-147` 里 disconnect 臂是注释掉的 TODO、`:153-162`） |
| SearchAuctions.php:289-310 | 过滤：`currentBid == buyoutPrice`→跳过（`:298`）；**`webGetTime` 在循环内**（`:305`）；`expiry < officialTime`→跳过 |
| SearchAuctions.php:325-372 | 卖家名查 `entities_by_id/Avatar/<dbid>`（每 dbid 一次）；失败→`addExceptionMsg` 后 **return，不做拆分**（`:334-352`）；随后 `bidders` 含自己→竞拍表，**卖家是自己→整条丢弃**，余下→在售表（`:352-371`） |
| SearchAuctions.php:495-516 | `auction_select` 空→"Selected auction is invalid."**无 return**（竞价照常试）；`:486-516` 无条件 redirect `SearchAuctions.php?search_type_name=…&search_min_bid=…&search_max_bid=…&search_submit=`（submit 按钮无 value→第四参数恒空，逐项 `urlencode`） |
| SearchAuctions.php:574-588, 651-790 | 空段三条文案；结果表 `currentBid > availableGold`→单选框 `disabled`（`:685,700-706`）；竞拍表状态格 `:757-771`：`highestBidder == 自己`→"You are the highest bidder"+`Max bid: N`，否则"You have been outbid"，第三臂 `&nbsp;` 不可达 |
| Util.php:29-104 | `timeFloatToString`：单位链 `(60,"sec")(60,"min")(24,"hr")(7,"day")`，`(int)` 截断，**`$timeMod > 1` 才加 s**（故 `45 secs`），零模不前置，`$timeDiv == 0` 即 break，负号前置；**无粒度参数时一周渲染成 `0 sec`**（天单元溢出） |

## 端口语义

`web_auctions.rs` 三个入口 + 两个工具函数，全部按上表逐句落地：

- `inventory_auction_action`（`Inventory.php:72-133` 臂，用 03-03 的
  `InventoryRender.items` 传入已取库存）
- `player_auctions_page`（`PlayerAuctions.php`）
- `search_auctions_page`（`SearchAuctions.php`，含 `do_bid_action` /
  `do_buyout_action` / `do_search_action` / `build_render`）
- `time_float_to_string`、`php_str_to_int`、`php_urlencode`

实体缝沿用 BW-14-03-02/03 口径：玩家实体经扩展后的
`WebEntityQueries`（新增 `web_create_auction` / `web_bid_on_auction`，
`PlayerInfo` 增 `database_id`），`global_entities/AuctionHouse` 是第二实体故
单立 `AuctionHouseQueries`（9 法：time/seller/bidder/item-type/bid-range
criteria + combine and/or + search + auction info），卖家名查询另立
`AvatarLookup`。

**验收证据（提交和查询状态匹配）**：

- *提交*：`create_auction_announces_the_item_name` 逐字比对捕获的
  `AuctionSubmission{1000, 3600, 10, 250}` 与 `Auction of Sword created.`
  状态串、redirect `PlayerAuctions.php?auction_id=33`；buyout 缺失/`0`/`-5`
  三例捕获的 `buyout == -1` 逐例钉死；`bid_action_records_the_submission_and_redirects`
  与 `buyout_action_bids_zero_with_the_flag` 逐字比对捕获的
  `BidSubmission{5,false,100}` / `{7,true,0}` 与状态/错误串。
- *查询*：`typed_criteria_combine_in_reference_order` 钉死整条调用序
  `create_item_type([7, 9]) → create_bid_range(10, 50) → combine_and(1, 2) →
  create_bidder(7) → combine_or(5, 3) → search(Some(Criteria(6)))`；
  `the_bid_range_alone_becomes_the_criteria` 钉 `(10, -1)` 边界与 `combine_or(5, 2)`；
  `bought_out_and_expired_auctions_drop_out` 五路输入（已买断/过期/自己挂单/
  自己竞价/超预算）钉出 `results == [5]`、`bids == [4]`、状态
  `HighestBidder{120}`、`affordable == false`、`time_calls == 5`（循环内 4 次
  + 渲染 1 次）。

## 有意偏离

1. **XHTML-MP 渲染层未移植**（`xhtmlMp*` 函数族、`recommendedBid =
   min(currentBid+1, 1.05*currentBid)` 的展示公式、单选框 disabled 属性、
   `Max bid: N` 的断行）：纯表现层无契约面；端口交付数据层语义与文案原串
   （两表各自的行结构已带 item 名/图标/卖家名/到期行/当前价/买断价/状态）。
2. **`eregi` 正则未建模**：端口做大小写不敏感子串匹配（样例流程用的都是
   单词检索），POSIX 类与交替式不模拟。
3. **原版会 fatal/报错页的三处改成防御性终止**：`Inventory.php:122`
   名字解析在 null 物品上 fatal→端口 `UnknownItem`（**拍卖已提交**这一原版
   事实保留并测钉）；`PlayerAuctions.php:88-91` 的 `webGetAuctionInfo` 无
   catch→端口防御性断连（测钉）；`SearchAuctions.php:305` 循环内
   `webGetTime` 失败会成 PHP 报错页→端口保留上一次时间戳。
4. **`BidStatus::Other`（`&nbsp;` 第三臂）保留为死臂**（`#[allow(dead_code)]`）：
   竞拍行只可能来自"自己竞价过"的拍卖，参照的三臂形状照抄。
5. **`SearchAuctions.php:495` 无 return 与 `:486-516` 无条件 redirect** 两处
   怪癖照抄（测钉：失败的竞价仍回检索参数 redirect，且断连 redirect 被覆盖）。

## 门禁（本项）

| 门禁 | 结果 |
|---|---|
| `cargo test -p server-runtime` | **1215 passed / 0 failed / 1 ignored**（新增 39 本项） |
| `cargo test --workspace` | **exit 0，4352 passed / 0 failed / 107 ignored**（109 个绿目标） |
| `cargo clippy -p server-runtime --all-targets` | 0 finding 落在 web_auctions.rs / web_pages.rs |
| `rustfmt --edition 2024 --check` | 0 diff（幂等） |
| `web_auctions.rs` 行覆盖 | **100.00%**（1604 个 DA 行 / 0 missed） |
| `web_pages.rs` 行覆盖 | **100.00%**（896 / 0 missed，缝扩展后仍满） |

新增验收测试（39 条，全部锚定原版行号）：`timeFloatToString` 单位链十例（含
`45 secs` 的 PHP 复数怪癖、一周→`0 sec`、负号）、`(int)` 强转八例（含 `i64`
上下钳）、`urlencode` 四例、买断判定四例、玩家拍卖页四腿断连 + 信息腿防御断连 +
检索失败只报错 + 行渲染（到期行/买断 `-`/零买断）+ 空单、创建拍卖七例（无物品/
价四例/期两例/买断默认三例/**创建先于名字解析**/成功含 redirect 与状态串/目录
外空名/实体拒绝）、竞价八例（成功/金额三例/无选中仍竞价/买断零额/买断拒绝/
失败仍 redirect/未知 action 透传/重定向 urlencode）、检索条件树六例（顺序/
仅价区间/名称匹配四例/类型腿断连/两腿断连/六种后续腿报错渲染空）、过滤与拆分离
四例（买断过期自己挂单自己竞价超预算/被超越/循环内时钟失败/卖家名缓存与查名失败
不拆分）、可负担性。

## 状态

已验收：提交和查询状态匹配——建拍与竞价提交按原版参数与顺序捕获，检索条件树
按原版逐法逐参调用，结果按原版过滤与拆分开表，全部文案与错误语义同原版。
