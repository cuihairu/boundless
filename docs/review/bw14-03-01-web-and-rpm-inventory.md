# BW-14-03-01 核对 PHP 页面与 RPM

**验收** 列出能够验证的接口和页面。
**结论** PHP 页面 28 项、Web 接口契约、RPM 交付内容逐项列出并给出可验证性。

## 1. PHP 页面（`programming/fantasydemo/web/php/`，28 项）

| 组 | 文件 | 作用 |
|---|---|---|
| 认证/会话 | `Login.php` | 登录（用户名/密码 → `BWAuthenticator::authenticateUserPass`）与 token 续期 |
| | `Session.php` | 会话存取（`$_SESSION[KNOWN_URL]` 等） |
| | `Authenticator.php` | 认证基类 |
| | `BWAuthenticator.php` | BigWorld 认证：`RemoteEntity("db")` 传 username/password，取回 type/id |
| | `Disconnect.php` | 登出/断开 |
| 角色 | `Characters.php` | 角色列表 |
| | `Character.php` | 单角色详情 |
| 物品 | `Inventory.php` | 背包 |
| | `Items.php` | 物品列表 |
| | `ItemViewer.php` | 物品查看 |
| | `ViewItem.php` | 物品展示页 |
| 拍卖 | `PlayerAuctions.php` | 我的拍卖 |
| | `SearchAuctions.php` | 拍卖搜索 |
| 框架/工具 | `BigWorld.php` | ServiceApp URL 单例（`KNOWN_URL`，默认 `http://localhost:13000`） |
| | `Page.php` / `AuthenticatedPage.php` | 页面基类（后者需登录态） |
| | `MenuBar.php` / `News.php` | 导航栏 / 公告 |
| | `Constants.php` / `CustomErrors.php` / `BWError.php` / `Debug.php` / `Util.php` / `Image.php` / `XHTML-MP-functions.php` | 常量/错误/调试/工具/图片/移动 XHTML |
| | `index.php` | 入口 |
| 素材 | `images/`、`styles/` | 静态资源（含 `items/`） |

## 2. Web 接口契约

- 后端 = ServiceApp 的 twisted Web 服务（`BigWorld.php`：`http://localhost:13000`，`KNOWN_URL` 存会话）。
- 认证 = `RemoteEntity("db")` + `authenticateUserPass(username, password)` → 返回 `{type, id}`（entity 类型 + 实体 id）；`doesAuthTokenExist`/`authenticateSessionToken` 复用会话 token。
- 会话 = `$_SESSION[KNOWN_URL]` 存当前在线 ServiceApp URL（README：多 ServiceApp 会话期分派）。

## 3. RPM 交付内容（`fixwork/` 三个 x86_64 RPM）

| RPM | 大小 | 交付内容 |
|---|---|---|
| `bigworld-bwmachined-14.4.1.el7.x86_64.rpm` | 4.4MB | **真 payload**（`rpm2cpio`/`cpio -t` 列出）：`etc/bwmachined.conf`、`etc/sysctl.d/bwmachined2.conf`、`opt/bigworld/14.4/bwmachined/{init.d/bwmachined2,sbin/bwmachined2}`、`opt/bigworld/current/bwmachined`、`usr/lib/systemd/system/bwmachined2.service` |
| `bigworld-devel-14.4.1.el7.x86_64.rpm` | 2.1KB | **占位**（`rpm -qlp`：Size=0、无 payload；头信息 Name/Version=14.4.1/Group=Middleware/MMOG/License=BigWorld License 在，payload 被裁） |
| `bigworld-web-integration-14.4.1.el7.x86_64.rpm` | 1.9KB | **占位**（同 devel：Size=0 无 payload）——PHP 页面在源树而非 RPM payload |

## 4. 可验证性

- **现可验**：页面/接口清单（本表）；bwmachined RPM payload（`rpm2cpio` 列出，含 systemd unit 与 bwmachined2 二进制路径）。
- **运行时不可验（缺口）**：页面调用后端 = ServiceApp Web 服务（BW-07-03-02 启动并注册 Service 未勾）+ 登录链路（BW-14-01-02 卡点：BW-06-01-02/BW-07-01），当前无 ServiceApp 可响应 `RemoteEntity("db")`；devel/web-integration RPM payload 被裁（占位），真实包随 BW-14-04 发行物与 BW-00-02 基准。
