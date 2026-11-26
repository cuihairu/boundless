# BW-10-05-06 验证新产物往返（由支持该格式的原版目标重载）

**验收**：由支持该格式的原版目标重载。

## 往返面

「新产物」= 端口自己产出的终流：Rust flood → Rust BSP generator
（generate → streamline → calculate_set_membership）→ `as_binary`，
即 navgen 存 `.cdata/worldNavmesh` 的那串字节。「原版目标」= el7
`libwaypoint.a` 的读取面 —— 05-05 已提交的 `gen_navpaths.cpp`
（链 `-lwaypoint -lchunk -lscene -lresmgr -lmoo -lmath -lphysics2
-lterrain -lcstdmf`），走 navmeshFactory/readWaypointSet/NavLoc/AStar
原版脸，harness 一行未改。

往返证明分三层：

1. **测试内钉死哈希**（`crates/waypoint-gen/src/generator/roundtrip.rs`
   新模块，`the_port_written_assets_match_the_reloaded_original_assets`）：
   端口重跑两场景产出终流，钉长度 + fnv —— slope 136B
   fnv `3a23247435af66a6`、shell 152B fnv `e01640188f0c6115`，即 05-05
   被原版 reader 重载过的原版资产哈希；随后把两个流写成
   `<tmp>/bw10-05-06/{slope,shell}.navmesh` 并读回逐字节核对（写盘的
   就是终流）。
2. **文件层 md5**（证据实测）：端口写盘的
   `slope.navmesh` md5 `30bd31c21138de0e2bc6815e66979c72`、
   `shell.navmesh` md5 `d8b632797387862b8ae69d66aca8efa9` —— 与 05-05
   原版生成器落盘的资产 md5 逐字节相同。
3. **原版目标实测重载**：用**未改动的**已提交 `gen_navpaths.cpp` 重新
   编译，工作目录设为端口写盘目录（harness 读 `./<scene>.navmesh`），
   stdout 输出 `roundtrip.log`，`cmp` 对已提交真值
   `gen_navpaths.log` —— **逐字节相同（LOG-IDENTICAL）**：原版目标
   从端口产物里读出 1+2 条记录（LHDR/LWP/LEDGE 全同）、答对全部 24 对
   CONTAINS/FIND 层探针、跑通全部 7 条路径（含 S1 穿越态收尾两步与
   shell 跨层 X1 真 PATHNONE），末行 `RESULT OK`。

## 参照源码

与 05-05 相同的读取面（chunk_waypoint_set_data.cpp:26-27/:262/:325/:410、
navloc.cpp:31、chunk_waypoint_state.hpp:48）；生成侧即 05-04 已移植的
`WaypointGenerator` 全管线（见
docs/review/bw10-05-04-navmesh-persistence.md 参照表）。

## 登记边界（怪癖）

- **原版目标的 stdout/stderr 分流**：harness 的 chunk 工厂注册 INFO 行走
  stderr（`INFO: [Chunk] Registering factory for worldNavmesh` 等 5 行），
  真值对照必须**只重定向 stdout**；`2>&1` 会把 INFO 混进比较流。
- **哈希钉住取 fnv 不取 md5**：crate 内既有 fnv1a64 惯例（gen_generate
  与 artifacts 比较器同款），文件层证据才用 md5 对账 05-05 的落盘产物。
- **测试写盘目录固定 `<tmp>/bw10-05-06/`**：harness 从工作目录读
  相对路径 `./<scene>.navmesh`，故证据运行的 cwd 就是该目录；测试每次
  运行重写两文件，无并发写者。

## Harness 复现

```sh
# 1) 端口产出并落盘新产物（waypoint-gen 测试内完成）
cargo test -p waypoint-gen the_port_written   # 写 <tmp>/bw10-05-06/*.navmesh

# 2) 原版目标重载（harness 源码 = 05-05 已提交版，一行未改）
g++ -std=c++14 -O2 -o gen_navpaths \
  docs/review/fixtures/waypoint/gen_navpaths.cpp \
  -I $BW/lib -DCODE_INLINE -DCSTDMF_IMPORT -DMF_CONFIG='"hybrid"' \
  -DMF_SERVER -DMF_USE_ASSERTS -D_HYBRID -DBW_BUILD_PLATFORM='"el7"' \
  -L $BW/build/el7/lib \
  -Wl,--start-group -lwaypoint -lchunk -lscene -lresmgr -lmoo \
  -lmath -lphysics2 -lterrain -lcstdmf -lpng -lz -Wl,--end-group \
  -lpthread

# 3) 在产物目录里跑原版目标，stdout 对照已提交真值
cd /tmp/bw10-05-06 && /path/to/gen_navpaths > roundtrip.log
cmp roundtrip.log docs/review/fixtures/waypoint/gen_navpaths.log   # 全等
```

## 测试与门禁

- `cargo test -p waypoint-gen`：68/68（67 既有 + 1 新往返测试）。
- `cargo test --workspace`：4420 通过 / 0 失败（4419 + 1）。
- `cargo fmt -p waypoint-gen -- --check` / `cargo clippy -p waypoint-gen
  --all-targets -- -D warnings`：绿。
- llvm-cov（--lcov，清 target 后全量重测）：waypoint-gen 全部源文件
  ≥99.19%（flood.rs 99.19%、generator.rs 99.31% 与 05-04 记录持平，
  9 行收尾括号 DA:0 已档）；新模块 roundtrip.rs 63 行 **100%**。
