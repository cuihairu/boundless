# BW-10-05-02 移植 flood 生成

## 验收

> **BW-10-05-02 移植 flood 生成**。**验收** 斜坡和室内外交界产物可对照。

## 产物对照（验收面）

参照 harness `docs/review/fixtures/waypoint/gen_flood.cpp` 直接编译原版
`lib/waypoint_generator/waypoint_flood.cpp`（补丁副本仅把
`boundarySet_` 前加一行 `public:` 以便倾倒），跑两个验收场景并把
`FILL/SCENE/H/A/B/TGA` 全量倾倒进
`docs/review/fixtures/waypoint/gen_flood.log`（752 行，已提交）。Rust 侧
`flood::artifacts::the_reference_harness_scenes_reproduce_the_truth_log`
用同一批 slab、同一批种子、同一倾倒格式重跑
[`crates/waypoint-gen/src/flood/artifacts.rs`]，**逐行全等**（含 TGA
字节的 FNV-1a 64 哈希）。

### 场景一：斜坡（slope）

- 8 块半开 slab，`x∈[i,i+1)` 处高度 `0.5·i`；9×9 网格
  （`xsize_ = int((max.x-min.x)/res)+1 = 9`，81 格）。
- `FILL 0 count 72`：x=8 列无 slab，`findDropPoint` 失败，72/81 格落层 15，
  高度 0.0→3.5 阶梯。
- 内部格 8 角全连（`A 15 … 0..7` 全 15）；边界格只保留指向已淹格的角度
  （如 0 号格只有 U/R，无 UR——对角步的门在其间格上）。

### 场景二：室内外交界（shell）

室外地条 + 密封房间（地板 0.5、墙 3.0 不可攀）+ 室内天桥（1.5）+
chunk 归属界 `x ≤ 3.5`：

- `FILL 0 count 21`：室外 0.0（西列 + 南北行，越界列被 chunk 剪掉，
  x=4 列靠 ±res 探针过 `checkBounds` 留下）。
- `FILL 1 count 6`：天桥种子落到 1.5（见下面的种子量化），天桥面 6 格
  落层 15。
- `FILL 2 count 12`：地板种子从 y=1.1 从天桥下方落到 0.5；天桥下 6 格
  因层 15 已被 1.5 占据，按 free-alloc 落到**第二层 14**——两层格
  （29,30,31,38,39,40 同格 `(15,1.5)+(14,0.5)`），跨层链接（同高 0.5
  的 14↔15 互链，`A 14 29 3 -> 15` 等）全部对上。
- 边界集 7 条，**带层号混合**：`B 22 15 / B 49 15`（地板 15）与
  `B 31 14 / B 40 14`（天桥下 14）——层号语义逐条对上。

### 种子量化（对照捕获的移植面）

`setArea` 先按**未量化参数**算 `xsize_/zsize_`，再给 `min_/max_` 加
`HACK_OFFSET`（0.1，避开整数边界的碰撞 hack）：

```
min_ = (0.1, ·, 0.1)；种子 (3.0,10,3.5) 探针 = floor((3.0-0.1)/1)·1+0.1 = 2.1
```

于是天桥种子实际探 (2.1,10.1,3.1) 落桥面 1.5，地板种子探 (2.1,1.1,4.1)
落 0.5——两个种子各得其所。`flood.rs` 的 `set_area` 同序同值。

## 参照源码

| 参照 | 位置 |
|---|---|
| `DROP_FUDGE`、`IPhysics::isUnblocked` 缺省实现 | waypoint_flood.hpp:39,44-75 |
| `MAX_HEIGHTS`、私有 `boundarySet_` | waypoint_flood.hpp:117,138 |
| `setArea`：量化边界、尺寸先算、HACK_OFFSET 后加 | waypoint_flood.cpp:149-199（183-184,191-196） |
| `checkBounds`：16 组 ±res 探针 + scramble 重试 | waypoint_flood.cpp:226-296 |
| `fill`：量化、丢点、级联重试、bestG、free-alloc、push 守卫、pop 计数 | waypoint_flood.cpp:306-726（340-342,348-398,417-431,438-457,470-472,502） |
| dst bestG 以 **src** 格高算距离（原版怪癖，照抄） | waypoint_flood.cpp:570-580 |
| 空闲层带邻接即杀进程（CRITICAL → panic!） | waypoint_flood.cpp:560,597,609,657 |
| "Shall NEVER fail here!" 回退步 | waypoint_flood.cpp:668-676 |
| `checkBounds` 失败记 boundarySet_ | waypoint_flood.cpp:680-684 |
| `flashFlood` / `postfilteradd` / `postfilterremove` / `shrink`(no-op) / `writeTGA` | waypoint_flood.cpp:732,797,955,1142,1214 |

## 移植面

| 参照 | Rust |
|---|---|
| `WaypointFlood` | `WaypointFlood`（`flood.rs:117`） |
| `IPhysics` | `FloodPhysics`（`flood.rs:58`，缺省 `is_unblocked` `flood.rs:71`） |
| `Chunk::owns` + space 界 | `FloodChunk`（`flood.rs:101`） |
| `setArea`（先尺寸后偏移） | `set_area`（`flood.rs:159`，`HACK_OFFSET` `flood.rs:25`） |
| `checkBounds` | `check_bounds`（`flood.rs:271`） |
| `fill` | `fill`（`flood.rs:330`） |
| `flashFlood` / `postfilteradd` / `postfilterremove` / `shrink` | `flash_flood` 668 / `postfilter_add` 729 / `postfilter_remove` 838 / `shrink` 967（no-op） |
| `writeTGA` | `write_tga`（`flood.rs:974`，字节与原版全等） |
| `boundarySet_`（`std::set<pair>`） | `BTreeSet<(i32,i32)>`（`flood.rs:253`，同序） |

## 测试与门禁

- `cargo test -p waypoint-gen`：**40 通过 / 0 失败**（39 旧 + 1 新产物对照）。
- 覆盖（llvm-cov 0.9.1，`--lcov` DA:0 计数）：`flood.rs` **99.19%**
  （615 行仅 5 行未盖 = 370/387/399/402/403 纯闭括号，DA:0 为区域伪迹，
  各分支体均有真计数）；`flood/covers.rs` **100%**。
- `cargo test --workspace`：**4389 通过 / 0 失败**；`cargo test -p
  test-support` 绿；`cargo check --workspace` 绿；`cargo fmt --check
  -p waypoint-gen` 绿；`cargo clippy -p waypoint-gen --all-targets
  -- -D warnings` 绿。

## 登记边界

- **BgTaskManager 中断点省略**：移植为同步调用（参照 fill 内四处
  shouldAbortTask 返回 -1）。
- **ERROR_MSG/WARNING_MSG 静默**、**LogFile/DEBUG_WAYPOINT_FLOOD 省略**；
  CRITICAL_MSG（空闲层带邻接）→ `panic!`，单测双向覆盖（free-alloc
  560 与重分配 597 两个臂各一条 `should_panic`）。
- **a=6 负索引**：3×3 网格上 a=6 的 ondex 越界在 C++ 是 UB，Rust 会
  panic——移植按字面镜像，该域仅出现在非 3×3 网格，登记不改。
- **`Chunk::owns` 重叠臂**：参照 `Chunk::owns` 的 overlapper 分支待
  BW-10-03 chunk 面落地后对齐；harness 与单测均为盒式 owns。
- **种子 push 守卫**（种子格已有邻接则 count 0）：由单测覆盖，不在
  本产物场景（最终场景 FILL 2 为真 12 格地板淹）。
- **DA:0 伪迹行**：flood.rs 370/387/399/402/403（纯闭括号）。
- **跨席已知红**：workspace 全量 clippy 红在 `message_logger`
  （bw16-18 合并遗留 `empty line after doc comment`），不在本席依赖链，
  不在本席清。

## Harness 复现

```sh
cp $BW/lib/waypoint_generator/waypoint_flood.{hpp,cpp} /tmp/floodsrc/
sed -i 's|\t\tBW::set<std::pair<int, int> > boundarySet_;|\tpublic:\n\t\tBW::set<std::pair<int, int> > boundarySet_;|' \
  /tmp/floodsrc/waypoint_flood.hpp
echo > /tmp/floodsrc/pch.hpp
g++ -std=c++14 -O2 -o gen_flood docs/review/fixtures/waypoint/gen_flood.cpp \
  /tmp/floodsrc/waypoint_flood.cpp -I <stubs> -I /tmp/floodsrc \
  -I $BW/lib -DCODE_INLINE -DHANDLE=void* -DINVALID_HANDLE_VALUE=NULL -DBW_GUARD= \
  $BW/build/el7/lib/libcstdmf.a $BW/build/el7/lib/libmath.a -lpthread
./gen_flood > docs/review/fixtures/waypoint/gen_flood.log
```

stubs 影子头（chunk/cstdmf 面，flood 只用 `Chunk::owns` 与 space
min/max）与倾倒细节见 `gen_flood.cpp` 头注。产物二进制与 TGA 不入库。
