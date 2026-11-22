# BW-14-02-02/03 原版 Android/iOS 示例互通测试——宿主缺口登记

**验收** 保存协议结果或宿主缺口。
**结论** 互通测试未运行；两侧缺口（移动宿主 + 新服务端登录）均已登记。协议结果未采集。

## 1. Android（BW-14-02-02）

参考示例：`programming/bigworld/examples/client_integration/c_plus_plus/android/`。

**缺口（宿主侧）**：
- 构建需 Android NDK（`Application.mk`：`APP_PLATFORM=android-9`、`APP_STL=gnustl_static`、`APP_ABI=armeabi-v7a`）。
- JNI 原生层（`android.app.NativeActivity` + `android_native_app_glue`）链接 `connection_model`（BigWorld 客户端连接模型库）+ `-llog -landroid -lEGL -lGLESv1_CM` + `libgnustl_static.a`。
- 运行需真机/模拟器（EGL/GLESv1 渲染面）。
- 本机为 Linux：无 Android NDK 工具链、无设备/模拟器。

**缺口（服务端侧）**：互通目标为新服务端登录链路——loginapp 挑战协商（BW-06-01-02）+ baseapp onLogin（BW-07-01）均未落地，`apps/server` 仅最小入口（进程生命周期 + 信号退出）。

## 2. iOS（BW-14-02-03）

参考示例：`programming/bigworld/examples/client_integration/c_plus_plus/ios/`。

**缺口（宿主侧）**：
- 构建需 macOS + Xcode（`FantasyDemo.xcodeproj`/`FantasyDemo.xcworkspace`）。
- 依赖 cocos2d 系库（`libs/`：cocos2d、cocoslive、CocosDenshion、FontLabel、TouchJSON）+ `Settings.bundle`。
- 本机为 Linux：无 macOS 宿主（与 BW-15-01-01 的 macOS runner 卡点同批，随三端 runner 落地）。

**缺口（服务端侧）**：同上，新服务端登录链路未落地。

## 3. 复验条件

1. Android：NDK + 设备/模拟器（或 CI runner）。
2. iOS：macOS + Xcode（或 CI runner）。
3. 服务端：登录链路落地（BW-14-01-02 的前置：BW-06-01-02、BW-07-01）。
三者齐备后运行互通测试并保存协议结果（替代本缺口登记）。
