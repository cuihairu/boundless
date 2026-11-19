# Boundless 的 BigWorld 功能移植 TODO

参考仓库为 [../BigWorld](../BigWorld/README.zh.md)，源码基准为 `4657a17cf10d55e2fbebf258f96da96a3dbe0c22`。清单来自该提交的目录、构建入口、源码锚点及仓库文档。本次完成静态盘点，所有勾选项均为待办，构建、混跑和性能数据留待各项验收。上游 README 记录的服务端技术基准为 Linux x86-64、C++23、嵌入 CPython 3.13.16。客户端与编辑器需单独复测历史 Windows 构建。

## 1. BigWorld 功能对照清单

基准为 [BigWorld 源码导读](../BigWorld/docs/zh/analysis/source-code-guide.md) 和本地提交的构建清单、源码目录。勾选表示对应实施任务已经通过验收。此处是功能索引，后文编号是实施和验收依据。

### 运行时与分布式服务

- [ ] 基础类型、数学、二进制流、压缩、时钟、容器和后台队列。BW-01。
- [ ] DataSection、XML/Binary/Packed/ZIP、资源搜索、缓存和配置继承。BW-01。
- [ ] Mercury 事件循环、可靠 UDP、TCP、Packet/Bundle/Channel、RPC、包过滤和加密互通。BW-02。
- [ ] CPython 嵌入、原生对象与 Python 桥接、数学/资源/网络/定时器 API 和接通的脚本重载。BW-03。
- [ ] EntityDef、类型/属性/方法、Mailbox 契约、实体定义摘要及 `process_defs`。BW-04。
- [ ] Machined/机器发现、进程注册、出生和死亡通知、服务探活与 Reviver。BW-05-01/02。
- [ ] BaseAppMgr、CellAppMgr、DBAppMgr 的工作进程管理、负载、Space 切分和集群关闭。BW-05-03~06。
- [ ] LoginApp 版本与认证、账号登录和重登、Proxy 会话及客户端连接。BW-06。
- [ ] Base 实体、Cell 配对、Service 实例、共享数据、全局实体和跨进程 Mailbox。BW-07-01~03。
- [ ] Base 备份接管、归档、下载与录像写入。BW-07-04/05。
- [ ] Cell/Space、Real 实体、Controller/Extra、移动、传送、乘客和感知。BW-08-01/02/06。
- [ ] AOI/Witness、Ghost 复制、消息缓冲、Offload 迁移及 Cell 故障恢复。BW-08-03~05。
- [ ] DBApp 实体读写、账号与 Billing、MySQL/XML、SQLite 二级库、整理、快照和同步工具。BW-09-01~05。

### 地图、导航与客户端

- [ ] Chunk 室内外流式装载、Portal、空间映射、场景查询、compiled space。BW-10-01/02。
- [ ] Terrain2 高度、地形洞、材质、BSP、射线、扫掠和碰撞。BW-10-03。
- [ ] Waypoint 集、NavLoc、角色体积 girth、Portal 限制、跨 Chunk 路径和缓存。BW-10-04。
- [ ] Waypoint flood、Recast 生成、NavGen 图形入口、导航数据输出与重载。BW-10-05。
- [ ] CellApp 寻路和移动控制器、Python 寻路接口、寻路成功/失败回调和端到端地图往返。BW-10-06/07。
- [ ] 客户端窗口、输入、相机、玩家、运动滤波、客户端实体、UDO、回放和下载。BW-11-01~03。
- [ ] Moo 渲染、模型、动画、材质、特效、地形、植被、粒子、后处理、GUI、声音、VOIP、网页和条件 SDK。BW-11-04/05。

### 工具、观测与示例

- [ ] Watcher、MessageLogger、Profiler/`_bw_profile`、状态查询与 Cell Viewer。BW-12、BW-23、BW-25。
- [ ] Bots 客户端、脚本控制和巡逻，单测夹具、消息过滤、多进程用例及原版差分。BW-12-04、BW-17/18。
- [ ] 资源转换与增量编译、资源检查、打包、导航生成和离线处理。BW-13-01/02。
- [ ] WorldEditor、ModelEditor、ParticleEditor、DCC 导出、插件、启动器和共享编辑代码。BW-13-03~06。
- [ ] FantasyDemo、Python/Android/iOS 客户端示例、扩展样例、Web Integration、打包与 VitePress 文档。BW-14、BW-19。

## 2. KBEngine 功能对照清单

参考本地 [KBEngine](../kbengine/README.zh.md) 提交 `55a6655203e75d1019c9a9ddbdefb38f1b79260c`。这里逐项记录取舍。**纳入**表示 Boundless 有对应实施任务，以 BigWorld 兼容要求或明确的新增设计为准。**复核**表示需先确认是否纳入，复核项的勾表示范围决策已记录。

| 编号 | 本地源码入口 |
| --- | --- |
| KB 源码 | `../kbengine/kbe/src/` 下的 `lib/`、`server/` 及 `server/tools/` |
| KB 配套 | `../kbengine/kbe/tools/`、`kbe/res/sdk_templates/`、`typings/`、`tests/` 和 `docs/` |

- [ ] **KB-01 纳入** · Machine 与组件发现、注册、进程状态、心跳和关闭。对应 BW-05、BW-25。
- [ ] **KB-02 纳入** · BaseApp/BaseAppMgr 登录分配、代理、实体工厂、跨进程引用和全局数据。对应 BW-05~07。
- [ ] **KB-03 纳入** · CellApp/CellAppMgr、Space、实体同步、AOI、跨进程迁移和控制器。对应 BW-05、BW-08。
- [ ] **KB-04 纳入** · LoginApp/DBMgr 的账号与实体读写、重复登录及 MySQL。对应 BW-06、BW-09。
- [ ] **KB-05 纳入** · 资源配置、网络消息、EntityDef、Python 脚本、线程任务和定时器。对应 BW-01~04。
- [ ] **KB-06 纳入** · 地图资源、寻路、地图异步加载与路径结果。BigWorld 行为见 BW-10；对比 KBEngine `navigation_mesh_handle`、`navigation_tile_handle` 与 `loadnavmesh_threadtasks` 的具体差异见 BW-29-01。
- [ ] **KB-07 纳入** · Watcher、组件状态、日志和网络、原生、Python profile。对应 BW-12、BW-23、BW-25。
- [ ] **KB-08 纳入** · Telnet 式交互诊断和内存查看体验。对应 BW-24，使用 Boundless 本地只读接口。
- [ ] **KB-09 纳入** · Bots 与内部测试、CI 构建、VitePress 文档和跨平台发行。对应 BW-15、BW-17~19、BW-22。
- [ ] **KB-10 纳入** · SDK 模板与 Unity/Unreal 客户端接入。Boundless 的独立 SDK 和多引擎示例见 BW-21。
- [ ] **KB-11 纳入** · `kbcmd`、GUIConsole、WebConsole、pycluster 的状态查询、日志和运维工作流。Boundless 已计划部分见 BW-24/25/27，额外工作流的取舍见 BW-29-02。
- [ ] **KB-12 复核** · Redis 数据库实现、Interfaces 对接及订单/回调、邮件辅助。对应 BW-29-03。
- [ ] **KB-13 复核** · LoginApp 的 `clientsdk_downloader`、JavaScript SDK 模板、Python assets 模板与类型存根。对应 BW-29-04。
- [ ] **KB-14 复核** · `xlsx2py` 表格转换、安装脚本和新建 assets 工程脚手架。对应 BW-29-05。

## 3. Boundless 实施清单

### 编号和实施口径

- `BW-模块-任务` 按功能编号。第一轮逐项对齐原版，并标明 Boundless 新增的交付能力。第二轮用 `OPT-模块-任务` 对应同一功能域。执行顺序见文末里程碑。
- 第一轮由 Rust 承载原生引擎、协议、进程、存储和客户端运行时，由 Python 承载原版脚本 API、游戏逻辑及适合的工具。先沿用参考版本的 CPython 3.13 行为。嵌入和原生扩展的具体实现待最小原型验证。
- 一比一以外部可观察行为验收。保留适用的进程边界、消息编号及线格式、配置默认值、数据格式、脚本 API 与回调次序、状态转换和错误语义。Rust 内部用等价实现表达 C++ 宏、对象生命周期和内存管理。平台、编译宏与依赖版本进入验收记录。
- 每项先定位构建目标及调用方，采集原版结果，再实现、比对并留存证据。对时间、随机值、地址等易变字段固定输入或显式归一化。遇到原版不可运行、未启用或缺资源的能力，记录原因和可获得的证据，保留待验状态。
- 下文源码路径默认相对于 `../BigWorld/programming/bigworld/`。版本以 Git 跟踪内容为准。工作区中的 `game/`、RPM 和完整资源下载可作为补充样本，使用前分别记录版本和来源。

## 第一轮 · 一比一移植

### BW-00 基准、构建与兼容性测试

源码和资料 `programming/Makefile`、`build/`、`cmake/`、`vcpkg.json`、[TESTING.md](../BigWorld/TESTING.md)、[源码导读](../BigWorld/docs/zh/analysis/source-code-guide.md)。

- [ ] **BW-00-01 构建目标清册**。展开 Make/CMake、发布脚本及条件编译，列出可执行文件、库、扩展和平台矩阵。逐项标明已参编、条件启用、仅源码、仅发行物或缺依赖。整理 `third_party/`、资源许可证和 NOTICE，确认哪些复用依赖、哪些需替代实现。
- [ ] **BW-00-02 原版运行基准**。记录二进制哈希、编译选项、配置、Python 标准库和数据库版本。复跑上游单测入口并记录实际参编用例。获取单机集群、最小客户端、Windows 客户端及工具的可运行基准。
- [ ] **BW-00-03 Boundless 构建入口**。建立 Cargo workspace、Python 模块及启动入口，固定工具链和依赖。为本地资源、配置、原版二进制和测试夹具设独立路径。验收为干净检出可构建、启动、关闭并保留日志。
- [ ] **BW-00-04 差分验收工具**。建立二进制帧、Python 回调序列、资源产物、实体快照及数据库行的基准样本。支持原版全栈、新版全栈和替换单个组件三种模式。按兼容边界报告字段差异、超时及回调顺序差异。
- [ ] **BW-00-05 功能覆盖台账**。逐项核对 `lib/`、`server/`、`server/tools/`、`client/`、`tools/`、`common/`、`examples/` 和 FantasyDemo 的构建目标与用户功能。记录「原版路径、启用条件、功能编号、验证夹具、覆盖状态」。对第三方 SDK、发行物中的独有内容和缺失资源单独记录来源与验证缺口。每个已启用目标都要有可运行验收或可追踪的阻塞原因。

### BW-01 基础运行时、配置和资源

源码 `lib/cstdmf/`、`lib/math/`、`lib/resmgr/`、`lib/server/bwservice.hpp`、`lib/server/server_app.cpp`、`lib/build/`、`lib/memhook/`。

- [ ] **BW-01-01 公共类型和数学**。移植 ID、时间戳、字符串与容器等被外部使用的语义，以及向量、矩阵、四元数、包围盒、坐标变换。比对轴向、数值边界、精度和二进制布局。
- [ ] **BW-01-02 流与基础编解码**。复现 BinaryStream、定长和变长整数、字符串、内存流、压缩流及读越界报告。以原版生成的样本双向读写。
- [ ] **BW-01-03 DataSection 和资源路径**。覆盖 XML/Binary/Packed/ZIP、目录与文件系统搜索顺序、重复节点索引、路径映射、缓存、更新和保存语义。比对同名覆盖、空节点及循环读写产物。
- [ ] **BW-01-04 进程生命周期与配置**。覆盖入口参数、`BWResource`、`BWConfig`、`bw.xml` 配置继承和覆盖、初始化、信号、退出码、分阶段关服。比对配置信息缺失、启动失败和正常关闭。
- [ ] **BW-01-05 Tick、定时器和后台队列**。实现游戏时钟、Updatables、任务完成回主线程及关服排空。比对同 tick 多事件、取消、回调内重新注册，以及后台任务完成与销毁的先后。

### BW-02 Mercury 网络、RPC 和连接基础

源码 `lib/network/`、`lib/connection/`、`common/py_network.cpp`。

- [ ] **BW-02-01 I/O 与事件分发**。覆盖 Endpoint、EventPoller、EventDispatcher、`NetworkInterface`、socket 缓冲配置和定时器。记录网络、频繁任务与计时器同轮就绪时的顺序。
- [ ] **BW-02-02 接口注册和 RPC**。从接口定义及宏展开提取消息 ID、固定或可变长度、handler、请求/应答 ID 与超时。比对内部和外部接口的字节序、包头和请求回调。
- [ ] **BW-02-03 可靠 UDP**。实现 Packet、Bundle、UDPChannel、ACK/序号窗口、重传、乱序处理、分片重组、Channel 生命周期及统计。双向混跑并覆盖丢包、延迟、重复包和序号回绕。
- [ ] **BW-02-04 TCP 与外部传输**。移植 TCPBundle/TCPChannel、帧边界、读写缓冲、连接过滤及在构建中启用的传输方式。对照短读写、背压、超时和断链。
- [ ] **BW-02-05 包过滤和加密互通**。按原版启用条件复现过滤链、会话加密协商、包校验和受控限流，使用双方合法客户端互操作。对照正常握手、认证失败及流量达到配置上限时的结果。

### BW-03 CPython 嵌入和脚本 API

源码 `lib/script/`、`lib/pyscript/`、`common/`、各进程 `script_bigworld.cpp`、[Python 迁移台账](../BigWorld/docs/python-313-migration.md)。

- [ ] **BW-03-01 解释器生命周期**。对齐初始化、模块注册、资源导入路径、标准库加载、脚本日志、异常报告和退出顺序。验证各进程启动、空脚本和异常脚本。
- [ ] **BW-03-02 Python 对象桥接**。覆盖原生对象包装、属性与方法绑定、容器代理、引用保活、GC、GIL 和跨线程回调约束。比较对象身份、异常类型、回调析构与循环引用。
- [ ] **BW-03-03 公共脚本模块**。移植脚本数学与 Provider、时间和定时器、资源访问、网络/URL 请求（`lib/urlrequest/`）及共享模块。按进程盘点模块名、参数、返回值和异常，通过原样调用的 Python 测试。
- [ ] **BW-03-04 进程脚本入口与热重载**。复现角色专属 `BigWorld` API、脚本启动回调和当前实际接通的重载行为。比较类替换、已有实例、失败后的状态和引用清理。以 [热重载生产化计划](../BigWorld/docs/upgrade-plan/hot-reload-productionization.md) 反查计划项与已落地代码。

### BW-04 EntityDef、实体数据与 RPC 契约

源码 `lib/entitydef/`、`lib/entitydef_script/`、`lib/bwentity/`、`tools/process_defs/`。

- [ ] **BW-04-01 定义解析与编译**。覆盖 `entities.xml`、`.def`、`alias.xml`、父类和接口、Service 定义、属性默认值与索引、Base/Cell/Client 方法排序。比对实体、属性、方法 ID 和定义摘要。
- [ ] **BW-04-02 数据类型和属性流**。覆盖基本类型、ARRAY/TUPLE、FIXED_DICT、USER_TYPE、MAILBOX、嵌套属性和变化切片。比对 Python 类型转换、默认值、压包、广播和持久化字段。
- [ ] **BW-04-03 可见性与调用约束**。覆盖 exposed 方法、属性分发标志、客户端/服务端权限、参数校验、方法应答和 MailBox 引用。比对同一实体定义在 Base、Cell、Client 和 DB 的投影。
- [ ] **BW-04-04 定义工具和 SDK 契约**。复现 `process_defs` 的命令行、诊断及产物。核对 `bwentity` 和客户端集成示例依赖的定义接口与生成结果。

### BW-05 集群控制、进程发现和恢复

源码 `server/tools/bwmachined/`、`lib/network/machine_guard.cpp`、`server/reviver/`、`server/{baseappmgr,cellappmgr,dbappmgr}/`。

- [ ] **BW-05-01 Machined 与服务发现**。复现 MachineGuard 查询、进程注册/注销、发现、出生/死亡通知、主机和进程统计，以及原版工具所用的受控进程管理入口。用原版组件和新版守护进程做双向互测。
- [ ] **BW-05-02 Reviver 外部恢复**。覆盖组件探活、优先级、异常重启、重新注册和关服抑制。验收为进程退出和失联后恢复流程与原版等价，且正常停机不会触发误重启。
- [ ] **BW-05-03 BaseAppMgr 调度**。覆盖 BaseApp 注册/移除、负载报告、创建目标选择、登录条件、全局状态转发和关闭协调。比较多实例下的调度和失败回退。
- [ ] **BW-05-04 CellAppMgr 分区和负载均衡**。覆盖 Space/Cell 创建与销毁、BSP 切分、边界报告、负载采样、拆并 Cell、跨主机分配和 viewer 读接口。用原版轨迹对照 split/merge 和重分配。
- [ ] **BW-05-05 DBAppMgr 发现和主从角色**。覆盖 DBApp 注册、主节点选取、负载/状态转发，以及死亡后的拓扑和服务通知。比对已登录实体和未完成请求的恢复结果。
- [ ] **BW-05-06 集群阶段与故障矩阵**。对齐启动就绪条件、时间同步、受控关服及经理/工作进程失联通知。逐角色验证重启、网络隔离、进程恢复及重复通知，记录哪些故障原版支持自动恢复。

### BW-06 LoginApp、BaseApp 会话与客户端连接

源码 `server/loginapp/`、`server/baseapp/{proxy.cpp,login_handler.cpp,pending_logins.cpp}`、`server/dbapp/*login*`、`lib/{connection,connection_model}/`。

- [ ] **BW-06-01 LoginApp 公共登录入口**。复现登录参数和版本检查、挑战的启用和应答、请求缓存、DB 转发、过载及失败状态。比对正常和失败登录的响应状态及线格式。
- [ ] **BW-06-02 账号认证与重复登录**。串通 LoginApp、DBApp billing、BaseAppMgr 和目标 BaseApp。覆盖认证拒绝、登录记录、重复登录/重登和账号实体选择。
- [ ] **BW-06-03 BaseApp 接续与 Proxy**。复现登录地址切换、pending login/session key、Channel 绑定、Proxy 与玩家实体绑定、断线/踢出/重新连接和会话清理。比较回调和状态顺序。
- [ ] **BW-06-04 客户端连接状态机**。复现连接模型、服务端发现、切服、RPC/属性消息派发、时间同步、上传移动输入及下载流。用 headless 与原版客户端分别验证登录、进入世界、断开及重登。

### BW-07 BaseApp 实体与服务

源码 `server/baseapp/`、`server/baseapp_extensions/`、`lib/appmgr/`。

- [ ] **BW-07-01 Base 实体创建与销毁**。覆盖动态创建、从 DB 装载、ID 分配、初始化、定时器、驻留、销毁及失败回调。比对内存和持久化状态、重复回调及 ID 归还。
- [ ] **BW-07-02 Cell 配对与 MailBox**。覆盖 `createCellEntity`、`cellData`、Cell 创建完成/丢失通知、跨进程方法调用、Mailbox 路由与迁移后重定位。
- [ ] **BW-07-03 全局实体、Service 与共享数据**。复现 global bases、服务实例启动、注册和分发、SharedData 异步更新与删除。按原版部署方式验证跨 BaseApp/CellApp 顺序与恢复。
- [ ] **BW-07-04 备份、接管和归档**。移植 backup hash/发送、备份接收、offloaded backup、Archiver、CellApp 死亡处理和恢复。分别对照故障前、故障窗口和恢复后的实体/会话状态。
- [ ] **BW-07-05 下载与回放写入**。覆盖客户端流式数据下载、回放头、压缩块、tick 写入和完成回调。与 Cell 采集及客户端回放读取联验。

### BW-08 CellApp、Space、AOI 与实体迁移

源码 `server/cellapp/`、`server/cellapp_extensions/`、`lib/server/`；设计索引 [实体迁移](../BigWorld/docs/zh/architecture/entity-migration-offload.md)、[AOI/Witness/Ghost](../BigWorld/docs/zh/architecture/aoi-witness-ghost.md)。

- [ ] **BW-08-01 Cell/Space 生命周期**。覆盖 Space 创建、拓扑变化、空间数据、物理空间后端及卸载、Cell 注册与销毁。对照多个 Space、空 Space 和 Cell 回收。
- [ ] **BW-08-02 Real 实体与控制器**。复现实体创建、Tick、Controller/Extra 挂接、移动/转向/加速、定时器、寻路、乘客、传送与销毁。比对轨迹、控制器持久状态和回调次序。
- [ ] **BW-08-03 AOI 和 Witness**。移植 RangeList/RangeTrigger、Witness、EntityCache、优先级/带宽调度、进入/离开事件、位置和属性同步。覆盖半径边界、实体移动、自身实体及多个观察者。
- [ ] **BW-08-04 Ghost 复制和缓冲**。覆盖 Ghost 建立、跟随和销毁、属性/位置复制、Real 地址追踪、消息缓冲和重放。验证邻 Cell 穿越期间客户端看到的实体序列。
- [ ] **BW-08-05 Offload、跨区传送和故障**。移植迁移判定、Real/Ghost 角色转换、切换时重定向、控制器和回调状态迁移、CellApp 死亡处理。验证两 Cell 双向穿越、迁移中消息和目标失联。
- [ ] **BW-08-06 感知、观测和回放采集**。覆盖 vision/proximity/noise、cell viewer、负载节流、回放事件采集和 tick 边界，并比对掉帧或资源慢加载时的状态。

### BW-09 DBApp、持久化和数据库运维

源码 `server/dbapp/`、`server/dbapp_extensions/`、`lib/{db,db_storage,db_storage_mysql,db_storage_xml,sqlite}/`、`server/tools/{sync_db,consolidate_dbs,transfer_db,snapshot_helper,clear_auto_load,remove_db}/`。

- [ ] **BW-09-01 DBApp 实体操作**。复现 get/load/write/delete/lookup、DBID 分配、在线实体记录、按类型查找和回调。对照写入、调用重试、并发登录与重复载入。
- [ ] **BW-09-02 MySQL 持久化**。还原属性到表和列的映射、字节与字符集、账户表、自定义类型、事务、锁、后台任务和同实体操作顺序。既有数据库可读写，比较 SQL 结构、行内容及事务提交/回滚结果。
- [ ] **BW-09-03 XML 后端和 SQLite 二级库**。覆盖 XML 存储、BaseApp 二级库、归档整合、auto-load 和重启恢复。用原版生成的数据执行导入、读写、合并和恢复。
- [ ] **BW-09-04 Billing 与扩展**。移植标准、Python、自定义和实际参编的其他认证后端及扩展入口。对照成功、失效、重复请求和依赖不可用时的错误状态。
- [ ] **BW-09-05 运维工具**。复现建表和同步、dry-run、快照、传输、二级库合并、清理 auto-load 及移除辅助工具。比较命令行、退出码、输出和操作前后数据。
- [ ] **BW-09-06 PostgreSQL 持久化**。作为 Boundless 新增的 DBApp 主存储后端，与 MySQL、XML 共用存储接口，通过配置选择。实现实体/账号字段映射、DBID 分配、索引与约束、事务、并发锁和连接恢复。复用存储契约测试，覆盖创建、装载、更新、删除、账号重登、auto-load 和二级库归档合并。以原版 MySQL 路径的实体状态、错误语义与回调结果作为行为基准。
- [ ] **BW-09-07 PostgreSQL 数据库运维**。提供建库、schema 同步与 dry-run、备份恢复和现有 MySQL 数据导入。用原版样本核对实体 ID、账号关联、二进制字段、默认值和下一个可分配 DBID。验证迁移失败时的状态和重试行为。

### BW-10 世界资源、碰撞和导航

源码 `lib/{chunk,chunk_loading,chunk_scene_adapter,compiled_space,space,scene,terrain,physics2,waypoint,waypoint_generator,navigation_recast,delegate_interface}/`、`common/{py_chunk.cpp,py_terrain.cpp,py_physics2.cpp}`。

- [ ] **BW-10-01 Chunk 和空间加载**。复现室内外 Chunk、边界/Portal、映射、装载和卸载、空间索引及场景适配。按加载轨迹比对绑定状态和对象集合。
- [ ] **BW-10-02 编译空间和场景查询**。覆盖 compiled space 读取、渲染/查询对象注册、静态和动态场景、委托接口及实体空间映射。比对由原版转换器产出的资源读入结果。
- [ ] **BW-10-03 地形与物理**。覆盖高度、洞、材质、碰撞三角形、BSP、空间树、射线/扫掠及 Portal 状态。对照边界点、重叠面、不同材质和坐标系。
- [ ] **BW-10-04 Waypoint 寻路运行时**。对齐 `lib/waypoint/` 的 NavLoc 定位、waypoint set 邻接、按 girth 选路、局部与跨 Chunk 搜索、路径缓存及 Portal 限制。用相同资源比较路径点、不可达、搜索距离上限和缓存失效。
- [ ] **BW-10-05 NavGen 与导航资源生成**。分别追踪 `lib/waypoint_generator/` 的 flood/generator、`lib/navigation_recast/recast_generator.cpp` 及 `tools/navgen/` 的启用条件和输出。覆盖地形、斜坡、室内外接缝和不同 girth，验证旧资源读取和新版生成物往返。原版 NavGen 有 Windows 图形入口。
- [ ] **BW-10-06 CellApp 与 Python 寻路**。沿 `entity_navigate.cpp`、`navmesh_navigation_system.cpp`、`navigation_controller.cpp` 对齐 `navigate`、`navigateStep`、`canNavigateTo`、`navigateFollow`、`navigatePathPoints`、`getStopPoint`、`waySetPathLength` 与移动/加速 API。核对轨迹、控制器取消、成功/失败回调和跨 Cell 迁移后的状态。
- [ ] **BW-10-07 地图端到端验收**。以带地形高低差、障碍、Portal 和两个 Chunk 的固定地图，覆盖编辑与生成、服务端加载、客户端查询、Bots 穿越和导航调试显示。对照服务端坐标、路径、地形洞与碰撞结果。绑定已启用的原版目标逐平台验收。

### BW-11 客户端、渲染与交互

源码 `client/`、`lib/{moo,model,duplo,romp,camera,input,ashes,particle,post_processing,fmodsound,speedtree,scaleform,web_render,emptyvoip,open_automate}/`。

- [ ] **BW-11-01 应用生命周期与输入**。移植窗口、设备初始化、帧阶段、输入、设置、相机、帧率/LOD 调节和调试视图。复测窗口/全屏、焦点切换、退出及设备重建。
- [ ] **BW-11-02 客户端实体与运动**。覆盖 Player、客户端独有实体、UDO 工厂、实体选择、移动滤波、预测和修正、场景实体绑定。回放固定服务端输入，对照轨迹和 Python 回调。
- [ ] **BW-11-03 回放与资源下载**。用原版录像校验头/元数据/压缩块/checksum 及 tick 读取，覆盖播放、跳转、结束和下载分片。新版产物交回原版支持的读取入口验收。
- [ ] **BW-11-04 渲染和世界表现**。迁移 Moo 的模型/骨骼动画、材质/纹理/特效、光照、地形渲染、环境、植被、粒子、后处理及相机。逐项核对资产格式、渲染状态与固定镜头截图。记录材质转换和视觉容差，分别验收三端。
- [ ] **BW-11-05 GUI、音频和集成**。覆盖 Python GUI、界面事件、迷你地图、声音、VOIP、网页嵌入及 SDK 条件功能。以原版样本比较接口和行为，给缺 SDK 的功能保留兼容待办。

### BW-12 观测、运维接口和测试客户端

源码 `lib/{cstdmf,network,server,testing,unit_test_lib}/`、`server/tools/{message_logger,bw_profile,bots}/`。

- [ ] **BW-12-01 Watcher**。复现路径树、类型信息、读/写/调用、远程请求、转发和统计字段。比较原版监控工具的查询、返回值、权限和错误。
- [ ] **BW-12-02 MessageLogger**。移植日志接入协议、组件注册、字段和查询接口、文件日志数据库及条件启用的 MongoDB 后端。用相同事件比较分段、轮转和查询结果。
- [ ] **BW-12-03 Profiler 与故障注入兼容入口**。覆盖原版 profiler 帧和计数器、`_bw_profile` Python 模块及输出、网络模拟接口和可控时间。内部采集与性能分析交付见 BW-23，测试夹具与故障注入框架见 BW-18。
- [ ] **BW-12-04 Bots**。复现配置、批量创建、登录、退出、运动脚本及指标。跑登录潮、稳定移动和集群重启，记录可重复的负载和差分结果。

### BW-13 资源管线、编辑器与导出器

源码 `tools/`、`lib/{asset_pipeline,controls,gizmo,guimanager,guitabs,ual,visual_manipulator}/`。

- [ ] **BW-13-01 资产编译管线**。移植 dependency/discovery/cache、batch/JIT compiler 和转换器注册。逐类验证 space、visual、primitive、texture、effect、BSP、配置输入及增量重跑的产物。
- [ ] **BW-13-02 离线工具**。覆盖 `assetprocessor`、`offline_processor`、`res_packer`、`resourcechecker`、`navgen`。按原版构建入口验证命令行或图形工作流、错误和可被原版读取的产物。导航生成与运行时联验见 BW-10-05/07。
- [ ] **BW-13-03 WorldEditor**。复现地图打开/保存、Chunk/Portal/地形编辑、对象放置、选择和变换、属性编辑、撤销重做、资源浏览和空间发布，用同一地图跑编辑后往返测试。
- [ ] **BW-13-04 ModelEditor 和 ParticleEditor**。覆盖模型、材质、动画检查和保存，粒子创建、预览和序列化。以旧资源编辑、保存，再由原版打开验收。
- [ ] **BW-13-05 DCC 与外部集成**。盘点 Max 动画/模型、Maya 模型导出、`bw_world_machine_import`、`bwlauncher` 和插件系统的实际目标。验证导出语义和文件，单独列宿主及 SDK 的平台矩阵。
- [ ] **BW-13-06 原版编辑器公共库与调试资产**。将 `worldeditor_ui`、`modeleditor_core`、`editor_shared`、`exporter_common`、`tools/common`、`plugin_system` 逐一对应到编辑器、导出器或资源管线。核对 `tools/visual_studio/visualizers` 的类型视图，验证 Rust 调试环境的等价字段展示。

### BW-14 FantasyDemo、SDK 示例与发行

源码 `../fantasydemo/`、`examples/`、`scripts/generate-fantasydemo-assets.py`、仓库根 `scripts/package-daily*` 和发行物。

- [ ] **BW-14-01 Demo 脚本和数据**。复用已跟踪的 Account、Avatar、Guard、Space、ClientOnly、实体定义和 server 配置。比对登录、创建角色、进入场景、移动、AOI、NPC、退出后重登。登记生成资源和完整资源包的来源、版本及可验证路径。
- [ ] **BW-14-02 SDK 和扩展样例**。将 Python simple client、Android/iOS 客户端和 CellApp Controller/Extra 扩展示例纳入 `examples/sdk/` 兼容验证。SDK 实现与游戏引擎适配由 BW-21 交付。
- [ ] **BW-14-03 Web Integration**。盘点 FantasyDemo PHP 页面、BigWorld Web 接口及 RPM 可核实的交付内容，以 Python 实现已确认的登录、角色、物品和拍卖路径，比较会话、数据和错误语义。
- [ ] **BW-14-04 可安装发布物**。为三端分别提供运行时、Python 模块/标准库、配置样例、工具与资源安装布局。比对公开配置和 CLI，测试干净安装、升级保留数据、启动与卸载。

### BW-15 Linux、Windows、macOS 原生支持

新版原生支持三端。暂定首批构建目标为 Linux x86-64、Windows x86-64、macOS arm64。其余架构按平台矩阵列项。核心角色和工具使用同一协议、资源、脚本及数据库契约。平台专属代码集中封装。原版可运行两端提供基准，macOS 等没有同端原版时以可追溯的原版样本和跨平台混跑验收。

- [ ] **BW-15-01 平台接口**。封装事件轮询和唤醒、单调时钟、信号/控制事件、子进程与退出状态、文件锁、内存映射、动态库和系统目录。对照大小写、路径编码、原子替换、IPv4/IPv6 和断网行为。
- [ ] **BW-15-02 Linux**。验证原生服务端全角色、Bots、编辑器/客户端、资产工具及发布包。保留旧 EL7 互通基准，另记录 Boundless 自身的发行版及系统库要求。
- [ ] **BW-15-03 Windows**。验证原生服务端全角色、客户端、编辑器及产物。处理进程停止、DLL/CPython 查找、路径与驱动，并核对原版 Windows 客户端和编辑器样本。
- [ ] **BW-15-04 macOS**。验证原生服务端全角色、客户端、编辑器及产物。验证 CPython 加载、应用资源路径、图形设备行为和退出回收，并与 Linux/Windows 服务端互通。
- [ ] **BW-15-05 三端 CI 和混合部署**。三个原生 runner 分别构建、跑协议/资源/Python/持久化测试和实际进程启动测试。持久化矩阵覆盖 MySQL、PostgreSQL、XML 主存储及 SQLite 二级库，三端 DBApp 均连接真实 PostgreSQL 实例验证登录、写档和重启加载。再组合三端客户端与服务端、替换独立服务进程，核对版本标识、结果和关服顺序。
- [ ] **BW-15-06 平台图形和宿主依赖**。维护渲染后端和 DCC/插件支持表。三端验证等价的客户端与编辑工作流，宿主专属插件在可用宿主测试，并用导出文件检验其他平台消费能力。

### BW-16 目录分层、依赖方向与模块归属

以下为 Boundless 拟议目录。可执行入口、可复用库、Python 脚本、测试夹具和平台适配分别归位。实际 crate 可在实现时随拥有者逐步建，目录名和功能编号对应。

```text
boundless/
  Cargo.toml
  crates/
    foundation/       # BW-01 公共类型、时钟、字节编解码
    platform/         # BW-15 OS 接口及 linux/windows/macos 实现
    resources/        # BW-01 DataSection 和虚拟文件系统
    mercury/          # BW-02 传输、消息帧、RPC
    contracts/        # BW-02/04/05–09 各角色共享的消息与数据契约
    entitydef/        # BW-04 实体定义和数据契约
    python-host/      # BW-03 CPython 宿主、对象桥接
    observability/    # BW-12 Watcher、日志、性能采样
    profiling/        # BW-23 Rust/Python 采集、跟踪会话和产物格式
    diagnostics/      # BW-24/25 内存快照、状态聚合和查询契约
    server-runtime/   # BW-01 进程启动、配置、游戏 tick
    world/
      space/          # BW-10 Chunk、Portal、场景和编译空间
      terrain/        # BW-10 地形读取及查询
      collision/      # BW-10 BSP、射线和扫掠
      navigation/     # BW-10 waypoint、导航查询和资源生成
    services/
      machined/       # BW-05
      reviver/        # BW-05
      baseappmgr/     # BW-05
      cellappmgr/     # BW-05
      dbappmgr/       # BW-05
      loginapp/       # BW-06
      baseapp/        # BW-06/07
      cellapp/        # BW-08
      dbapp/          # BW-09
    storage/
      core/           # BW-09 DBApp 存储契约、通用类型和错误
      mysql/          # BW-09 MySQL 主存储
      postgres/       # BW-09 PostgreSQL 主存储、schema 与迁移
      xml/            # BW-09 XML 主存储
      sqlite/         # BW-09 BaseApp 二级库
    connection/       # BW-06/11 客户端连接、同步和回放
    rendering/        # BW-11 图形接口、渲染资源和后端
    client-runtime/   # BW-11 帧、输入、玩家、UI、音频与脚本
    asset-pipeline/   # BW-13 依赖发现、转换与编译
    editor-runtime/   # BW-13 编辑状态、命令、撤销重做和共享控件
    bots-runtime/     # BW-12/17 无界面 Bot、连接和场景执行
    test-support/    # BW-18 Rust 夹具、虚拟时间、多进程和故障注入
  apps/
    server/           # BW-05–09 各角色独立二进制入口
    client/           # BW-11 图形客户端入口
    editors/          # BW-13 WorldEditor、ModelEditor、ParticleEditor 入口
    tools/            # BW-04/09/12/13 各命令行工具入口
      profile/        # BW-23 启停采集、导出及对比入口
      diagnostics/    # BW-24/25 交互检查与集群状态 CLI
    bots/             # BW-17 独立压测进程与场景 CLI
  python/
    api/              # BW-03 按执行角色装载的兼容模块
    tools/            # BW-12/13/14 Python 工具与 Web Integration
      analytics/      # BW-26 事件校验、查询及报表 CLI
  sdk/
    core/             # BW-21 无引擎依赖的 Rust SDK
    ffi/              # BW-21 原生 ABI 及公开头文件
    python/           # BW-21 Python 客户端 SDK
    engines/
      unity/          # BW-21 C# 封装与 Unity 包
      unreal/         # BW-21 C++ 模块与 Unreal 插件
      godot/          # BW-21 GDExtension 与脚本接口
  examples/
    fantasydemo/      # BW-14 示例实体、配置、脚本和资源
    sdk/              # BW-14/21 各引擎、Python、移动端与扩展示例
  config/             # 默认运行配置和可安装的资源路径配置
  packaging/          # BW-14/15 按 linux、windows、macos 拆分
  docs/
    .vitepress/       # BW-19 配置、主题和导航
    guide/            # 安装、快速开始和部署
    architecture/     # 模块、协议与进程拓扑
    reference/        # 配置、脚本 API、SDK 和 CLI
    operations/       # 运维、数据迁移、排障和压测
    public/           # 文档站公开静态资源
  extensions/
    vscode/           # BW-20 独立 TypeScript 扩展和 VSIX 打包
  integrations/
    lsp/              # BW-27 实体定义与工作区语言服务
    debug/            # BW-27 DAP 集成、进程发现与调试配置
    mcp/
      docs/           # BW-28 版本化索引、文档检索和资源
  tests/
    python/           # BW-18 Python API 与各角色脚本回归
    fixtures/         # 有来源记录的协议、资源、录像和 DB 样本
    parity/           # 原版差分、单组件替换和跨平台互通
    integration/      # 服务拓扑、端到端和发布物启动
    benchmarks/       # Rust 性能基线及第二轮回归
    load/
      scenarios/      # BW-17 Bots 压测场景、负载模型及阈值配置
  ci/
    scripts/          # BW-22 本地和 CI 共用的执行入口
    matrices/         # 平台、数据库、SDK、发布物矩阵
  .github/
    workflows/        # BW-22 CI 触发、runner 与产物编排
```

- [ ] **BW-16-01 Workspace 依赖边界**。服务与客户端运行时依赖共享契约及各自所需的基础库，服务进程间调用走 Mercury 契约。跨角色共用的消息结构按发送方/接收方接口归入 `contracts`，避免进程 crate 互相依赖。依赖方向为 `apps → 角色或工具运行时 → 契约与领域库 → 基础层`。
- [ ] **BW-16-02 平台与外部依赖边界**。OS 调用进 `platform`，图形后端进 `rendering`，数据库驱动进 `storage`，CPython FFI 进 `python-host`。应用入口装配具体实现，通过 workspace 依赖图检查循环及跨层引用。
- [ ] **BW-16-03 Python 布局与资源交付**。实现各角色的模块注册和部署搜索路径，保留 `BigWorld` 等公开导入名称及原版资源定位。用 Demo 原样导入测试检验构建目录和安装目录。
- [ ] **BW-16-04 源码映射与扩展接口**。登记原版构建目标、功能编号、新 crate/模块及测试位置。编辑器和工具复用世界/资源/转换库。明确 Rust 原生扩展与 Python 扩展各自入口，验证加载和错误传播。

### BW-17 Bots 压测

原版入口为 `server/tools/bots/`。BW-12-04 负责原版 Bots 行为对齐，本模块负责场景化压测和结果验收。

- [ ] **BW-17-01 Bot 运行时与控制**。复用客户端连接层，覆盖批量加减、标签分组、Python 行为驱动、beeline 和巡逻路径，以及原版 Watcher 控制接口。验证原版和新版 Bots 均可登录并完成同一移动轨迹。
- [ ] **BW-17-02 场景与执行器**。在 `tests/load/scenarios/` 保存固定种子、账号池、并发量、爬升速率、预热及持续时间。覆盖登录洪峰、稳态移动、密集 AOI、跨 Cell、断线重连和长稳运行，记录负载生成端的资源使用。
- [ ] **BW-17-03 指标和回归**。输出机器可读结果，按场景汇总登录成功率、在线数、延迟分位数、tick 超时、带宽及资源使用。短场景进 PR smoke，容量和长稳场景进入定时或手动任务，结合测得的基线设阈值。

### BW-18 内部测试框架

原版入口为 `lib/unit_test_lib/`、`lib/testing/` 和 [TESTING.md](../BigWorld/TESTING.md)。Rust 测试支持库归 `crates/test-support/`，Python 套件归 `tests/python/`。

- [ ] **BW-18-01 通用夹具和用例映射**。实现资源根目录、临时工作区、固定随机种子、配置与数据库夹具。用 Rust 原生测试和 pytest 复刻模块测试，登记原版用例到新版用例的对应关系。
- [ ] **BW-18-02 多进程测试框架**。对应原版 `multi_proc_test_case`，提供按拓扑启动、按条件等待就绪、独立端口和数据库、日志采集、失败现场保留及进程清理。验证并行执行时隔离和三端清理。
- [ ] **BW-18-03 可控时间与消息故障**。对应原版 `lib/testing/` 消息过滤能力，提供虚拟时钟、手动推进 tick、丢包、延迟、乱序与进程故障注入。验证定时器、重传、迁移和恢复行为可重复。
- [ ] **BW-18-04 本地与 CI 共用入口**。提供按模块执行、全量测试、兼容差分、覆盖率和结果归档入口。文档记录调用、依赖、夹具来源和失败复现方式。

### BW-19 VitePress 文档站

参考原版 `docs/.vitepress/config.mjs`、`docs/zh/` 和仓库根 `package.json`。Boundless 文档站位于 `docs/`。

- [ ] **BW-19-01 站点骨架**。配置导航、侧栏、搜索、静态资源和构建入口。在 CI 校验链接与生产构建，并生成可预览的站点产物。
- [ ] **BW-19-02 用户与开发文档**。编写三端安装、Demo、独立服务启动、Python 实体开发、客户端 SDK 和引擎接入指南。每条快速开始使用干净环境验证。
- [ ] **BW-19-03 架构及运维参考**。维护进程拓扑、协议兼容矩阵、配置和 API、数据库迁移、Bots 压测、内部测试框架和发布指南，链接对应任务及可运行样例。

### BW-20 VS Code 插件

Boundless 新增交付，独立位于 `extensions/vscode/`，内部拆为 `src/`、`syntaxes/`、`snippets/` 和 `test/`。

- [ ] **BW-20-01 工程识别与语言配置**。识别资源根、`entities.xml`、`alias.xml`、实体 `.def` 和 Base/Cell/Client 脚本目录。提供语法、片段和实体定义到脚本的跳转；诊断复用 `process_defs` 或同一 EntityDef 检查入口。
- [ ] **BW-20-02 开发命令和调试**。提供任务入口启动/停止本地集群、查看运行状态、运行选定测试和 Bots 场景、查看进程日志。接入 BW-27 的 LSP/DAP 和 BW-28 的文档 MCP，验证 Python 附加调试与 Rust 调试启动配置。
- [ ] **BW-20-03 打包与三端验证**。提供 VSIX、扩展宿主集成测试及安装说明，在 Linux、Windows、macOS 用 Demo 工作区检查核心流程。源码和发布包在插件目录独立构建。

### BW-21 多引擎客户端 SDK

原版互通基准见 `lib/{connection,connection_model,bwentity}/` 与 `examples/client_integration/`。Unity、Unreal、Godot 作为首批拟支持的引擎，具体支持版本随适配原型验证。

- [ ] **BW-21-01 公共核心 SDK**。复用 Mercury、EntityDef 和 connection，提供登录、断开/重连、实体创建销毁、属性同步、RPC、移动输入及空间事件。用统一契约套件验证原版及新版服务端互通。
- [ ] **BW-21-02 ABI 与 Python SDK**。定义原生句柄、回调线程、内存归属、错误码和版本协商，生成公开头文件。验证加载、断线和销毁。Python 客户端覆盖原版 simple 示例。
- [ ] **BW-21-03 Unity 适配**。实现 C# 封装、主线程事件派发和 Unity 包。示例验证登录、实体生成、属性更新、移动、RPC 与场景退出。
- [ ] **BW-21-04 Unreal 适配**。实现插件、C++/Blueprint 调用接口和游戏线程派发。用同一 SDK 契约和示例验证组件生命周期。
- [ ] **BW-21-05 Godot 适配**。实现 GDExtension、脚本绑定和信号派发。用同一 SDK 契约和示例验证节点生命周期。
- [ ] **BW-21-06 SDK 发布与矩阵**。按引擎、版本、操作系统和架构发布并标注支持情况。对照 Python 与原版 Android/iOS 示例验证协议面。各引擎用同一套服务端、实体定义和行为夹具完成端到端跑通。

### BW-22 CI 工具与发布验证

参考原版仓库的 `.github/workflows/`、`scripts/package-daily*` 和 `scripts/check-docs-links.py`。脚本放在 `ci/`，工作流调用相同入口。

- [ ] **BW-22-01 构建与静态检查入口**。固定工具链与锁文件，提供 Rust 格式、lint、编译、Python 检查和 TypeScript 检查入口。按变更路径选择检查，文档变更触发站点检查。
- [ ] **BW-22-02 测试矩阵与服务装配**。编排三端原生 runner、真实 MySQL/PostgreSQL 后端、XML/SQLite、原版兼容差分和 SDK/插件测试。每套任务可在本地复跑，归档测试结果、覆盖率、服务日志及失败夹具。
- [ ] **BW-22-03 压测与回归报告**。PR 跑短 smoke，定时或手动任务跑容量、稳定性和故障场景。生成不同版本可比较的报告，记录硬件、拓扑、数据库后端及场景版本。
- [ ] **BW-22-04 发布物流水线**。构建并归档三端二进制与安装包、VitePress 站点及匹配的文档索引、SDK 包和 VSIX。校验版本、散列和打包内容，使用干净环境对安装/导入/运行做 smoke。

### BW-23 内置 Profiler 与性能分析

原版锚点为 `lib/cstdmf/profiler.cpp`、`server/tools/bw_profile/bw_profile.cpp` 和 `client/profiler_app.cpp`。`_bw_profile` 是原版 Python 模块，Boundless 另提供独立采集 CLI。

- [ ] **BW-23-01 原生和 Python 采集**。记录 Rust 分段计时、调用次数与线程，采集 Python 调用耗时，并在统一单调时钟下关联进程、tick 和请求。用固定脚本验证嵌套调用、线程退出和异常后的事件闭合。
- [ ] **BW-23-02 有界采集会话**。按进程和类别启停，覆盖 CPU 采样、tick、RPC、网络流量和 DB 等待。实现持续时间、缓冲上限和丢失记录，复测超时、取消、断连及无采集时开销。
- [ ] **BW-23-03 性能产物和比较**。保留原版可核对的 JSON 输出语义，另导出时间线、火焰图及按函数、线程、tick 和消息的聚合。配合 Bots 记录版本、场景和拓扑，复测同负载下 p95/p99、热点与资源消耗差异。

### BW-24 交互式内存诊断

参考 BigWorld 的 `lib/cstdmf/memory_debug.cpp` 与本地 KBEngine 的 [Telnet 控制台实现](../kbengine/kbe/src/lib/server/telnet_handler.cpp)。Boundless 采用类似 Telnet 的交互体验，命令通过本地诊断接口执行。

- [ ] **BW-24-01 内存快照**。汇总各进程 RSS、分配量、分配高水位、实体/缓存对象计数及可用的 Python 对象统计。记录各平台的指标口径和采集成本，区分实时值与累计值。
- [ ] **BW-24-02 交互查询**。支持列出诊断目标、按进程或子系统查询、生成快照、比较两次快照和导出 JSON。默认查询范围为本机且只读，按需授权访问，输出结构化统计。
- [ ] **BW-24-03 增长排查**。用可控分配和释放夹具验证差值、分配热点和回收趋势，配合长稳压测报告快照时间、采集开销和对象增长。

### BW-25 进程和集群运行状态

原版锚点为 Watcher、MachineGuard、`server/loginapp/status_check_watcher.cpp` 与 `server/cellapp/cell_viewer_server.cpp`。

- [ ] **BW-25-01 状态采集**。聚合进程版本、就绪、心跳和负载、玩家与实体数、Space/Cell 分布、AOI、网络与 DB 队列以及故障恢复阶段。标注时间、来源与采集有效期。
- [ ] **BW-25-02 查询与视图**。提供单进程/整集群 CLI、JSON 快照和实时刷新视图，支持按角色、Space、Cell 定位并关联 Watcher、日志和 profile 产物。供 CLI、CI 和 VS Code 复用同一查询契约。
- [ ] **BW-25-03 故障呈现**。验证未就绪、过载、心跳过期、节点掉线、部分查询失败和恢复，区分不可达、陈旧和零值。状态输出用于集群 smoke。

### BW-26 数据分析与报表

性能采集由 BW-23 负责，本模块处理游戏和运营事件数据。先交付事件文件及离线分析链路，数据仓库选型留待实际数据量和查询需求明确。

- [ ] **BW-26-01 事件契约与采集**。为登录、会话、空间移动和业务自定义事件定义版本、事件 ID、时间与来源，提供 Python 上报接口和批量导出。验证重试去重、晚到数据、版本兼容及字段脱敏。
- [ ] **BW-26-02 分析工具**。提供校验、筛选、聚合和报表 CLI，覆盖在线/并发人数、会话时长、登录漏斗、空间热度及自定义事件。以固定数据集验收各指标的计数、时间窗口和导出结果。
- [ ] **BW-26-03 数据作业回归**。验证重复导入、失败续跑、日期切分和大批量输入。将查询示例、指标口径及使用说明纳入 VitePress。

### BW-27 开发工具协议

独立协议服务由 `integrations/` 交付。VS Code 插件按 BW-20 消费这些接口。

- [ ] **BW-27-01 LSP**。为 EntityDef 与资源引用提供诊断、补全、定义跳转、引用查找和悬浮信息。复用 EntityDef 解析和工作区索引，验证修改、重命名、多根工作区及协议请求取消。
- [ ] **BW-27-02 DAP**。复用可用的 Python 与 Rust 调试适配器，提供启动/附加、多进程目标发现和源码映射。以 BaseApp 与 CellApp 用例验收断点、栈帧、变量查看和断开后的进程清理。
- [ ] **BW-27-03 工具互通**。为运行状态、进程选择、测试和压测交付稳定 CLI 与机器可读输出。用 VS Code 和独立协议测试客户端验证错误传递、三端路径、版本兼容和长任务取消。

### BW-28 文档知识库 MCP

文档来源为 BW-19 VitePress 的 Markdown、公开 API 参考和已登记的示例。MCP 服务位于 `integrations/mcp/docs/`。

- [ ] **BW-28-01 版本化文档索引**。按 Git 版本、语言、文档路径及标题锚点建立索引，从 VitePress 公开源生成。验证章节拆分、增量重建、删除和版本切换。
- [ ] **BW-28-02 MCP 文档查询**。提供文档搜索、按版本读取和链接解析，返回来源、路径、章节和版本。首发支持本地 stdio，由 VS Code 或其他 MCP 客户端接入。
- [ ] **BW-28-03 检索和发布验收**。用实体属性、Cell 迁移、PostgreSQL 运维等真实问题验证命中章节及链接。CI 在索引过期、引用失效或内容超出公开文档范围时报告问题。

### BW-29 KBEngine 差异和可选能力复核

按前面的 KBEngine 清单逐项记录「已有等价、列入后续实现、暂缓」及依据。确认列入后给每个新增能力分配实施编号和验收样例。

- [ ] **BW-29-01 地图导航差异**。验证 KBEngine `navigation/` 中的 Recast/Detour、平铺数据及 CellApp 异步加载是否在当前构建与脚本入口可达。用跳点、跨 tile、不可达及异步加载期的地图夹具与 BW-10 对照，记录旧 KBEngine 导航资源导入是否进入范围。
- [ ] **BW-29-02 运维工具差异**。按 `kbcmd`、GUIConsole、WebConsole、pycluster 分别核对进程发现、日志、状态、profile 和集群管理，记录对应 BW-23~27 或可选独立控制台需求。
- [ ] **BW-29-03 存储与外部服务差异**。分别复核 Redis 后端、`interfaces` 外部服务、订单/回调和邮件辅助的适用场景及启用条件。若纳入，逐项定义兼容性和端到端验收。
- [ ] **BW-29-04 SDK 和 Python 开发差异**。分别复核在线 SDK 下载、JavaScript 模板、Python assets 模板、类型存根。比对 BW-21/27 已覆盖工作流后登记剩余项。
- [ ] **BW-29-05 工作流差异**。分别复核 `xlsx2py`、安装脚本和新工程脚手架。确认是否纳入，并为选中的功能登记样例输入、输出与平台。

## 4. 第二轮 · 保持兼容的优化

每项从同一版本的第一轮验收结果起步。使用相同拓扑、资源和脚本前后对比，报告 p50/p95/p99 tick 与 RPC 时延、稳态吞吐、CPU、RSS、带宽、数据库负载或对应工具耗时。基准环境、目标与性能阈值由 BW-00 实测确定。对外行为变更须另行版本化，并在混部与回滚用例中验证。

- [ ] **OPT-01-01** 优化时钟、队列、内存分配和资源缓存。比较 tick 尾延迟、分配次数和加载峰值，复跑时序与缓存失效用例。
- [ ] **OPT-02-01** 评估缓冲复用、批量收发与减少复制。比较 RPC RTT、网络吞吐、丢包恢复和内存占用，混跑旧线协议。
- [ ] **OPT-03-01** 减少 Python/native 转换和跨语言回调开销，衡量 GIL 占用与回调耗时，复跑引用生命周期、异常及热重载。
- [ ] **OPT-04-01** 缓存解析结果并评估预生成编解码器，衡量启动和序列化开销，锁定定义摘要和产出字节。
- [ ] **OPT-05-01** 用热点和故障场景测量负载上报、切分/合并频率与恢复窗口，再调优调度策略。检查实体迁移、关服和混部稳定性。
- [ ] **OPT-06-01** 降低登录排队、会话接续和资源下载压力。比较登录峰值、拒绝率和尾延迟，复跑重复登录、重连与兼容回放。
- [ ] **OPT-07-01** 优化备份序列化、归档和共享数据传播。比较带宽、队列峰值、恢复时长及恢复后的实体状态。
- [ ] **OPT-08-01** 基于实测评估 AOI 索引、Witness 调度及安全的空间并行，衡量密集区 tick、广播量和跨边界迁移时长。复核可见集和事件顺序。
- [ ] **OPT-09-01** 优化连接池、查询合并、写入批次和二级库整合。MySQL 与 PostgreSQL 各自使用同一数据集比较优化前后的时延和吞吐，复跑事务、重试与恢复。
- [ ] **OPT-10-01** 优化空间流式装载、碰撞走查和导航缓存。比较峰值内存、场景冷启动和查询尾延迟，核对坐标与碰撞结果。
- [ ] **OPT-11-01** 优化各端 GPU 上传、渲染批次、资源流与客户端帧调度。比较帧时间和显存，用固定镜头做视觉回归。
- [ ] **OPT-12-01** 降低日志、Watcher、Profiler 的观测成本。验证背压、关闭观测后的开销及新旧工具查询兼容。
- [ ] **OPT-13-01** 优化依赖缓存、并行转换和编辑器加载。比较冷构建、增量构建与大地图编辑时间，校验可复现产物。
- [ ] **OPT-14-01** 优化发布包和 Demo 首次启动，衡量体积和安装耗时，复跑干净安装、升级及脚本导入。
- [ ] **OPT-15-01** 按操作系统测量 I/O、定时精度和图形后端。使用同一协议与功能套件复测各端行为。
- [ ] **OPT-16-01** 根据第一轮依赖图调整过重或高度耦合的 crate，比较增量编译耗时，保留接口和安装路径兼容。
- [ ] **OPT-23-01** 比较长稳压测中开启与关闭采集时的 CPU、内存、tick 尾延迟和丢失记录，优化采集和导出开销。
- [ ] **OPT-26-01** 以相同事件回放优化批处理和查询，比较导入速率、查询时延及内存占用，核对报表一致性。

## 5. 实施顺序与里程碑

| 编号 | 交付 | 验收 |
| --- | --- | --- |
| M1 | BW-00/16/18 基础、BW-15-01 平台接口、BW-19 VitePress 骨架、BW-22 CI 入口，推进 BW-01/02/03/04 | 原版基线和源到目标映射可追溯。三端持续构建，资源、协议、脚本 API 和实体定义可差分。 |
| M2 | BW-05 基础发现与注册、BW-06/07/09 最小闭环、BW-08 最小 Space、BW-17 Bots smoke、BW-21 SDK 核心 | 原版 Python 示例和新版无界面客户端均可登录、创建或加载角色、进入世界、写存档和重登。先用 MySQL 建立原版差分基准，再让新版 DBApp 在 PostgreSQL 上通过同一闭环。 |
| M3 | BW-05/07/08 完整拓扑，推进 BW-10/12/17/18、BW-22 压测回归与 BW-23/24/25 诊断 | 两个 BaseApp 和两个 CellApp 的集群可跑 AOI、切分、迁移、恢复及受控关闭。Bots 长稳运行可同时采集 profile、内存快照和集群状态。 |
| M4 | BW-10/11/13/14 产品交付、BW-09-06/07 PostgreSQL 和 BW-15 三端验收、BW-19/20/21/22/26/27/28 工具链发布 | 同一脚本和资源可在三端部署，PostgreSQL 通过矩阵和迁移测试。数据报表可复算，文档站、VSIX、SDK、LSP/DAP 集成和匹配版本的 MCP 索引有端到端验证记录。 |
| M5 | 第二轮 OPT 项 | 对每项记录功能验收和优化前后的同口径测量，并复跑变更触及的兼容用例。 |

任务结果登记字段为「任务编号、原版构建目标/源码、Boundless 模块、平台、输入夹具、原版结果、新版结果、证据链接、状态」。仅具备静态证据或存在平台/依赖缺口的项目持续记录，等待实际可执行结果。
