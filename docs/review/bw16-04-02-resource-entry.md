# BW-16-04-02 统一编辑器和工具的资源入口 — 一份产物、三类应用

- **任务编号**: BW-16-04-02（lane BW-16 源码映射与扩展接口）
- **验收**: 两类应用可加载同一产物。
- **结论**: 满足并勾行。统一资源入口（`crates/resources` 的 `BwResource`）早已在树并被三族
  应用消费；本席补齐的是**同一份产物经同一入口跑通三族**的证明：
  `crates/test-support/tests/resource_entry.rs` —— 一个 scratch root、一个
  `BwResource::init_paths`，其上（a）编辑器族 `chunk::space_publish` 发布 `space_new/`
  并经 multiplexer 启动回 16 块 chunk 空间（BW-13-03-06 验收在共享入口上重放）；
  （b）工具族 `asset_pipeline::res_packer` 经同一入口 pack 发布物 `space.settings`
  并回读逐字段相等；（c）应用族 `server_runtime::config::BwConfig` 从同一入口启动
  `server/bw.xml` 链（`apps/server` 的启动序：先资源后配置）。红臂核对三族的
  message-exact 缺失面（2 测全绿）。

## 1. 三族消费同一入口（参照锚点）

| 应用族 | 参照 | 新版消费面 | 入口路由 |
|---|---|---|---|
| 编辑器（WorldEditor 族） | `tools/worldeditor/terrain/space_recreator.cpp` + `project/chunk_walker.cpp` | `chunk::space_publish::SpaceRecreator::create` 发布；`GeometryMapping`+`ChunkSpace::load_chunk/bind` 启动回 | `BwResource::file_system()`（multiplexer）供 settings/chunk 解析 |
| 工具（res_packer 族） | `tools/res_packer/` | `asset_pipeline::res_packer::convert`（`PackerEnv::new(&BwResource)`） | `env.resource.file_system()` 读源、写目标 |
| 应用（server 角色） | `lib/server/bwconfig.cpp`，`apps/server/src/main.rs` 的启动序 | `server_runtime::config::BwConfig::init_from_sources(&resource, None)` | `resource.open_section("server/bw.xml")` 走 section 缓存+multiplexer |

编辑器族 `create` 取裸 `FileSystem` 缝（参照同），测试以
`NativeFileSystem::new(root/)` 挂与 `refresh_paths` 完全相同的根——发布的产物落在
入口的资源根下，随后两族全部经入口读回。

## 2. 实测（2 测全绿）

```
$ cargo test -p test-support --test resource_entry
test one_resource_entry_serves_the_editor_tool_and_app_faces ... ok
test the_entry_reports_misses_to_every_family ... ok
test result: ok. 2 passed; 0 failed
```

绿臂断言链：发布 16 `.chunk`+`.cdata`+`space.settings`+`locations.xml` → 经入口读
settings 建 mapping → 16 格 load+bind → pack 回读 `chunkSize==100.0`、
`bounds/minX..maxY`、`terrain/version==200`、`heightMapEditorSize==64`、terrain
子节点数与 XML 树相等 → `BwConfig` 链读 `shared/limit==10`、`app/port==5000`
且 `is_bad` 为假。

红臂断言链：无 settings 的源空间 → `create` 假 + `"Unable to copy space.settings\n"`
逐字；入口找不到的节 → packer `"Failed to open data section …"` + `errors_occurred`；
空 server 目录 → `is_bad` 真。

## 3. 边界与勘误

- **「依赖 editor/tools 应用未建，apps/ 现仅 server」已陈旧**：`apps/bots`（BW-10-07-05，
  `2fd88e4`）已在树；且两类应用族（编辑器/工具）的消费面本体早已入树并在消费同一入口
  ——验收句「两类应用可加载同一产物」以本测试直接满足，无需 GUI 二进制（那些归
  BW-15-03 Windows 构建入口）。
- 本测试证明的是**共享入口的可达性**，不扩权：编辑器 GUI、Maya exporter 等 Windows-only
  外壳仍未建（BW-16-04-01 登记表已列）。
- 顺带修复（同 commit 前置）：本席 `c8dc49e`（space index over the hull tree）重写
  `crates/chunk/src/lib.rs` 时静默丢五条 `pub mod` 声明（`chunk_editor`/`chunk_placer`/
  `snaps`/`space_publish`/`undoredo`，文件在盘、模块未编译——BW-13-03 编辑器族五面全部
  假死）。本 commit 恢复声明与 re-export；`chunk_placer` 对焦点网格代际的缝：
  `ChunkSpace::remove_chunk`（`ChunkPlacer::deleteChunk` 面，参照无 `ChunkSpace::removeChunk`
  ——placer 侧建模；列清理沿 `ignoreChunk` 的 3x3 约定、无 bound-only 拒绝）与测试世界的
  ready-space 契约（bind+focus 后查询，参照 `ChunkSpace::findChunkFromPoint` 走 column，
  chunk_space.cpp:736）。恢复后 chunk 167 lib 测全绿（+31 space_publish、+16 chunk_editor、
  +9 snaps、+13 undoredo、placer 50）。
