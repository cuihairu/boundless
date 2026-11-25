# BW-10-05-05 验证旧导航资产读取（固定地图可寻路）

**验收**：固定地图可寻路。

## Harness 设计（两级、两个二进制）

05-04 已交付的 `gen_generate.cpp` 驱动**未改动**的原版
flood+generator+view 管线，其终流（streamline + calculateSetMembership 之后的
`asBinary`）正是 navgen 存进 `.cdata/worldNavmesh` 的资产字节。本项把这两个终流
以文件形式交给读侧 harness：

1. **`gen_generate.cpp`（生成侧）**：尾部追加一段 `fwrite` 把
   `finalStream` 写成 `./slope.navmesh`（136B）/ `./shell.navmesh`（152B），
   **故意不带任何 stdout 输出** —— 重建后 `cmp` 对已提交的
   `gen_generate.log` 逐字节相同，05-04 的真值不受影响（资产中间文件与
   二进制一样不入库；字节本体住在 log 的 HX 十六进制转储里）。
2. **`gen_navpaths.cpp`（读侧，新）**：为什么是独立二进制——生成侧把
   `waypoint_view.cpp` 等三个源文件直接编进来，而读侧要链 el7 的
   `libwaypoint.a`，两边各有一份 `waypoint_view.o`，符号冲突。读侧只走
   原版读取面，不需要生成器类，因此独立编译、只链 el7 库。

读侧按 `navmeshFactory` 的脸走（chunk_waypoint_set_data.cpp:325，注册于
:26-27，工厂循 :410）：手工读 16 字节头（version/girth/nwp/ne，
MF_ASSERT version==0），`new ChunkWaypointSetData(base)`、`girth()` 先设、
`readWaypointSet`（:262）吃剩余字节，base 逐记录累加；随后按
`gen_waypoints.cpp` 的 chunk 装配方（ChunkSpace + GeometryMapping + Chunk +
ChunkWaypointSet + ChunkNavigator::add）挂载，跑 12 个定点
CONTAINS/FIND 层探针和 7 对起终点的原版 AStar 寻路，打印
`gen_navpaths.log`（85 行）。

## 产物对照

| 资产 | 大小 | md5 | 记录数 |
|---|---|---|---|
| slope.navmesh | 136B | `30bd31c21138de0e2bc6815e66979c72` | 1（version 0，girth 0.8，2 wp / 8 edge） |
| shell.navmesh | 152B | `d8b632797387862b8ae69d66aca8efa9` | 2（各 version 0 girth 0.8，各 1 wp / 4 edge） |

（中间 `.navmesh` 文件不入库；测试经 `final_stream()` 从
`gen_generate.log` 的 `<scene> bytes N` + HX 行重新导出字节并校验声明
长度，不依赖未提交二进制。）

真值要点（gen_navpaths.log）：

- slope 一记录两多边形：P0 x∈[3,7] minh 1.5/maxh 3.5、P1 x∈[0,3]
  minh 0/maxh 1.5，共享边互指 0001，其余 FFFF —— 与 05-04 SETMEM 逐边吻合。
- shell 两记录：地板 set minh=maxh=0.5、桥 set minh=maxh=1.5，
  **中心同为 (3, 3.5)**，层选择只能靠高度带；两集合所有边均 FFFF。
- 7 条参考路径：S1 两步 (1,0.51,1) wp1 → (3,3.5,3) wp0；S2 两步反向；
  S3/S4/F1/B1 单步；**X1 PATHNONE**（层间无邻接，搜索失败）。

## 移植面（测试 `crates/waypoint/tests/gen_navmesh_paths.rs`，3 测试）

1. **读取面**：`parse_world_navmesh` 解出的记录数、girth、wp/edge 数、
   每条 LWP（高度带/边数/中心）与每条 LEDGE（顶点坐标 + 邻接编码
   FFFF/0001/0000）对真值逐项全等。
2. **层探针**：每 set 的 `contains`（首 wp）与 `find(point, false)`
   对照 24 对 CONTAINS/FIND 真值 —— shell 中心相同仅高度带不同，层选择
   全靠 minh/maxh。
3. **寻路面**：7 对探针先复算 `NavLoc` 构造解析（+0.01 抬升、含高
   containment 先、find-closest 兜底）比对 PATHBEGIN 的 src/dst wp，
   再逐点重放每条 PATH 步（1e-6 容差），X1 必须同样 `!searched`。

## 登记边界（怪癖）

- **isGoal 打在穿越态上**（chunk_waypoint_state.hpp:48
  `waypointsEqual`）：AStar 命中的是*跨越边界的那一刻*，打印路径的
  末步是穿越点而非终点（S1 止于 (3,3.5,3) wp0）。重放严格比对参考
  打印的步序列，不自行补终点。
- **NavLoc 层选择只靠高度带**（navloc.cpp:31，+0.01 抬升 :39-41，
  navigator find :49）：shell 两记录中心相同，地板点落 set 0、桥点落
  set 1 全由 minh/maxh 决定。
- **CONTAINS/FIND 语义差**：slope set0 在 (1,0.5,1) 上 CONTAINS=0 而
  FIND=1 —— contains 是严格含高判定，find 命中即返回（含 wp 内）。
- **shell 层间天然断链**：calculateSetMembership 未给出任何 set 间
  邻接（两 set 全 FFFF 边），跨层探针 X1 是真 PATHNONE，端口必须同样
  `!searched`。
- **128× incRef 钉住**：原版 AStar free list 会把回收的搜索态析构两次
  （gen_waypoints.cpp 同款处理），harness 侧照抄钉住集才能跑完。
- **两级 harness 是链接约束不是设计偏好**：生成侧直编
  waypoint_view.cpp 与 el7 libwaypoint.a 的 waypoint_view.o 冲突；
  读侧只链 el7 库（`-lwaypoint -lchunk -lscene -lresmgr -lmoo
  -lmath -lphysics2 -lterrain -lcstdmf -lpng -lz`）。
- **生成器尾部加 fwrite 不动 stdout**：`cmp` 对旧 log 全等后才提交，
  05-04 证据链保持原值。

## 参照源码

| 面 | 位置 |
|---|---|
| navmeshFactory 注册 | lib/waypoint/chunk_waypoint_set_data.cpp:26-27 |
| navmeshFactory（16B 头 + readWaypointSet 循环） | 同 :325（循内调用 :410） |
| readWaypointSet | 同 :262 |
| NavLoc(ChunkSpace*, point, girth)（+0.01 / navigator find） | lib/waypoint/navloc.cpp:31（抬升 :39-41） |
| isGoal（穿越态 waypointsEqual） | lib/waypoint/chunk_waypoint_state.hpp:48 |
| AStar::search 契约 | lib/waypoint/astar.hpp:21 |

## Harness 复现

```sh
# 1) 生成侧（/tmp/genstub 桩面同 05-04；产物写 ./<scene>.navmesh）
g++ -std=c++14 -O2 -o gen_generate gen_generate.cpp \
  /tmp/genstub/waypoint_generator/waypoint_flood.cpp \
  /tmp/genstub/waypoint_generator/waypoint_generator.cpp \
  /tmp/genstub/waypoint_generator/waypoint_view.cpp \
  -I /tmp/genstub -I /tmp/genstub/waypoint_generator -I $BW/lib \
  -DCODE_INLINE -D_RELEASE -DHANDLE=void* -DINVALID_HANDLE_VALUE=NULL \
  -DBW_GUARD= $BW/build/el7/lib/libcstdmf.a $BW/build/el7/lib/libmath.a \
  -lpthread
./gen_generate > gen_generate.log        # cmp 对已提交版必须全等

# 2) 读侧（只链 el7 库；读 ./slope.navmesh ./shell.navmesh）
g++ -std=c++14 -O2 -o gen_navpaths gen_navpaths.cpp \
  -I $BW/lib -DCODE_INLINE -DCSTDMF_IMPORT -DMF_CONFIG='"hybrid"' \
  -DMF_SERVER -DMF_USE_ASSERTS -D_HYBRID -DBW_BUILD_PLATFORM='"el7"' \
  -L $BW/build/el7/lib \
  -Wl,--start-group -lwaypoint -lchunk -lscene -lresmgr -lmoo \
  -lmath -lphysics2 -lterrain -lcstdmf -lpng -lz -Wl,--end-group \
  -lpthread
./gen_navpaths > gen_navpaths.log
```

## 测试与门禁

- `cargo test -p waypoint`：104/104（lib 84 + navmesh_bin 9 +
  gen_navmesh_paths 3 + cross_chunk_search 8）。
- `cargo test --workspace`：4419 通过 / 0 失败（4416 + 3）。
- `cargo fmt -p waypoint -- --check` / `cargo clippy -p waypoint
  --all-targets -- -D warnings`：绿。
- llvm-cov（--lcov，清 target 后全量重测）：waypoint crate 全部
  源文件 ≥98.32%（最低 chunk_waypoint_set_state_path.rs 98.32%、
  astar.rs 98.88%，其余 ≥99.09 或 100%）—— 本项只新增集成测试未动
  库代码，既有覆盖不降。
