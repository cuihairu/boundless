# BW-15-06-01 图形后端列表：逐平台能力与依赖

**验收** 每个平台记录能力及依赖。本档登记原版各图形后端（客户端 D3D、工具 OpenGL、
移动 cocos2d、web Awesomium）各自的目标平台、能力边界与依赖，作为新版渲染栈
（BW-11 立项、BW-16-02-02 图形后端隔离）的分层依据；并记录原版在非 Windows 桌面
无图形后端这一基线事实。

**参考锚点**：`/home/cui/workspaces/BigWorld`，HEAD `dc3b0b59`
（`bigworld-14.4.1-369-gdc3b0b59`）。

## 1. 平台基线（构建门）

`programming/bigworld/CMakeLists.txt:57-72`：

| 分支 | 平台 | 构建物 |
|---|---|---|
| `BW_IS_REMOTE_ONLY` | Linux 64 | 仅服务端 |
| `WIN32` | win32 / win64 | 客户端 + 工具 + 服务端 |
| 其余 | — | `FATAL_ERROR "Only Windows builds are currently supported."` |

即**桌面客户端只在 Windows 构建**；非 Windows（Linux）只构建服务端；树内无
macOS 桌面构建分支。客户端产物按平台落目录：`client/CMakeLists.txt:342`
`BW_SET_BINARY_DIR(bwclient "${BW_GAME_DIR}/client/${BW_PLATFORM}")`。

## 2. 后端总表

| 后端 | 宿主应用 | 平台 | 能力 | 依赖（参考证据） |
|---|---|---|---|---|
| **Direct3D 9**（`lib/moo`） | `bwclient`（客户端）、工具视口 | **Windows only**（win32/win64） | 完整 3D 渲染：纹理/材质/Deferred 光照/阴影/粒子/天空盒 | `d3d9`、`d3dx9`、`dxguid`、`setupapi`、`psapi`——`lib/moo/CMakeLists.txt:475-482` 在 `IF(BW_PLATFORM_WINDOWS)` 下 `BW_TARGET_LINK_LIBRARIES(moo INTERFACE ...)`；设备面 `lib/moo/moo_dx.cpp` + `render_context.cpp`（`IDirect3D9`、`CreateDevice`、device-lost 处理） |
| **OpenGL**（navmesh 预览） | `navgen`（工具） | Windows | navmesh 可视化预览（`glBegin(GL_LINES)` / `glBegin(GL_POLYGON)`） | `opengl32`——`tools/navgen/CMakeLists.txt:100-98`（`BW_TARGET_LINK_LIBRARIES(... opengl32)`）；`<GL/gl.h>`、`HGLRC`（`navgen.cpp:89/343`）——Win32 GL 上下文 |
| **cocos2d / OpenGL ES** | `examples/client_integration/c_plus_plus/{android,ios}`（移动端客户端） | Android、iOS | 移动端客户端（含 `CocosDenshion` 音频、`cocoslive`、`FontLabel`） | cocos2d 源随树：`examples/client_integration/c_plus_plus/ios/libs/`；Android 原生 GL——`android/jni/Android.mk:51` `LOCAL_LDLIBS := -llog -landroid -lEGL -lGLESv1_CM` |
| **Awesomium（内嵌 Chromium）** | `web_app`（web 客户端） | Windows/MSVC | web 前端 UI 视图 | `third_party/awesomium/include`，`BW_AWESOMIUM_SUPPORT` 门控（`lib/web_render/CMakeLists.txt:9-14`，并为 MSVC 关闭优化）；接入点 `client/web_app.cpp:47/59/70` `PyAwesomiumProvider::{init,fini,tick}` |

**树内无其它图形后端**：对 `wgl|glX|CGL|glViewport` 的全树扫描（除 `third_party`）
仅命中 `tools/navgen/navgen.cpp`；`lib/moo` 无 OpenGL 平台 API，故非 Windows 桌面
客户端在原版树中**无可用渲染后端**（与第 1 节构建门一致）。

## 3. 逐平台能力及依赖（验收总表）

| 平台 | 图形能力 | 依赖 | 备注 |
|---|---|---|---|
| **Windows 桌面** | 完整 3D 客户端（D3D9）；工具 OpenGL 预览（navgen）；web UI（Awesomium） | `d3d9`/`d3dx9`/`dxguid`/`setupapi`/`psapi`、`opengl32`、`third_party/awesomium` | 唯一完整图形平台 |
| **Linux 桌面** | **无**（树内无 GL/其它后端，构建门亦只允许服务端） | — | 服务端无渲染需求；`moo` 对服务端的 client 依赖已由 `IF(NOT BW_IS_SERVER)` 剥离（`lib/moo/CMakeLists.txt:485-489`） |
| **macOS 桌面** | **无**（顶层 CMake 无 macOS 分支） | — | 参考无 macOS 桌面客户端可作基准；对应 BW-15-04-03 需以「原版样本 + 跨端混跑」验收（见 BW-15 开篇约定） |
| **Android** | 有（cocos2d / OpenGL ES 1.1 + EGL） | `cocos2d`、`-lEGL -lGLESv1_CM`、`-landroid`、`-llog` | 移动端独立栈，不复用 `moo` |
| **iOS** | 有（cocos2d / OpenGL ES） | `cocos2d`、`cocoslive`、`CocosDenshion`、`FontLabel`、`TouchJSON` | 同上 |

## 4. 缺口

- **第三方 SDK 未随树**：`moo` 的 `d3d9/d3dx9` 属 DirectX SDK（已随系统/SDK 提供），
  `navgen` 的 `opengl32` 属系统 GL，Awesomium 属外部二进制（`BW_AWESOMIUM_SUPPORT`）——
  `third_party/` 无 `awesomium/` 目录（与 BW-15-06-02 的 DCC SDK 缺口同类）。
- **新版渲染栈未建**：新版尚无 `rendering` crate（BW-11 未立），故本表在新版侧
  无对应实现可登记——这是分层依据，不是待办实现。
- **服务端不加载图形后端**：原版 `moo` 的 client 依赖由 `IF(NOT BW_IS_SERVER)` 剥离
  （`lib/moo/CMakeLists.txt:485-489`）；新版以 BW-16-02-02（服务端构建不加载图形后端）
  对齐该隔离，渲染后端随 BW-11 立项后再挂。

## 5. 边界

本档只**记录**原版图形后端的平台能力与依赖；新版渲染栈、编辑器视口、跨端客户端
验收随 BW-11 / BW-15-06-03/04 落地后按本表复测。
