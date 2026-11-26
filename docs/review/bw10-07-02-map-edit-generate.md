# BW-10-07-02 执行地图编辑和生成

todo.md:1531。验收条件：「新版导出后可被基准目标消费」。

交付日期 2026-11-25，branch `lane/bw10-terrain`。

## 范围

参照流水线 = navgen 的每-chunk 面（`chunk_waypoint_generator.cpp:139-194`）：
洪泛（`ChunkFlooder::reset`，`chunk_flooder.cpp:98-110`）→ `gener.init(w, h, flooder.min(),
resolution)` → 16 层高邻接网格拷贝 → `generate()` → `determineEdgesAdjacentToOtherChunks()` →
`streamline()` → `extendThroughUnboundPortals()` → `calculateSetMembership()` → `asBinary`/`saveOut`。

`ChunkFlooder::reset` 两处硬编码是固定图的尺度来源：`reso = 0.5f`，且洪泛区域是 chunk 的
bounding box 每侧外扩 `reso * 2 = 1.0`（`setArea` 再叠加 `HACK_OFFSET 0.1`）。因此洪泛区
严格大于 chunk 所有区，接缝两侧都会把外沿标成另一个 chunk 的边。

固定图是 07-01 手导图的 ×10（与 07-01 的坐标族连续，且 0.5 网格、0.6 girth 全部逐字取
navgen 的硬编码值）：

- 世界 [0,10]²，地面 y=4.0
- 孔洞：void 带 x∈[2,3) z∈[0,5)（西、东两片地面夹着，洪泛绕不过去）
- 坡：x∈[0,5) z∈[5+0.5k, 5.5+0.5k)，k=0..9，高 4.5+0.5k（0.5 步——半数是 f32 精确值，
  0.6 步会在 `fabs(h − src.y) <= maxStep` 上因舍入失配）
- 障碍：slab x∈[6.5,8.5) z∈[4,6) h13（高出地面 9 ≫ maxStep 0.6，洪泛进不去 ⇒ 障碍是「缺席」）
- chunk A bb [0,5]×[0,10] 所有 x∈[0,5)，chunk B bb [5,10]×[0,10] 所有 x∈[5,10)
- 物理解析：全图 slab（每个 chunk 的洪泛带 ±1m 重叠带覆盖），种子 = 实体位姿 + (0,1,0)：
  mapA (1,6,1)、mapB (6,6,2)；maxStep 0.6、scramble 0.5、girth (0.5,1.5,0.5)

## 参照侧产物（已在 `docs/review/fixtures/waypoint/`）

| 文件 | 内容 |
| --- | --- |
| `gen_navgen_map.cpp` | 生成 harness：SlabPhysics（与 `gen_generate.cpp:47-102` 同面）+ `fillWorld` + `runChunk` 逐段打印 |
| `gen_navgen_map.log` | 3710 行真相：`FILL 0 count 200/204`、H/A/B 网格倾倒、`SEEDAREA min -0.9/-0.9 与 4.1/-0.9 size 15 25`、GENERATED 7/9 polys、DETERMINED、STREAMLINED、SETMEM sets 1、`mapA bytes 496 fnv 3f4c4a26e9865b1f`、`R 0 version 0 girth 0.6 nwp 7 ne 33`（mapB 640B / fnv c9b5fe1853f86a48 / nwp 9 ne 43）、`WROTE`、`RESULT OK` |
| `world_navmesh_genmap_a.bin` | 496 B，md5 `f77acd44c519b90d926d700526e1577a`（参照写出的 mapA 导出字节） |
| `world_navmesh_genmap_b.bin` | 640 B，md5 `e0c5fc7f1c10a2a78bcef3c6d8728ee7` |
| `gen_navgen_paths.cpp` | 消费 harness：原版 `ChunkWaypointSetData::readWaypointSet` + `NavLoc` + `AStar` |
| `gen_navgen_paths.log` | 162 行真相：LHDR/LWP/LEDGE（34 条 `FFFF`）、18 条 CONTAINS/FIND、9 条 NAVLOC、4 组路径探针 |

参照拓扑（DETERMINED 段的接缝标记各 4 条）：mapA 的 P3 V3 (5.1,2.6) 与 P4 V6 (5.1,5.6) 标
another-chunk；mapB 的 P7 V1 (4.6,…) 与 P5 V1 (4.6,…) 标 another-chunk——两侧的 ±1m 重叠带
各标一次，符合 `ChunkFlooder::reset` 的真实行为。

## 验收三腿

1. **生成重放**：`crates/waypoint-gen/src/generator/navgen_map.rs` 用 Rust 洪泛 + Rust BSP
   生成器按参照顺序跑两个 chunk，按 harness 的格式逐行打印，与 `gen_navgen_map.log` 的 3710
   行逐行相等；导出 fnv 3f4c4a26e9865b1f / c9b5fe1853f86a48 == 上面两个 .bin 的 fnv。
2. **字节验收（基准目标消费）**：把端口导出字节写成 `./mapA.navmesh` + `./mapB.navmesh`，
   交给原版 `gen_navgen_paths`（el7 库链接，读入端不碰生成器类）跑，stdout 与
   `gen_navgen_paths.log` 逐字节 LOG-IDENTICAL，`RESULT OK`。
3. **端口读入端重放**：`crates/waypoint/tests/genmap.rs` 解码两个 .bin，逐条比对 LHDR
   （girth 0.6 / 7 与 9 waypoints / 33 与 43 edges）、16 条 LWP、76 条 LEDGE（含 34 条
   `FFFF` 邻域原码）、18 条 FIND、9 条 NAVLOC、3 组路径逐步（ACROSS 4 态、RAMP 3 态、
   AROUND 4 态）与 SEAM 的 PATHNONE。

`cmp` 另证参照写出的 mapA/mapB.navmesh 与端口导出字节逐字节相同。

## 新增代码

- `crates/waypoint-gen/src/generator.rs`
  - `pub trait GenChunk { owns(&Vector3) -> bool; local_bb() -> (Vector3, Vector3) }`
  - `to_vector3`（`:2343-2360` 面）：网格坐标 × resolution + gridMin；高度取第一个落在该
    poly 带内的层高，否则 `max_height + 0.01`；参照结尾的
    `transformInverse().applyPoint` 因 navgen 的 identity 变换省略。
  - `determine_edges_adjacent_to_other_chunks`（`:2628-2718` 面）：先清 `adj_to_another_chunk`
    （顶点共享）；巨大 poly 的四外角 outsides 位检查（1/2/4/8，==15 全标并 continue）；逐边
    跳过 `adj_nav_poly > 0`，两端点 `to_vector3` 后抬到 `max_height + 1` 由
    `find_drop_point` 覆写 y，`!owns(p1) && !owns(p2) && !owns((p1+p2)/2)` 才标。
    参照结尾的 polygonPortalParity 警告循环是死代码（`polygonPortalParity` 从不递增，`:2640`），
    端口只留活的部分。
- `crates/waypoint-gen/src/generator/navgen_map.rs`（测试）：固定图物理 + 半开区间 stub
  chunk（与 harness 编译的 shadow `chunk/chunk.hpp` 同面）+ `run_chunk` 全流程 + 逐行断言。
- `crates/waypoint/tests/genmap.rs`（测试）：读入端重放，见上。

## 登记边界

- **`extendThroughUnboundPortals`（`waypoint_generator.cpp:2370-2607`）未移植**：该面只对带
  未绑定 extern 门户的 chunk 生效；本夹具两个 chunk 都 bound（`bounds()` 为空，同 navgen
  `canProcess` 的 3×3 邻域保证），参照调用是 no-op，故省略并登记，不做只移植门禁的缩水面。
- **`NavLoc` 的 girth-grid 环形搜索未建模**：`chunk_navigator.cpp:156-232` 带「上一圈找到即停」
  的提前终止，端口只建模了高度带 containment 与跨 set 共享距离的 closest 回退。真相日志里
  两个 containment 未命中的探针（孔洞中心 (2.5,4,1)→wp3、障碍中心 (7.5,4,5)→wp8）按记录的
  环答案断言（`RING_ANSWERS`），不自行重算。
- **SEAM 探针 PATHNONE**：65535（another-chunk）边不接线，由 07-03 的门户绑定面接通。
- **消费者 waypoint id 是 set-local**：harness 把两个 set 都挂到一个 chunk 上，`waypoint()`
  返回的是各自 set 内的 0 基索引（已实证）。
- 洪泛区的 ±1m 重叠带是参照的真实行为，两侧各标一次接缝边，不是端口偏差。

## 门禁

- `cargo test -p waypoint-gen`：69 passed / 0 failed
- `cargo test -p waypoint`：9 + 2（genmap）passed / 0 failed
- `cargo test --workspace --no-fail-fast`：4479 passed / 0 failed（115 个 target）
- `cargo fmt -p waypoint-gen -p waypoint -- --check`：clean
- `cargo clippy -p waypoint-gen -p waypoint --all-targets -- -D warnings`：clean
- `cargo llvm-cov clean` 后 `cargo llvm-cov -p waypoint-gen`（行覆盖）：
  generator.rs 98.55%、generator/navgen_map.rs 99.55%、flood.rs 99.19%、
  flood/artifacts.rs 100%、flood/covers.rs 100%、generator/artifacts.rs 100%、
  generator/roundtrip.rs 100%、view.rs 100%、view/artifacts.rs 100%；TOTAL 99.31%

generator.rs 未覆盖的新代码行全部是参照的死臂/本夹具不可达臂：`:1855` to_vector3 的
`max_height + 0.01` 兜底（本夹具每个顶点都命中带内层高）、`:1892/1895/1898/1901` 巨大 poly
四外角位（从未四角齐）、`:1905/1906/1909` 巨大 poly 全标臂与 continue、`:1930/1933`
`find_drop_point` 失败臂（本世界每点都有地面）、`:1940` 该臂的收尾括号（结构行）。
余下未覆盖行（`:114/735/815/817/841/843/861/862/1492`）是此前交付即存在的臂，与本次无关。
