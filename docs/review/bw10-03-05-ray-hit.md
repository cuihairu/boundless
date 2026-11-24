# BW-10-03-05 实现射线命中 — 复核证据

- **任务编号**: BW-10-03-05（lane `lane/bw10-terrain`，todo.md 行 1359）。
  **验收条款**: 首个交点、法向和材质对照。
- **参照权威树**: `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版源码**:
  - 射线主走查: `lib/physics2/bsp.cpp:1865-1997`
    `BSPTree::intersects(start, end, dist, ppHitTriangle, pVisitor)`
    —— 迭代 LIFO 栈（`BSPStackNode{pNode_, eBack_, sDist_, eDist_}`），
    种子 `(pRoot_, -1, 0, 1)`；叶（无子）`sBack=eBack=-2` 只扫描。
  - 节点命中面: `bsp.cpp:221-247`
    `BSPNode::intersectsThisNode(start, dir, dist, ppHitTriangle,
    pVisitor)` —— 连续扫 `i < numTriangles_+numSharedTriangles_`
    （自有 run + shared run），`collisionFlags()==TRIANGLE_NOT_IN_BSP`
    跳过、拒绝时存/还原 `dist`，每个候选问 visitor
    （`!pVisitor || pVisitor->visit(candidate, dist)`），命中即写
    `ppHitTriangle`、返回 bIntersects（**不在首命中处 break**——
    接受条件 `0 < distanceToPlane < length` 保证 closest-so-far
    胜出）。
  - 容差: `bsp.cpp:85` `const float BSPNODE_TOLERANCE = 0.01f`。
  - 平面交: `lib/math/planeeq.ipp:264-275` `intersectRayHalf` ——
    `isZero(normalDotDir)` ⇒ 返回 `0.f`（避除零），否则
    `(d_ - normal_·source)/normalDotDir`。
  - 三角形命中: `lib/physics2/worldtri.cpp:208` `EPSILON=0.000001`、
    `worldtri.cpp:211-280` `WorldTriangle::intersects(start, dir,
    length)` Möller–Trumbore 双面（无背面剔除）；`almostZero`
    排他窗 `lib/stdmf.hpp:616-619` ⇒ `-EPSILON < det < EPSILON`
    拒绝；接受条件 `0 < distanceToPlane < length`（ Behind-origin
    与超限都拒绝），命中窄化 `length`。
- **Boundless 模块**: `crates/physics/src/bsp.rs` ——
  `BspTree::intersects(start, end, &mut dist, visitor) ->
  Option<&WorldTriangle>`（返回值替代参考的
  `const WorldTriangle** ppHitTriangle` + bool 出参形态）、
  `intersects_this_node`、`StackNode` 栈、
  `pub const BSPNODE_TOLERANCE: f32 = 0.01`、
  `pub trait CollisionVisitor { fn visit(&mut self, hit_triangle:
  &WorldTriangle, dist: f32) -> bool; }`；三角形 MT 在
  `crates/physics/src/world_triangle.rs`（BW-10-03-04 已交付，
  本任务被射线面全程调用）。

## 移植语义（逐句核对）

1. **首命中/最近者胜**: 节点扫走只在与运行 `dist` 比较（接受窗
   `0 < distanceToPlane < length`）后窄化——首个交点即最近交点；
   `ppHit` 逐接受更新，无 break。移植端 `dist: &mut f32` 既是
   入参搜索上限又是出参命中分数（参考同形），命中返回
   `Some(&WorldTriangle)`，MISS 返回 `None` 且 `dist` 保持原值
   （真值行 MISS seed==dist 钉死）。
2. **走查分支逐面**（bsp.cpp:1865-1997 逐句）:
   - **首见分支**（`eBack==-1`）: 容差扩端——
     `tolerancePct = BSPNODE_TOLERANCE / delta.length()`，sOut/eOut
     是 `start + delta*(sDist - tolerancePct)` /
     `start + delta*(eDist + tolerancePct)` 的 distanceTo；
     `sBack = (sOut < 0)`，startSide = `(&pFront_)[sBack]`。
   - **同侧分支**（`sBack==eBack`）: 仅当 `|sOut|<TOL ||
     |eOut|<TOL` 才推对侧记录（容差边缘滑入对侧子树），然后推
     startSide `(sDist,eDist)` 继续——**本节点的自有 run 不扫描**。
   - **异侧分支**: `iDist = intersectRayHalf(start,
     normal·delta)`（平行 ⇒ 0）；startSide 在前 ⇒ 推
     `(node, eBack, sDist, eDist)` 再推 `(startSide, -1, sDist,
     iDist)` 继续；否则落穿到扫描。
   - **回访分支**: `eBack = cur.eBack_`（参考此处对 sBack 的写
     死值从不被读——移植端不绑定并注释登记）、重算 `iDist`、
     落穿到扫描。
   - **节点测试尾**（bsp.cpp:1985）: 命中且
     `dist <= eDist + tolerancePct` ⇒ 立即返回；否则还原
     dist/hit 并推 end side（`eBack>=0` 时）`(iDist, eDist)`。
3. **同侧下滑引擎盲区（本任务最重要的登记）**: 垂直射线（x/y
   恒定）永不穿越根平面 x=0 / node1 平面 y=1 ⇒ 走查全程同侧，
   **同侧下滑不扫任何祖先 run** —— 几何上穿过 t0/t1/t2 的射线
   也 MISS。这不是移植 bug 而是引擎原样行为：LG 四条射线全部
   MISS（真值行），并由 gdb 断点日志（断在
   `BSPNode::intersectsThisNode` 精确符号上打印每节点
   plane/nTri/nShared/进入点，`finish` 取判定）逐一验证走查只进
   叶节点。夹具注释与真值行按真实行为重写钉死。
4. **visitor 面**: 参考 `CollisionVisitor::visit(candidate, dist)
   -> bool`，拒绝 ⇒ 还原 dist、跳过 ppHit 写、继续扫描。移植端
   `pub trait CollisionVisitor` 同签名（`&mut dyn` 分发）；测试
   计数 visitor 钉死：拒绝者 `seen` 仍逐候选记录、`dist` 还原、
   后续候选继续被问、最终 MISS。
5. **`ppHitTriangle` 初值面未建模（登记）**: 参考
   `ppHitTriangle` 入参在调用方可预置（个别引擎内调用点把
   预置指针当候选）；树内走查只在接受时覆写、从不读初值，
   本仓无任何树内调用点预置 ⇒ 移植端以 `Option<&WorldTriangle>`
   返回值形态收口，无初值面可失真。
6. **位级真值**: 生成器与移植端同为 `-O0`/未优化 IEEE f32、
   逐运算同序（MT 乘加序、hit point = `start + delta*dist`
   逐分量 mul-then-add、`normal()` `(v2-v0)×(v2-v1)` 参考序）；
   真值以 `%08x` 位串打印，移植端按位比较（`to_bits`）。
   `intersectRayHalf` 平行臂返回 0、`almostZero` 排他窗照抄。

## 边界（本任务未移植，登记）

- **扫掠碰撞**（`BSPTree::intersects(triangle, ...)`
  bsp.cpp:1811、`BSPNode::intersects(triangle, translation)`
  bsp.cpp:372-425、三角形-树/三角形-三角形 worldtri.cpp）与
  **portal** 属 BW-10-03-06/07，本任务不含。
- **`bsptools` 编辑面**（BSPTreeTool 构建/保存）不移植；生成器
  侧仅作夹具产出端。

## 验收测试（夹具非手造）

- **生成器**: `docs/review/fixtures/physics/bsp/gen_bsp.cpp`
  新增 `ray_probe`（原版 `BSPTree::intersects` 直跑，f32 按
  `%08x` 位串打印 seed/dist/hit point/normal/flags；MISS 行打
  seed 与未动 dist），三树共 **16 条 RAY 真值**：
  - BT 9 条（built.bsp）：下行先跨面三角形（flags 0x0100，
    normal (0,-49,0) 位级）后斜面取最近；仅斜面；WATER
    （0x0040）自下而上命中；NOT_IN_BSP 独行 ⇒ MISS；seed 0.45
    命中 0.4167、seed 0.4 钳到 MISS（搜索上限语义）；斜穿跨面
    平面 0.5 分数点 (1,0.5,1.5)；上行自下取底面（双面）。
  - LG 4 条（legacy.bsp）：**全 MISS**——垂直射线不同侧、同侧
    下滑不扫祖先 run 的引擎盲区被真值钉死（见移植语义 3）。
  - UC 3 条（unit_cube.bsp，真场景资产）：顶面命中（normal
    位级）、侧面命中、完全在外 MISS。
  - 四 .bsp md5 与 BW-10-03-04 交付时逐字节不变
    （`64b49ee1…`/`44a33ce1…`/`1425d7cc…`/`add114f0…`），
    `gen_bsp.log` 仅追加 16 条 RAY 行后逐字节可复现（重跑
    md5 校验全 OK）。
- `tests/bsp_bin.rs` 新增
  `the_reference_rays_are_answered_bit_for_bit`（BT 9 / LG 4 /
  UC 3 逐条回放）：`seed.to_bits()`/`dist.to_bits()` 位级断言、
  命中三角 `flags()` 对照（材质+碰撞字节同字）、`normal()` 位级、
  命中点 `s + delta*dist` 位级、MISS/命中形态对照。
- 单元测试（bsp.rs）：`the_ray_walk_answers_the_nearest_hit_first`
  （下行 0.25 先于 0.5、命中点位级、上行取背面）、
  `the_input_dist_is_the_search_limit`（seed 0.1 MISS 且 dist
  原样、seed 0.8 收窄 0.25）、
  `the_ray_misses_through_a_side_it_does_not_touch`（回访分支），
  `a_rejecting_visitor_restores_and_keeps_scanning`（拒绝
  visitor：seen 逐候选、dist 还原 1.0、最终 MISS；接受 visitor：
  首命中即返）、`not_in_bsp_triangles_are_skipped_by_the_ray_walk`，
  外加 `ray_blob` 双旗双三角形叶树构造器。MT 面另有
  `the_ray_test_is_the_reference_moeller_trumbore`（并行窗、
  behind-origin、超限、边界含 u==1、双面）。

## 门禁数字

- `cargo test -p physics`：lib **29** + bsp_bin **7** 全绿。
- `cargo clippy -p physics --all-targets -- -D warnings`：0。
- `cargo fmt -p physics -- --check`：0 漂移。
- `cargo llvm-cov -p physics`：行覆盖 bsp.rs **1172/1178 =
  99.49%**、world_triangle.rs **164/164 = 100.00%**（≥98% 线
  达标）。
- `cargo check --workspace`：全成员通过。

## 状态

待 push 后 CI 复核（与四条跨席基线红对集，见 memory
ci-fleet-gotchas）；BW-10-03-04 的 CI run 45311799331（head
9aa99f8）一并复核。
