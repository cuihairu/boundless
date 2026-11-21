# BW-00-02-03 录制单机集群启动基线 — 记录（部分，卡点）

- **任务编号**: BW-00-02-03（lane BW-00-02 原版运行基准，todo.md 行 102）
- **参考树**: `/home/cui/workspaces/BigWorld`（构建 + 启动尝试，只改构建产物，未改源码）。
- **Boundless 模块**: 无（原版单机集群基线，供 BW-00-02 差分验收引用）。

## 进展：服务端二进制全量构建（此前未建）

参考树服务端二进制**此前未建**（见 BW-00-02-01：库阶段完成、可执行未产出）。本次用 `make -j$(nproc)`（`VCPKG_ROOT=/home/cui/vcpkg`，工具链 g++ 15.2 `-std=c++23`——本参考树已迁移 c++23，与 el7 构建一致）完成全量构建，**成功无错误**，产出（`game/bin/server/el7/`）：

| 二进制 | 位置 |
|---|---|
| baseapp / serviceapp / baseappmgr / cellapp / cellappmgr / dbapp / dbappmgr / loginapp / reviver / bots | `server/el7/server/` |
| bwmachined2 | `server/el7/tools/` |
| clear_auto_load / consolidate_dbs / sync_db / transfer_db / remove_db | `server/el7/server/commands/` |

（构建日志 `/tmp/bw-build-full.log`，373 次编译 + 链接，末步 `cp baseapp serviceapp`。）

## 启动尝试（fantasydemo 最小配置）

资源路径：`fantasydemo/res:game/res/bigworld`（simple client README 给出的运行面）；配置 `server/bw.xml`（fantasydemo 最小版，BWConfig::init 读入，缺失段用默认值）。进程以 `--res <路径>` 启动，bwmachined2 `-f` 先起（接口发现回退全接口，单机可接受）。

| 进程 | 结果 | 说明 |
|---|---|---|
| bwmachined2 | 启动 | 后台运行（接口发现用） |
| baseappmgr | **运行** | 达 `Version: 14.4.1`，持续运行至超时（日志 `bw00-02-03-baseappmgr-start.log`） |
| cellappmgr | **运行** | 达 `Version: 14.4.1`，持续运行（`metaLoadBalancePriority` 配置段缺失警告，仍运行） |
| dbappmgr | **运行** | 达 `Version: 14.4.1`，持续运行 |
| loginapp | **失败** | `Couldn't load private key from non-existent file server/loginapp.privkey`（RSA 密钥缺失，日志 `bw00-02-03-loginapp-blocked.log`） |
| baseapp | **失败** | `No key file server/bigworld.key found`（BigWorld 许可密钥缺失，日志 `bw00-02-03-baseapp-blocked.log`） |
| cellapp | **失败** | 同上（`bigworld.key` 缺失） |
| dbapp | **失败** | `No key file server/bigworld.key found` + `ModuleNotFoundError: No module named 'twisted'`（许可密钥 + 服务端 site-packages 未入路径） |

## 卡点（外部条件，照实登记）

1. **`server/bigworld.key`（BigWorld 许可密钥）缺失**：baseapp/cellapp/dbapp 均要求该文件（`ERROR: No key file server/bigworld.key found. Either place your server/bigworld.key file at the root of your resource tree...`）。全仓搜索无 `bigworld.key`（仅 curl 测试证书 `*.key`）——**商业许可制品，被裁剪，不可重建**。
2. **`server/loginapp.privkey`（RSA 私钥）缺失**：loginapp 初始化即失败——同样被裁剪（部署级密钥制品）。
3. **dbapp 的 `twisted` 模块**：Python 服务端模块（el7 third_party wheels 已建但未入 `sys.path`）——次要，随许可密钥一并待补。

**结论**：单机集群的**管理器层**（baseappmgr/cellappmgr/dbappmgr）可在本参考树启动并运行；但**应用层**（baseapp/cellapp/dbapp）与 **loginapp** 被 BigWorld 商业许可密钥 / 部署密钥阻断——**完整单机集群拓扑与运行日志无法从当前参考树获取**。

## 状态（部分，未勾）

服务端二进制全量构建成功（新进展）；管理器启动基线已录（3 个日志）；应用层/登录被 `bigworld.key`/`loginapp.privkey` 阻断（外部许可制品，不可重建）——**待用户提供合法许可密钥或改用授权环境后补录完整基线**。
