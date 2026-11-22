# BW-13-06-04 迁移调试字段视图（Debug Field View）

**验收** Rust 实体等价字段可在 IDE 中展开。
**状态** 阻塞登记（blocked）——参考侧被可视化对象 `BW::SceneObject` 在新树无活 Rust 等价实体类型；任务行留 `[ ]`，待 cell 侧实体运行时落地后复验。

## 1. 参考侧

单一文件 `tools/visual_studio/visualizers/bigworld.natvis`（11 行，`AutoVisualizer` XML，VS 调试器可视化器）。全树仅此一处 natvis（grep `*.natvis` / `natvis` 引用，无其他）。

## 2. 参考行为逐条复现

被可视化对象 `BW::SceneObject`（cell 场景对象：handle + 类型 id + 类型名查表）：

- **DisplayString**：`Object: {handle_} of type: {type_,u} ({<type name lookup>})`
  - 展开摘要 = `handle_`（句柄）+ `type_`（类型 id）+ 经 `BW::SceneTypeSystem` 查表得到的类型名。
  - 类型名查表 = `s_typeNameLookup.table_._Myfirst[s_objectTypeContext.externalMapping_._Myfirst[type_ - 1]]`（静态查表：外部映射下标 → 表内名字）。
- **Expand** 三项：
  - `Type` = `type_,u`（十进制）
  - `TypeName` = 上述查表表达式（`s`，字符串）
  - `Handle` = `handle_,x`（十六进制）

即调试器里把 `BW::SceneObject` 展开为 `Handle` / `Type` / `TypeName` 三个可读字段。

## 3. 新树映射

| 参考 | 新树 | 说明 |
|---|---|---|
| `BW::SceneObject`（活场景对象） | **无活等价类型** | 见下 |
| natvis 字段视图（VS 调试器） | Rust `Debug` trait 字段视图（rust-analyzer / IDE 调试可视化） | Rust 原生等价物 |
| `BW::SceneTypeSystem` 类型名查表 | （随实体运行时落地时实现） | 查表逻辑在新树尚无载体 |
| `tools/visual_studio/visualizers/*.natvis` | 无（新树零 `.natvis`） | VS-only 工具面 |

**活类型可得性核实**（2026-11-22，含 merge 后 main）：
- `crates/entitydef` 仅元数据/描述层类型：`DataDescription`、`MemberDescription`、`EntityDescriptionMap`、`DataType`、`EntityMethodDescriptions` 等——描述定义，非运行时对象。
- `crates/foundation/src/ids.rs`：`pub type EntityId = i32;`——标量 id（4 字节，含 null/first/local 哨兵），**不是**带 `type` 字段的活实体对象。
- `crates/db`（DBApp 存储契约，merge 后落地）：存储 get/load，无实体运行时对象。
- 全树 grep `pub struct Entity|SceneObject|Space`、`handle:` 字段配对 `type` 字段：无命中。新树**尚无**可被该字段视图可视化的活实体类型。

## 4. 归属与阻塞结论

- **主体阻塞**：`BW::SceneObject`（cell 活场景对象）属 cell 侧实体/space 运行时（BW-08/BW-10 地域），新树未落地 → 无「Rust 实体等价字段」可展开。验收「Rust 实体等价字段可在 IDE 中展开」**当前不可达**。
- **工具面阻塞**：natvis 为 Visual Studio（Windows-only）调试器文件，迁移属 BW-15 Windows 后端批次；Rust 侧等价实现是实体类型的 `Debug` impl（rust-analyzer 可展开），不依赖 Windows。
- **登记**：任务行留 `[ ]`，卡点为「cell 侧实体运行时（BW-08/BW-10）未落地 + VS natvis 随 BW-15」。待实体类型落地后复验：为活实体类型加 `Debug` 视图（handle/type/type_name 三项），或在 BW-15 批次复制参考 natvis。
- **不属本项**：`BW::SceneTypeSystem` 类型名查表的 Rust 移植随实体运行时一并落地；mercury/endpoint 等非实体类型已有 `#[derive(Debug)]`，非本项主体。
