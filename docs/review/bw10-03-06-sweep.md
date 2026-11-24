# BW-10-03-06 实现扫掠碰撞 — 复核证据

- **任务编号**: BW-10-03-06（lane `lane/bw10-terrain`，todo.md 行 1360）。
  **验收条款**: 重叠面和边界条件吻合。
- **参照权威树**: `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版源码**:
  - 静态重叠树走查: `lib/physics2/bsp.cpp:1811-1814`
    `BSPTree::intersects(triangle, ppHitTriangle)` → `bsp.cpp:308-365`
    `BSPNode::intersects(triangle, ppHitTriangle)` —— d0<0 先 back
    否则先 front；自有 run 仅 `min < BSPNODE_TOLERANCE && max >
    -BSPNODE_TOLERANCE` 才扫；min3/max3（bsp.cpp:278-299）三元链，
    与 std::min/std::max 平局取首参语义不同。
  - 三角形节点扫: `bsp.cpp:191-220`
    `intersectsThisNode(triangle, ppHitTriangle)` —— 命中即写
    `*ppHitTriangle = &triangle`（**输入三角形**，非候选），bool
    承载全部信息。
  - 扫掠棱柱走查: `bsp.cpp:372-425`
    `BSPNode::intersects(triangle, translation, pVisitor)` —— **无
    d0 分支**：自有 run → back → front 固定序；窗口取 dA（顶点）
    与 dB（顶点+translation）并集 min/max，
    `std::min(min3(dA), min3(dB))` 平局首参胜出。
  - 扫掠节点扫: `bsp.cpp:164-190`
    `intersectsThisNode(triangle, translation, pVisitor)` —— 先问
    `candidate.intersects(triangle, translation)` 几何面，命中再问
    visitor（`pVisitor == NULL || visit(candidate, 0.0f)`），接受
    即返 true，拒绝继续扫完；`TRIANGLE_NOT_IN_BSP` 跳过；dist
    恒 0.0f。
  - 树级扫掠入口: `bsp.cpp:2013-2016`。
  - 三角-三角: `lib/physics2/worldtri.cpp:109-196`
    `WorldTriangle::intersects(triangle)` —— 双平面距离、
    side-out（`d0d1 > 0 && d0d2 > 0` 拒）、D=cross(nA,nB) 轴取
    （X 缺省；|Dy|>|max| 时 Y **更新 max**；|Dz|>|max| 时 Z
    **不更新 max**，worldtri.cpp:169-170 原样）、1D 投影
    `v[i][index]`；COMPUTE_INTERVALS 阶梯
    （`worldtri.ipp:157-186`）: d0d1>0 → ISECT(vv2,vv0,vv1)；
    d0d2>0 → ISECT(vv1,vv0,vv2)；d1·d2>0 || !almostZero(d0) →
    ISECT(vv0,vv1,vv2)；!almostZero(d1) → ISECT(vv1,vv0,vv2)；
    !almostZero(d2) → ISECT(vv2,vv0,vv1)；全 almostZero ⇒
    coplanar **返回 false**。ISECT（worldtri.ipp:147-150）
    isect0=VV0+(VV1−VV0)·D0/(D0−D1)、isect1=VV0+(VV2−VV0)·
    D0/(D0−D2)；sort 换序（ipp:193-202）；答
    `isectA1 >= isectB0 && isectB1 >= isectA0`。
  - 扫掠棱柱: `worldtri.cpp:295-486`
    `WorldTriangle::intersects(triangle, offset)` —— 本面
    SHOULD_NOT_NORMALISE；unitDot = normal.unitVector() ·
    offset.unitVector()（`almostZero(unitDot, 0.005)` ⇒ brute）；
    vd_i = intersectRayHalfNoCheck（`planeeq.ipp:280-284`，
    (d − n·v_i)·oneOverNormalDotDir）；全 vd<0 或全 vd≥1 早退
    false；任一端出窗 ⇒ brute。brute: pe1=输入面，`pe1.normal·
    offset < 0` 时**取反平面+倒序顶点**（flip 分支）；poly=本
    三角形，chop(pe1) → chop(pe2=−n 于 wt1.v0+offset) → 三侧平
    面 `plane.init(wt1.v(i), wt2.v(i), wt1.v((i+1)%3),
    SHOULD_NOT_NORMALISE)`（`planeeq.ipp:117-128` 原始叉积）。
    efficient: 投影主平面 ((Y,Z) 缺省；|ny|>|nx| → (X,Z) 且
    **更新 max**；|nz|>|max| → (X,Y))；triB3D = v_i +
    offset·vd_i；`inside`（worldtri.cpp:281-285）共线算内；
    signA/signB 定向；段走 i1=0,i2=2 起 `i2=i1,i1++`（0-2、
    1-0、2-1），全三点异侧即拒。
  - 多边形剪裁: `lib/physics2/worldpoly.cpp:31-37` `tooClose`
    （0.001mm 窗平方比较）、`worldpoly.cpp:159-221` `chop` ——
    <3 点清空返 false；侧变即插割点
    `lastPoint + (point−lastPoint)·(−lastDist/(dist−lastDist))`；
    回拷时对（含回绕）后继 <1mm 的点去重；≥3 点存活为真。
  - 容差窗: `lib/stdmf.hpp:616-619` `almostZero` 排他窗 ——
    COMPUTE_INTERVALS 用缺省 0.0004，扫掠 unitDot 用 0.005。
- **Boundless 模块**: `crates/physics/src/bsp.rs`（min3/max3、
  `intersects_triangle` 静态走查、`intersects_sweep` 棱柱走查、
  两节点扫、`CollisionVisitor`）；`crates/physics/src/
  world_triangle.rs`（`intersects_triangle`、`intersects_swept`、
  almost_zero/isect/compute_intervals/sort_pair/triangle_plane）；
  `crates/physics/src/world_polygon.rs`（`WorldPolygon::chop`）。

## 移植语义（逐句核对）

1. **静态重叠走查**（bsp.cpp:308-365 逐句）: `d0 < 0` 先 back
   否则先 front；自有 run 窗 `min < TOL && max > −TOL`；min3/
   max3 参考三元链逐字（平局语义与 std::min 不同——单元测试
   `min3_and_max3_follow_the_reference_ternary_chains` 钉死）。
2. **棱柱走查无 d0 分支**（bsp.cpp:372-425）: 自有 run → back →
   front 固定序；窗口对 dA/dB 并集取 min/max，
   `min = min_b < min_a ? min_b : min_a` 复刻 std::min(min3(dA),
   min3(dB)) 首参胜出；ray_blob 拒绝 visitor 真值枚举证实 back
   叶先于 front 叶被奉上。
3. **扫掠节点扫**（bsp.cpp:164-190）: 几何命中先判
   `candidate.intersects_swept(triangle, translation)`，命中再问
   visitor（None 视作接受）；接受返 true 短路，拒绝继续扫完；
   NOT_IN_BSP 跳过；visitor 收到恒 0.0f dist。
4. **静态节点扫**（bsp.cpp:191-220）: 参考命中写
   `*ppHitTriangle = &triangle`（输入三角形），树级 bool 只回答
   有/无——移植端仅返回 bool，`ppHitTriangle` 身份面不建模
   （登记）。
5. **三角-三角**（worldtri.cpp:109-196 + worldtri.ipp:145-202）:
   side-out 拒、轴取三分支（Y 更新 max、Z 不更新——两个真值
   探针分别钉死 243-244 与 249 行）、COMPUTE_INTERVALS 全阶梯
   （含 coplanar false 尾与 degenerate d2 臂——WT 真值探针
   only-b-coplanar 与 third-distance 钉死）、ISECT 逐运算同序、
   sort 换序、答 `a1 >= b0 && b1 >= a0`。
6. **扫掠棱柱**（worldtri.cpp:295-486）: unitDot 窗 0.005 ⇒
   brute；vd 早退双臂（全 <0、全 ≥1——WT 真值端帽探针钉死全
   vd==1 早退）；混合出窗 ⇒ brute；brute flip 分支
   （normal·offset<0 取反平面+倒序顶点——WT 真值探针钉死）；
   五面 chop 依序 pe1/pe2/三侧；efficient 投影轴三分支（Y/Z
   两个真值+单元探针钉死）、triB3D 切片、inside 共线算内、段
   走 a 段先于 b 段（两真值探针分别从 a 段拒与从 b 段拒钉死
   迭代序 440/448 行）。
7. **多边形剪裁**（worldpoly.cpp:159-221 逐句）: <3 清空、侧变
   割点比值 `−lastDist/(dist−lastDist)`、回绕 mm 去重、≥3 存活。
8. **容差窗**: almostZero 排他语义（恰 ±epsilon 拒）——0.0004
   与 0.005 两窗分开用，单元测试钉死。
9. **位级**: g++ -O0 x86-64 与 Rust 同为 IEEE f32 同序逐运算；
   真值 %08x 位串与移植端 `to_bits()` 按位比较。

## 边界登记

- `WorldPolygon::split` 无扫掠面调用者，不移植（登记）。
- 参考静态扫 `ppHitTriangle` 出参身份（写的是输入三角形）不建
  模——树级 bool 已承载全部信息（登记）。
- 扫掠段走是**顶点级**分离测试非区域级：真值 WT TSW 有构造中
  剖面与三角形实际重叠、参考仍按顶点半平面答 MISS 的行为——
  按原样移植，真值钉死。
- brute 侧平面取 `wt1.v(i)`（flip 后的倒序顶点）——flip 与否
  走不同顶点序，真值探针钉死。

## 样本非手造

`docs/review/fixtures/physics/bsp/gen_bsp.cpp` 增 ProbeVisitor
（逐 visit 打 `VST dist位 候选flags`）与 static_probe /
sweep_probe / tri_probe / tri_sweep_probe（原版
`BSPTree::intersects(triangle,ppHitTriangle)` /
`BSPTree::intersects(triangle,translation,pVisitor)` /
`WorldTriangle::intersects` / `WorldTriangle::intersects
(triangle,offset)` 直跑）:

- **BT**（原版 buildBSP 树）: STC×2（宽板穿四面体 HIT、远处
  MISS）；SWP×3（下行棱柱接受 visitor HIT 1 visit、推离 MISS
  0 visit、**拒绝 visitor 枚举全部 6 候选**——四面体面×3、
  WATER 0040、跨面三角形 0100，答 MISS）。
- **LG**（legacy 树）: STC×1（平面鳍 z=1 割段覆 t1 足印 HIT）；
  SWP×3（斜棱柱上行 HIT 1、鳍沿自身面下推 MISS 0、鳍沿法向
  上推 HIT 1）。
- **UC**（真场景资产）: SWP×2（宽棱柱下行 HIT 1、端帽恰触顶面
  HIT 1——**原版答 HIT 非预测 MISS，真值即裁决**）；STC×1
  （水平板穿侧壁 HIT）。
- **WT**（独立三角面对）: TRI×7（穿越 HIT、远处 MISS、共面
  MISS、Y 轴主导 HIT、Z 轴主导 HIT、仅 b 共面 MISS、退化第三
  距离 HIT）；TSW×11（端帽恰在面 MISS、始帽共面 HIT、跨面
  brute MISS、平行滑 brute MISS、零偏移同形 HIT、背面帽 flip
  MISS、远帽止步 MISS、Y 投影 HIT、b 段拒 MISS、a 段拒 MISS、
  迭代序 MISS）。
- 共 **40 行**新真值（STC 4 / SWP 8 / VST 10 / TRI 7 /
  TSW 11）；四个 .bsp md5 与 03-04/03-05 逐字节不变
  （64b49ee1…、44a33ce1…、1425d7cc…、add114f0…），
  gen_bsp.log 追加后重跑 md5 全 OK。

## 验收直接测试化

- `tests/bsp_bin.rs`
  `the_reference_overlap_and_sweep_probes_are_answered_bit_for_bit`:
  BT/LG/UC 全 STC/SWP 逐条回放——答案对照、接受/拒绝 visitor
  seen 序列（dist 位 + 候选 flags）位级对照、WATER/材质字节
  旗标抽查、真值 SWP 行 visit 计数与 VST 行数互证。
  `the_reference_triangle_faces_answer_every_truth_probe`: WT
  TRI×7 + TSW×11 逐条按参考构造三角形回放，答案逐条对照。
- 单元测试: world_polygon 7（割四边形/全后清空/全前不动/短输入
  清空/mm 去重/参考比值/窗界）；world_triangle 增 9（Y/Z 轴
  主导、仅 b 共面拒、退化 d2、Y 投影扫掠、背面帽 flip、远帽
  止步、a 段拒、b 段拒、区间助手已有）；bsp 增 5（棱柱走查
  命中/窗外/静态 d0 分支/NOT_IN_BSP 跳过/min3max3 平局链）。

## 门禁数字

- `cargo test -p physics`：lib **58** + bsp_bin **9** 全绿。
- `cargo clippy -p physics --all-targets -- -D warnings`：0。
- `cargo fmt -p physics -- --check`：0 漂移。
- `cargo llvm-cov -p physics`：行覆盖 bsp.rs **1351/1366 =
  98.90%**、world_polygon.rs **89/89 = 100.00%**、
  world_triangle.rs **494/494 = 100.00%**（≥98% 线达标）。
- `cargo check --workspace`：全成员通过。

## 状态

待 push 后 CI 复核（与跨席基线红对集：rust-test
sdk_matrix/windows_baseline 环境依赖对、macos server-startup、
package-smoke；dispatcher flake 按 memory item 10 归因）。
