# BW-10-05-04 分片一：navmesh 持久化（asBinary / saveOut 合并）

> **BW-10-05-04 实现原版 NavGen 图形工作流**。**验收** 同图可选择目标、生成和保存。

**分片状态**：本片交付工作流的**保存臂**——记录流写入器
`IWaypointView::asBinary` 与 `saveOut` 的旧记录合并算术（todo 不勾，
05-04 仍开）。余片：`WaypointGenerator` BSP 多边形化
（generate / determineEdgesAdjacentToOtherChunks / streamline /
extendThroughUnboundPortals / calculateSetMembership，3040 行）与
`tools/common/navmesh_processor.cpp` 的 per-girth 工作流环。
BW-10-05-03 Recast 移植范围仍挂起待用户拍板。

## 产物对照（验收面）

参照 harness `docs/review/fixtures/waypoint/gen_view.cpp` 把**未改动**的
原版 `lib/waypoint_generator/waypoint_view.cpp`（`cmp` 逐字节验证一致）
编进影子桩（`/tmp/viewstub`：chunk/resmgr 面全部内存化），跑 5 多边形
2 集合的测试场景，倾倒 `VIEW/ASBINARY/R/P/V/HX/CDATA1/CDATA2/CHUNK` 进
`docs/review/fixtures/waypoint/gen_view.log`（已提交，含每 16 字节一行
的 `HX` 十六进制逐字节对照）。Rust 侧
`crates/waypoint-gen/src/view/artifacts.rs` 同场景同倾倒格式重跑，
**逐行全等**（记录流字节级一致：ASBINARY 260 字节
fnv `9277bbe01734b419`、CDATA2 合并流 304 字节 fnv
`16029c0e097e371b`）。

场景覆盖 `asBinary` 的**全部邻接分支**（邻接域是 1 基全局多边形号）：

| 顶点 | 原始 adj | 输出 | 分支 |
|---|---|---|---|
| P0 v0 | 1（自身） | 0 | 同集合重映射 `indexMap[1]=0` |
| P0 v1 | 3（P3，异集合） | 0 | `std::map operator[]` 幻影插入默认 0 |
| P0 v2 | 0 + 跨块 | 65535 | `adjToAnotherChunk` |
| P0 v3 | -1 | -2 | vista `~ -adj` = adj-1 |
| P1 v0 | 2（P2） | 1 | 同集合重映射 `indexMap[2]=1` |
| P1 v1 | 7（>5） | 7 | 越界原样透传（三分支都不中） |
| P1 v2 | -5 | -6 | vista |
| P3 v3 | 5（P4 同集合） | 1 | 重映射 `indexMap[5]=1` |
| P4 v1 | 1（P0 异集合） | 0 | 幻影插入 |

girth 回退：`getGirth(0)=0` → 落 `defaultGirth 0.8`；`getGirth(1)=1.2`。
记录按集合号升序（参照 `BW::map` 序），每记录
`[int32 version 0][float girth][int32 nwp][int32 ne][全部 ctrl][全部
data]`——ctrl/data 两段**先后打包**（waypoint_view.cpp:394-400 的
memcpy 序），不是逐多边形交错。

`saveOut` 两趟：

- **趟 1**（无 cdata）：从零建 `.cdata`，写入与 ASBINARY 全同的流；
  `.chunk` 节旧节清空（boundary/navPolySet 删除），写入
  `worldNavmesh/resource = gen_view.cdata/worldNavmesh`。
- **趟 2**（`removeAllOld=false` + 旧流 [0.8 nwp1ne1][1.5 空][0.6
  nwp0ne1]）：合并结果 304 字节 = 新流 260 + 保留 16 + 28——**0.8 旧
  记录（默认 girth）被新流取代**，1.5/0.6 按旧序跟随其后。

## 参照源码

| 参照 | 位置 |
|---|---|
| `PairOfStreams` + `NavPolySets_asBinary_map` | waypoint_view.cpp:288-306 |
| preflight：girth 回退、`indexMap[p+1]=count++` | waypoint_view.cpp:313-327 |
| 主循环：ctrl `mhv.y<<Mhv.y<<vertexCount`、data `nc.x<<nc.z` | waypoint_view.cpp:330-385（346-355,358-366） |
| 邻接三分支 + 越界透传 | waypoint_view.cpp:368-374 |
| 记录打包 `[version][girth][nwp][ne][ctrl][data]` | waypoint_view.cpp:388-404（394-400 memcpy 序） |
| `saveOut` 旧流索引 + bit31 标记 + keepSize | waypoint_view.cpp:190-218（211,212,217） |
| 合并追加 `oldParts[i]>>31` / `& ~(1<<31)` | waypoint_view.cpp:227-241 |
| `saveOut` 无 cdata 建节、`.chunk` 旧节清扫、resource 串 | waypoint_view.cpp:243-281 |
| `VertexRef::getVertex` 网格缩放 | waypoint_view.hpp:83-118 |

## 移植面

| 参照 | Rust |
|---|---|
| `IWaypointView::asBinary` | `view::as_binary`（`view.rs:65`） |
| `NavPolySets`（`BW::map<int,PairOfStreams>`） | `BTreeMap<i32, SetStream>`（同序） |
| `PolygonRef`/`VertexRef` 读面 | `NavView` trait + `NavVertex`（世界坐标已含 `getVertex` 缩放） |
| 邻接三分支 + 65535 常量 | `ADJ_ANOTHER_CHUNK`（`view.rs:31`） |
| `saveOut` 合并臂 | `view::save_out_records`（`view.rs:189`） |
| 旧流解析（16B 头 + elt 计数步进） | `parse_old_parts`（`view.rs:156`） |
| bit31 丢弃标记 | 直接按 girth 判（语义同 `oldParts[i]>>31` 测试） |

## 测试与门禁

- `cargo test -p waypoint-gen`：**43 通过 / 0 失败**（40 旧 + 1 产物逐行
  对照 + 2 合并单元测：保留序断言、全默认 girth 旧流/`removeAllOld`
  提前返回）。
- 覆盖（llvm-cov 0.9.1，`--lcov` DA 计数）：`view.rs` **100.00%**
  （90/90）；`view/artifacts.rs` 98.18%（5 行 DA:0 = 306-315 比较失败
  报告臂，测试永不走进）；`flood.rs` 99.19% 不变。
- `cargo test --workspace`：**4392 通过 / 0 失败**；`cargo test -p
  test-support` 绿；`cargo fmt --check -p waypoint-gen` 绿；
  `cargo clippy -p waypoint-gen --all-targets -- -D warnings` 绿。

## 登记边界

- **MF_ASSERT 怪癖**：dev 构建下 `MF_ASSERT(offset <= INT_MAX)`
  （waypoint_view.cpp:212）对旧流里任何默认 girth 记录都会触发——
  bit31 标记后的 offset（如 40 | 0x80000000）大于 INT_MAX。release
  构建该断言编译掉，合并按预期丢弃默认 girth 记录。移植采用 release
  语义，harness 用 `-D_RELEASE` 编译。
- **`writeToSection`（旧 XML 面）未移植**：navgen GUI 写 `.navpoly`
  XML 的遗留路径，headless 工作流不经过；如后续 05-05/06 需要再评估。
- **resmgr/chunk 桩面**：`BinaryBlock`/`BinSection`/`BWResource`/
  `DataSection`/`Chunk` 在 harness 里是内存桩（`/tmp/viewstub`），只
  服务编译原版源码与驱动逻辑；`saveOut` 的磁盘格式（BinSection 序列化）
  与 BWResource 缓存不在移植面内。桩 `DataSection::save` 语义为内存
  重登记。
- **MemoryOStream `<<` 是裸 memcpy**（binary_stream 模板 addToStream），
  x86 上即小端——Rust 用 `to_le_bytes()` 同值。
- **截断旧流**：参照对 nwp/ne 超出缓冲是越界读（UB）；Rust 侧解析
  `try_into`/切片会 panic。登记不改（真产物不含截断流）。
- **跨席已知红**：workspace 全量 clippy 红在 `message_logger`
  （bw16-18 合并遗留），不在本席依赖链。

## Harness 复现

```sh
BW=/home/cui/workspaces/BigWorld/programming/bigworld
mkdir -p /tmp/viewstub/{cstdmf,resmgr,chunk,waypoint_generator}
cp $BW/lib/waypoint_generator/waypoint_view.{hpp,cpp} /tmp/viewstub/waypoint_generator/
# 影子桩：pch.hpp（把 memory_stream 链提升到全局作用域，避免
# waypoint_view.cpp:285 的文件中段 include 嵌套 BW::BW）、
# chunk/chunk.hpp（owns+space+transformInverse/centre/binFileName/
# resourceID/identifier）、resmgr/{binary_block,datasection,bin_section,
# bwresource}.hpp（内存树 + BWResource 路径->节 store；桩类需
# incRef/decRef/refCount 三件套以配 SmartPointer）
g++ -std=c++14 -O2 -o /tmp/viewstub/gen_view \
  docs/review/fixtures/waypoint/gen_view.cpp \
  /tmp/viewstub/waypoint_generator/waypoint_view.cpp \
  -I /tmp/viewstub -I /tmp/viewstub/waypoint_generator -I $BW/lib \
  -DCODE_INLINE -D_RELEASE -DHANDLE=void* -DINVALID_HANDLE_VALUE=NULL -DBW_GUARD= \
  $BW/build/el7/lib/libcstdmf.a $BW/build/el7/lib/libmath.a -lpthread
/tmp/viewstub/gen_view 2>/dev/null > docs/review/fixtures/waypoint/gen_view.log
```

（stderr 走原版 INFO_MSG；stdout 才是 log。`cmp` 验证
`/tmp/viewstub/waypoint_generator/waypoint_view.cpp` 与参照逐字节一致。）
产物二进制不入库。
