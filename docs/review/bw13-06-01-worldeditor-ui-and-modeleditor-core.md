# BW-13-06-01 映射 worldeditor_ui 与 modeleditor_core

**验收** 公共编辑接口有明确归属。本档将这两个原版编辑器库的公共接口逐一归属到
具体宿主（应用 / 库 / 无对应物），并给出新版目标，便于后续编辑器 crate 立项时直接
照此切分。

**参考锚点**：`/home/cui/workspaces/BigWorld`，HEAD `93bb0837`
（`bigworld-14.4.1-359-g93bb0837`）。

## 1. `tools/worldeditor_ui/` —— 空占位目录

目录仅含两个子目录 + 一个 0 字节 `dir_keeper.txt`：

```
tools/worldeditor_ui/
  binding/            # 仅 dir_keeper.txt（0 字节）
  models/             # 仅 dir_keeper.txt（0 字节）
  dir_keeper.txt      # 0 字节
```

- **无源码、无 `CMakeLists.txt`**，全树无任何文件 include `worldeditor_ui` 的头文件，
  也无任何构建目标引用该目录（按目录名 grep 构建脚本仅命中路径本身）。
- `dir_keeper.txt` 是版本控制占位文件（`/home/cui/workspaces/boundless` 系列约定：
  空目录需一个占位文件才能被 Git 跟踪）。
- **结论：无源码可迁移**。世界编辑器的 UI 代码实际在 `tools/worldeditor/` 自身
  （该目录 `BW_ADD_TOOL_EXE(worldeditor worldeditor WIN32 …)`，`tools/worldeditor/CMakeLists.txt:910`），
  `worldeditor_ui` 是预留/历史遗留的空壳。归属 = **无对应物（不迁移）**。

## 2. `tools/modeleditor_core/` —— 模型编辑器应用核心库

`tools/modeleditor_core/CMakeLists.txt`：`BW_ADD_LIBRARY(modeleditor_core …)`
（库，非可执行），`INCLUDE(BWStandardMFCProject)` → **Windows MFC**，
`BW_PROJECT_CATEGORY( modeleditor_core "ModelEditor Core" )`。

### 2.1 公共接口归属

唯一跨模块公共接口头是 `i_model_editor_app.hpp`：

```cpp
class IModelEditorApp : public IEditorApp   // IEditorApp 来自 editor_shared
{
    virtual bool initDone() = 0;
    virtual App * mfApp() = 0;                    // appmgr::App
    virtual CWnd * mainWnd() = 0;                // MFC 窗口句柄
    virtual const BW::string & modelToLoad() = 0;
    virtual void modelToLoad( const BW::string & modelName ) = 0;
    virtual void updateRecentList( const BW::string& kind ) = 0;
    virtual MEPythonAdapter* pythonAdapter() const = 0;
    virtual void OnFileRegenBoundingBox() = 0;
    virtual void loadLights( const char* lightName ) = 0;
    virtual void OnAppPrefs() = 0;
    virtual void OnFileAdd() = 0;
    virtual void exit( bool ignoreChanges = false ) = 0;
    // 非虚：Python 入口（MEPythonAdapter 调用）
    void loadModel( const char* modelName );
    void addModel( const char* modelName );
    void OnFileReloadTextures();
    void OnFileOpen();
};
```

| 接口成员 | 语义 | 归属 |
|---|---|---|
| `initDone` / `mfApp` | 初始化完成、所属 `App` | **App 宿主层**（模式编辑器应用壳） |
| `mainWnd` | MFC `CWnd*` 主窗口 | **GUI 层**（MFC 特化，Rust 侧无对应物） |
| `modelToLoad` / `modelToLoad(string)` | 待载入模型名（会话状态） | **模式编辑器应用核心**（模型载入） |
| `updateRecentList(kind)` | MRU 列表 | **App 宿主层**（mru.cpp） |
| `pythonAdapter()` | 脚本适配器 | **脚本集成层**（App/me_python_adapter.cpp） |
| `OnFileRegenBoundingBox` | 重算包围盒 | **模型处理层**（Models/mutant*） |
| `loadModel` / `addModel` | Python 直接调用的模型载入 | **脚本集成层**（公开 Python 契约） |
| `loadLights` | 载入灯光集 | **模型处理层**（Models/lights.cpp） |
| `OnAppPrefs` / `OnFileOpen` / `OnFileAdd` / `exit` | 文件/偏好/退出菜单动作 | **GUI 层**（Pages + GUI） |

### 2.2 内部模块与归属

| 子目录 | 关键文件 | 归属 |
|---|---|---|
| `i_model_editor_app.hpp`（根） | 应用接口 | 模式编辑器应用核心（公共契约） |
| `App/` | `me_app`（应用壳）、`me_shell`、`me_module`、`me_python_adapter`、`me_scripter`、`undo_redo`、`mru`、`material_preview`、`grid_coord` | 应用壳 + 脚本集成 |
| `GUI/` | `main_frm`、`model_editor_view`、`panel_manager`、`gl_view`、`tree_list`、`splash_dialog` 等 | MFC GUI 层（Windows-only） |
| `Models/` | `mutant*`（模型/动画/材质/LOD/校验）、`lights`、`visual_bumper` | 模型处理层 |
| `Pages/` | `page_actions`、`page_animations`、`page_display`、`page_lights`、`page_lod`、`page_materials`、`page_object` | 属性页（GUI）；公共消息来自 `../common/page_messages` |

### 2.3 依赖方向

`modeleditor_core` 链接（CMakeLists `BW_TARGET_LINK_LIBRARIES … INTERFACE`）：
`appmgr`、`cstdmf`、`chunk_scene_adapter`、`editor_shared`、`guimanager`、`input`、
`moo`、`physics2`、`post_processing`、`pyscript`、`resmgr`、`romp`、`terrain`、
`tools_common`、`nvmeshmender`（第三方）。

消费方：`tools/modeleditor/CMakeLists.txt:40`
`BW_ADD_TOOL_EXE( modeleditor modeleditor WIN32 ${BLOB_SRCS} )` —— 模型编辑器
可执行由 `tools/modeleditor/` 构建，`modeleditor_core` 是其核心库。

## 3. 归属结论

| 原版库 | 归属宿主 | 新版目标 |
|---|---|---|
| `tools/worldeditor_ui/` | **无对应物**（空壳，零源码，无构建目标、无 include） | 不迁移；世界编辑器 UI 归 `tools/worldeditor/` 自身 |
| `tools/modeleditor_core/` | **模型编辑器应用**（`tools/modeleditor` 可执行）；核心库形态，公共契约 = `IModelEditorApp`（继承 editor_shared 的 `IEditorApp`） | BW-13 编辑器 crate（模型编辑器）——库尚未建，随 BW-13 立项解锁；其 MFC GUI 层与 `CWnd`/`guimanager` 为 Windows-only，与 BW-15 Windows 后端批次同批落地 |
