# BW-00-05-05 核查发行物、SDK 和缺失资源 — 记录

- **任务编号**: BW-00-05-05（lane BW-00-05 功能覆盖台账，todo.md 行 148）
- **验收**: 缺口有来源与阻塞原因。

## 结论

台账新增 **23 行**（origin=release 15 + origin=gap 8），总行数 151→174。
8 个缺口**逐项有来源（登记处/文档）与阻塞原因**。

## 发行面（origin=release，15 行）

| 行 | 任务号 | 状态 | 说明 |
|---|---|---|---|
| makeServer.py / build_common.py | BW-14-04 | pending | 参考发行脚本；Boundless 等价面=ci/package.sh 夜间打包（BW-22-04-01），逐脚本复用未定 |
| makeRPM.py | BW-14-04 | blocked | 调 `make rpm`，但 rpm/everything 目标在参考树无定义（01-03 缺口） |
| 10 个 Windows 脚本（makeSolutions/cmake_all_windows/winArchiveBuild/winMakeClient/winCMakeClient/winCMakeShaders/winCMakeUnitTests/winMakeUnitTests/perforce_synchronizer/project_manager） | BW-14-04 | blocked | Windows-only；makeSolutions.py 另注硬编码 `C:\Python27\python.exe` 陈旧残留（01-03） |
| package-daily.sh | BW-14-04 | **covered** | 参考滚动发布脚本；Boundless 等价=`.github/workflows/package-daily.yml`+`ci/package.sh`（BW-22-04-01），无 tag/无 release |
| package-daily-windows.ps1 | BW-14-04 | blocked | Windows-only |

## 缺口（origin=gap，8 行，逐项来源+阻塞原因）

| 缺口 | 任务号 | 来源 | 阻塞原因 |
|---|---|---|---|
| make_rpm_target | BW-14-04 | makeRPM.py + 全树 `grep '^rpm:'`（01-03） | 参考树 rpm/everything 目标无定义 |
| bigworld_key | BW-00-02-03 | 00-02-03 启动日志（baseapp-blocked.log） | 商业许可密钥，全仓无、不可重建；挡 baseapp/cellapp/dbapp |
| loginapp_privkey | BW-00-02-05 | 00-02-03 启动日志（loginapp-blocked.log） | RSA 私钥缺失，loginapp 初始化即败 |
| twisted_module | BW-00-02-03 | 00-02-03（site-packages 未入路径） | dbapp 启动败；待 Python 迁移台账复核 |
| fantasydemo_art | BW-14-01 | generate-fantasydemo-assets.py docstring | 仓库无美术资源（仅 .wpr+web/php+占位 res） |
| openautomate_sdk | BW-11-05 | BW-00-01-04 决策表 | NVIDIA 专有 SDK，决策"不用"，功能替代缺口 |
| windows_runner | BW-00-02-05 | BW-00-01-02 平台矩阵 | Windows 真机/runner 缺失，挡 58 个 CMake-only 目标 + bwclient 基线 + Windows 发行脚本 |
| sdk_package | BW-14-02 | 01-02 bwentity/SDK 面 | Boundless 尚无 SDK 打包面（原版 SDK=bwentity DLL+entitydef 定义+examples） |

## 说明

- **SDK 面**：`bwentity` DLL（origin=cmake，BW-04-04）、`process_defs`/`entitydef`
  （make 行）已是索引；本项补 `sdk_package` 打包缺口。
- **发行物红线**：Boundless 夜间打包（package-daily.yml）**不建 tag、不发
  GitHub Release**（结果仅 workflow artifacts；版本号 release 用户触发），
  repo 全局红线一致。
- **vcpkg.json / cmake 输入**：属构建输入非发行物，未单列（BW-00-01-02
  已录平台矩阵）。

## 校验与门禁

- `python3 docs/review/tools/check_coverage_ledger.py`：**174 行全字段合法**，
  origin=make 79 项双向 1:1（cmake/resource/release/gap 行豁免清单核对），rc=0。
- `--schema-only` rc=0；`py_compile` 过；`--scaffold` 空。
- 本项纯台账；未触 crate。

## 边界声明

本项完成发行物/SDK/缺口索引（origin=release/gap）；完整性复核归 BW-00-05-06
（每个启用目标有运行证据或显式缺口）。

## 状态

- [x] 发行面索引（15 行：Linux/Windows/滚动发布）
- [x] 8 个缺口逐项有来源与阻塞原因
- [x] 校验绿（174 行，rc=0）
