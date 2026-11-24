# BW-10-03-07 处理 Portal 通行与坐标变换 — 复核证据

- **任务编号**: BW-10-03-07（lane `lane/bw10-terrain`，todo.md 行 1363）。
  **验收条款**: 开关前后结果与基线一致。
- **参照权威树**: `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版源码**:
  - Portal 构造: `lib/chunk/chunk_boundary.cpp:491-586`
    `Portal::Portal(PlaneEq iplane, Vector3* pointsIn, int nPoints,
    ChunkBoundary* pBoundary, Chunk* pChunk, const std::string&
    chunkName, ChunkSpace* pSpace, const Matrix& mapper)` ——
    init list `internal(false), permissive(true), hasChunkItem_(false),
    collisionFlags_(0), pChunk(NULL), plane(iplane), traverseMark_(-1)`；
    chunkName 分发 ""→NULL、heaven/earth/invasive/extern→哨兵指针、
    其余 `new Chunk(chunkName, pMapping)`；逐点 `Vector2
    next(pointsIn[i].x, pointsIn[i].y); avg += next;` 后 **`avg /=
    float(points.size())`**；`vAxis = plane.normal().crossProduct
    (uAxis)`；`origin = plane.normal() * plane.d() /
    plane.normal().lengthSquared()`（左结合 (n·d)/ls）；lcentre =
    `uAxis·avg.x + vAxis·avg.y + origin`；centre = lcentre；绕向：
    testPlane = 三点 PlaneEq（缺省 SHOULD_NORMALISE：
    `normal_.crossProduct(p1-p0, p2-p0)` 后归一、`d_ = normal_
    ·p0`），n1/n2 归一，`(n1+n2).length() < 1.f` 时
    `reverse(points)`。
  - 开关: `chunk_boundary.cpp:895-909` `Portal::setCollisionState
    (isOpen, collisionFlags)` —— `!hasChunkItem_ && !isOpen` 才建
    PortalChunkItem 入树（障碍**建后不移除**）；随后
    `permissive = isOpen; collisionFlags_ = collisionFlags`。
  - PortalObstacle: `lib/chunk/portal_chunk_item.cpp:44-175` ——
    ctor 算 bb：`ptExtra = plane.normal()*0.10f`，首点
    `bb_ = BoundingBox(pt,pt)` 否则 addBounds，且**恒**
    `bb_.addBounds(pt+ptExtra)`；`buildTriangles`：<3 清空，
    `ptExOri = origin + plane.normal()*0.05f`，pto/ptb 取
    points[0]/[1]，i=2.. push `WorldTriangle(pto, pta, ptb)`（n−2
    扇形）；ray collide：permissive ⇒ false；`tranl = extent −
    source`；逐三角 `float rd = 1.0f` 重初始化，
    `MT::intersects(source, tranl, &rd)`，
    `ndist = sTravel + (eTravel−sTravel)*rd`；onlyLess 且
    `ndist > dist` 续、onlyMore 且 `ndist < dist` 续；
    `dist_ = ndist`；`flags(uint8(collisionFlags()))`（低字节
    截断）；`say = cc_.visit(ob, tri, dist_)`，!say 返 true，
    `onlyLess = !(say&2); onlyMore = !(say&1)`；prism collide：
    `tranl = extent − source.v0()`，swept `intersects(source,
    tranl)`，`ndist = sTravel_`（棱柱面报 sTravel）。
  - ChunkObstacle 基类: `lib/chunk/chunk_obstacle.cpp:50` ——
    两参 invert（`transformInverse_.invert(transform)` →
    XPMatrixInverse 标量伴随）。
  - collideIntoObject: `lib/physics2/sweep_helpers.hpp` ——
    `sTr/eTr = invTransform.applyPoint(sp.csource()/cextent())`；
    `biggestAxisOfDifference(eTr, sTr)`（严格 > 才换轴，z 轴
    **不更新 diff1**）；bloat = `transformRangeToRadius(inv,
    shapeRange) + 0.01f`；`clip(sTr_clipped, eTr_clipped, bloat)`
    false ⇒ 拒；`sTravel = (sTr_clipped[bax]−sTrba)/dTrba *
    fullDistance()`、eTravel 同式；onlyLess 拒 `sTravel > dist`、
    onlyMore 拒 `eTravel < dist`；`start.transform(shape,
    shapeEnd, inv, sTravel, eTravel, dir, sTr_clipped,
    eTr_clipped)`；末问 `collisionFuncs.collide(shape, shapeEnd,
    cs)`。
  - AABB::clip: `lib/math/boundbox.cpp:310-388` —— sMin=0/eMax=1；
    delta = extent−source；`isZero(delta)` 是**精确零**比较
    （stdmf_minimal.hpp:110-139，isEqual(f,0) ⇒ f==0.f），零轴走
    半开面包检查 `src < min−bloat || src >= max+bloat`；
    `mt=(min−bloat−src)/delta`、`Mt=(max+bloat−src)/delta`，
    delta>0 时 sMin=max(sMin,mt)/eMax=min(eMax,Mt)，delta<0 互换；
    `sMin >= eMax` 拒；出参 `source += sMin*delta`、
    `extent += eMax*delta`。
  - SweepShape/SweepParams: `lib/physics2/sweep_shape.hpp` ——
    ray：leader=pt、bb=(pt,pt)、transformRangeToRadius→0、
    transform 用 sTr_clipped/eTr_clipped；prism：leader=v0、
    bb 三顶点、`transformRangeToRadius = trInv.applyVector(range)
    .length()*0.5`、transform 顶点整体平移 dir*sdist、end =
    `tr.applyPoint(v0 + dir*edist)`；SweepParams ctor：
    shapeRange = max−min、shapeRad = range.length()*0.5、
    dir = end−leader、bsource = box.min、bextent = bsource+dir、
    csource = bsource+range*0.5、cextent = csource+dir、
    box.addBounds(destMin/destMax)、fullDist = dir.length()、
    `dir /= fullDist`（倒数乘）。
  - CollisionCallback/CollisionState:
    `lib/physics2/collision_callback.hpp` —— COLLIDE_STOP=0/
    BEFORE=1/AFTER=2/NOT_IMPLEMENTED=4/ALL=3；`visit` 先问
    `doVisit`，NOT_IMPLEMENTED 走 deprecated 面水旗转 ALL 否则
    operator()；缺省 doVisit→4、operator()→STOP；CollisionState
    ctor onlyLess/More=false、s/eTravel=0、dist=−1。
  - 除法形态: `lib/math/vector2.ipp:221-226` 与
    vector3.ipp:356-363 `operator/=` 均为倒数乘（`1.f/s` 先算）。
  - 标量路径确认: el7 构建全库 grep **SSE_MATH3 未定义**（仅
    matrix.ipp/vector4.ipp 的 include 守卫引用）⇒ 库本体编译
    标量分支，与工具 TU 及 Rust 端同为 IEEE f32 同序逐运算，
    位级可比。
- **Boundless 模块**: `crates/physics/src/portal.rs`（Portal 构造、
  set_collision_state、PortalObstacle ctor/buildTriangles/
  collide_ray/collide_prism、collide_into_object、SweepShape/
  SweepParams/SweptShape、CollisionState/CollisionCallback、
  biggest_axis_of_difference）；`crates/foundation/src/math/
  boundbox.rs`（`BoundBox::clip`）；`crates/foundation/src/math/
  vector.rs`（Vector2 Add/Sub/Mul/Div 运算符，Div 倒数乘）。

## 移植语义（逐句核对）

1. **Portal 构造**（chunk_boundary.cpp:491-586 逐句）: 逐点求和
   后 `avg / points.len() as f32`（漏除即 lcentre 错 3 倍——单测
   先暴露、修复后真值 PT 块钉死）；vAxis 法向×uAxis；origin
   (n·d)/ls 左结合；lcentre 三项和；绕向 (n1+n2).length()<1
   反转——真值 PT1 四点保持、PT2 五点保持，UAX/VAX/ORG 位串
   全对。
2. **开关**（chunk_boundary.cpp:895-909）: 关闭且尚无障碍才建
   项；打开只翻 permissive、**不移除障碍**（参考语义：开
   portal 的碰撞应答由 permissive 短路，障碍仍在树里）——真值
   STATE 0/2（开）全 VIS 0、STATE 1/3（关）恢复命中，钉死。
3. **PortalObstacle**（portal_chunk_item.cpp:44-175 逐句）: bb
   三点恒加 `pt+normal*0.10f`（真值 OBBMIN/OBBMAX 位串钉死，
   max.x = 0.1 = ptExtra 贡献）；扇形 n−2 三角（首版误写 n−1，
   真值 VST 计数裁决改之）；ray 的 rd 每三角重置 1.0f、ndist
   全程单位（真值 R1 DST 4121999a = sTravel+Δ·rd，非局部单位
   ——单测先错后由真值改）；flags 低字节截断
   `collision_flags & 0xff`；say 协议 STOP 即停、BEFORE/AFTER
   置 onlyLess/onlyMore——真值 R1/R1A/R1B/R1C 四档钉死。
4. **collideIntoObject**（sweep_helpers.hpp 逐句）: 世界→障碍
   局部经 invTransform（真值 RT1F/RT1B/RT1S 往返位串钉死）；
   biggestAxis 严格 > 与 z 不更新 diff1（单测钉死）；bloat =
   半径+0.01f；clip 拒、onlyLess/More 窗拒、sTravel/eTravel
   按主轴比例乘 fullDistance（真值 STR/ETR 位串钉死）。
5. **AABB::clip**（boundbox.cpp:310-388 逐句）: 精确零轴走半开
   面检查；mt/Mt 正负向分支；`sMin >= eMax` 拒；出参按参数
   重写两端——真值 sweep 全链经此函数，portal_bin 位级回放
   即验证；单元侧 5 测钉死各分支。
6. **SweepShape/SweepParams**（sweep_shape.hpp 逐句）: ray
   leader/bb/radius、prism 三顶点 bb 与 `applyVector(range)
   .length()*0.5`、ctor 字段序（含 bsource=box.min、csource
   偏移半 range、`dir /= fullDist` 倒数乘）——真值 R/W 全
   探针位级钉死。
7. **CollisionCallback**（collision_callback.hpp 逐句）: 常量
   0/1/2/4/3；visit 先 doVisit、NOT_IMPLEMENTED 时水旗→ALL
   否则 deprecated（缺省 STOP）；CollisionState 初值 dist=−1
   ——单测钉死（含水旗回退路径）。
8. **位级**: 真值 %08x 位串与移植端 `to_bits()` 按位比较；
   覆盖矩阵 12 分量（三轴行+origin，真值打印同走法）。

## 边界登记

- 参考 PortalObstacle 经 `pPortal_` 裸指针回读 portal 活状态
  （permissive/collisionFlags）；Rust 所有权不允许回指针，
  **Portal 以显式参数串入** collide_ray/collide_prism/
  collide_into_object（登记）。
- 参考 `ltris_` 为可变成员缓存 buildTriangles 结果；移植端用
  `RefCell<Vec<WorldTriangle>>` 承载同语义（登记）。
- Chunk/PortalChunkItem/sceneObject/chunk-space 记账面（障碍
  登记、所有权、space 索引）不建模——碰撞面只认
  transform/bb/三角（登记）。
- ctor 对 points < 3 断言拒绝；参考对 [0..2] 无守卫越界读
  （登记：移植端拒绝构造而非 UB）。
- PortalTarget 枚举保留 chunkName 分发为数据：None（空名）、
  Heaven/Earth/Invasive/Extern（哨兵名）、Chunk(String)；
  不建 Chunk 对象（登记）。
- 参考 `pChunk_`、`chunkName_`/boundary 关联面、traverseMark_
  遍历记号不建模（登记——traverse 属 chunk 簇任务）。

## 样本非手造

`docs/review/fixtures/physics/bsp/gen_portal.cpp` 链接**原版
libchunk.a**（Portal ctor / setCollisionState / PortalObstacle
ctor+buildTriangles+collide 为库内目标码），sweep 面为头内
inline 原文（collideIntoObject/SweepParams/CollisionState/
CollisionCallback），标量路径经 SSE_MATH3 未定义确认位级一致：

- mapper = mTrans(10,0,5) postMultiply mRotY(π/2) postMultiply
  mScale(2,2,2)；ChunkSpace(1)+XMLSection+GeometryMapping 建
  外部 chunk "1_1o"，**chunk 变换 == mapper**（CXT==MPR 真值
  钉死，grid translate 构造期为零），世界↔chunk 坐标变换被
  真实走（scale 2 使 sTravel/距离比例真变）。
- PT1 矩形（plane (1,0,0) d=0，uAxis (0,0,1)，4 点）、PT2 五
  边形（plane (0,0,−1)，uAxis (1,0,0)，5 点）；OBT/OBI/OBB/
  RT1F/RT1B/RT1S 结构真值行钉死变换/逆/包盒/往返。
- **四节电池**：STATE 0（缺省开）、PSTM 关 flags 0x2103 →
  STATE 1、PSTM 开 → STATE 2、PSTM 关 flags 0 → STATE 3；每
  节 9 ray 探针（STOP/ALL/BEFORE/AFTER/两 miss/两五边形）+
  2 prism 探针（命中+miss）。
- **gen_portal.log 144 行**，重跑 diff 干净、exit 0、确定性；
  关键真值：OBI 逆矩阵位串（b2bbbd2e…/c1200000=−10/80000000
  =−0/c0a00000=−5）、OBBMAX.x 3dcccccd=0.1、RT1B z
  b5000000≈−4.8e-7、STATE 1 R1 DST 4121999a/STR 411fae14/
  ETR 41238520、VST FLG 00000003（STATE 1）/00000000（STATE
  3）——flags 低字节截断真值可见。

## 验收直接测试化

- `tests/portal_bin.rs`
  `the_reference_portal_probes_answer_bit_for_bit_across_the_switch`:
  按 log 逐行回放——mapper/ChunkXform/PT1/PT2 构造位串对照、
  OBT/OBI/OBB/RT1 往返对照、PSTM 行驱动移植端
  set_collision_state、RAY×36/PRS×8 全探针 RET/DST/STR/ETR/
  VIS 位级对照、VST 14 行 M/MI/WT/FLG/DST 位级对照；
  **验收条款「开关前后结果与基线一致」直接钉死**：STATE 0/2
  全答一致、STATE 1/3 一致（仅 FLG 字节随 flags 变）——回放
  不做任何逐态特判，全按 log 顺序驱动即过。
- 单元测试: portal.rs 23（ctor 均值/绕向反转/origin 比式/
  set_collision_state 翻 permissive 不删障碍/扇形 n−2/bb 恒
  加 0.1 法向面/say 四档窗/flags 低字节/water 回退/biggest
  axis 严格>与 z 不更新/clip 窗拒/SweepParams 字段序/ray 与
  prism 形态变换）；boundbox.rs 增 5（clip 通过重写/膨胀面/
  零轴半开/入过出拒/负向轴）；vector.rs 增 1（Vector2 运算
  符参考形态含倒数乘）。

## 门禁数字

- `cargo test -p physics`：lib **80** + bsp_bin **9** +
  portal_bin **1** 全绿。
- `cargo test -p foundation`：lib **209** 全绿（含新 clip/
  Vector2 运算符测）。
- `cargo clippy -p physics -p foundation --all-targets -- -D
  warnings`：0。
- `cargo fmt -p physics -p foundation -- --check`：0 漂移。
- `cargo llvm-cov -p physics`：行覆盖 portal.rs **805/814 =
  98.89%**、bsp.rs 1351/1366 = 98.90%、world_polygon.rs
  89/89 = 100.00%、world_triangle.rs 494/494 = 100.00%
  （≥98% 线达标）。
- `cargo check --workspace`：全成员通过（2 条 manifest 级
  unused-dep 警告系既有，非本任务引入）。

## 状态

待 push 后 CI 复核（与跨席基线红对集：rust-test
sdk_matrix/windows_baseline 环境依赖对、macos server-startup、
package-smoke；dispatcher flake 按 memory item 10 归因）。
