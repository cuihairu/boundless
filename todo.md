# Boundless 的 BigWorld 功能移植 TODO

参考仓库为 [../BigWorld](../BigWorld/README.zh.md)，源码基准为 `4657a17cf10d55e2fbebf258f96da96a3dbe0c22`。清单来自该提交的目录、构建入口、源码锚点及仓库文档。本次完成静态盘点，所有勾选项均为待办，构建、混跑和性能数据留待各项验收。上游 README 记录的服务端技术基准为 Linux x86-64、C++23、嵌入 CPython 3.13.16。客户端与编辑器需单独复测历史 Windows 构建。

## 1. BigWorld 功能对照清单

基准为 [BigWorld 源码导读](../BigWorld/docs/zh/analysis/source-code-guide.md) 和本地提交的构建清单、源码目录。功能索引指向后文对应的逐项验收清单。

### 运行时与分布式服务

- 基础类型、数学、二进制流、压缩、时钟、容器和后台队列。BW-01。
- DataSection、XML/Binary/Packed/ZIP、资源搜索、缓存和配置继承。BW-01。
- Mercury 事件循环、可靠 UDP、TCP、Packet/Bundle/Channel、RPC、包过滤和加密互通。BW-02。
- CPython 嵌入、原生对象与 Python 桥接、数学/资源/网络/定时器 API 和接通的脚本重载。BW-03。
- EntityDef、类型/属性/方法、Mailbox 契约、实体定义摘要及 `process_defs`。BW-04。
- Machined/机器发现、进程注册、出生和死亡通知、服务探活与 Reviver。BW-05-01/02。
- BaseAppMgr、CellAppMgr、DBAppMgr 的工作进程管理、负载、Space 切分和集群关闭。BW-05-03~06。
- LoginApp 版本与认证、账号登录和重登、Proxy 会话及客户端连接。BW-06。
- Base 实体、Cell 配对、Service 实例、共享数据、全局实体和跨进程 Mailbox。BW-07-01~03。
- Base 备份接管、归档、下载与录像写入。BW-07-04/05。
- Cell/Space、Real 实体、Controller/Extra、移动、传送、乘客和感知。BW-08-01/02/06。
- AOI/Witness、Ghost 复制、消息缓冲、Offload 迁移及 Cell 故障恢复。BW-08-03~05。
- DBApp 实体读写、账号与 Billing、MySQL/XML、SQLite 二级库、整理、快照和同步工具。BW-09-01~05。

### 地图、导航与客户端

- Chunk 室内外流式装载、Portal、空间映射、场景查询、compiled space。BW-10-01/02。
- Terrain2 高度、地形洞、材质、BSP、射线、扫掠和碰撞。BW-10-03。
- Waypoint 集、NavLoc、角色体积 girth、Portal 限制、跨 Chunk 路径和缓存。BW-10-04。
- Waypoint flood、Recast 生成、NavGen 图形入口、导航数据输出与重载。BW-10-05。
- CellApp 寻路和移动控制器、Python 寻路接口、寻路成功/失败回调和端到端地图往返。BW-10-06/07。
- 客户端窗口、输入、相机、玩家、运动滤波、客户端实体、UDO、回放和下载。BW-11-01~03。
- Moo 渲染、模型、动画、材质、特效、地形、植被、粒子、后处理、GUI、声音、VOIP、网页和条件 SDK。BW-11-04/05。

### 工具、观测与示例

- Watcher、MessageLogger、Profiler/`_bw_profile`、状态查询与 Cell Viewer。BW-12、BW-23、BW-25。
- Bots 客户端、脚本控制和巡逻，单测夹具、消息过滤、多进程用例及原版差分。BW-12-04、BW-17/18。
- 资源转换与增量编译、资源检查、打包、导航生成和离线处理。BW-13-01/02。
- WorldEditor、ModelEditor、ParticleEditor、DCC 导出、插件、启动器和共享编辑代码。BW-13-03~06。
- FantasyDemo、Python/Android/iOS 客户端示例、扩展样例、Web Integration、打包与 VitePress 文档。BW-14、BW-19。

## 2. KBEngine 功能对照清单

参考本地 [KBEngine](../kbengine/README.zh.md) 提交 `55a6655203e75d1019c9a9ddbdefb38f1b79260c`。这里逐项记录取舍。**纳入**表示 Boundless 有对应实施任务，以 BigWorld 兼容要求或明确的新增设计为准。**复核**表示需先确认是否纳入，复核决定由 BW-29 的执行项记录。

| 编号 | 本地源码入口 |
| --- | --- |
| KB 源码 | `../kbengine/kbe/src/` 下的 `lib/`、`server/` 及 `server/tools/` |
| KB 配套 | `../kbengine/kbe/tools/`、`kbe/res/sdk_templates/`、`typings/`、`tests/` 和 `docs/` |

- **KB-01 纳入** · Machine 与组件发现、注册、进程状态、心跳和关闭。对应 BW-05、BW-25。
- **KB-02 纳入** · BaseApp/BaseAppMgr 登录分配、代理、实体工厂、跨进程引用和全局数据。对应 BW-05~07。
- **KB-03 纳入** · CellApp/CellAppMgr、Space、实体同步、AOI、跨进程迁移和控制器。对应 BW-05、BW-08。
- **KB-04 纳入** · LoginApp/DBMgr 的账号与实体读写、重复登录及 MySQL。对应 BW-06、BW-09。
- **KB-05 纳入** · 资源配置、网络消息、EntityDef、Python 脚本、线程任务和定时器。对应 BW-01~04。
- **KB-06 纳入** · 地图资源、寻路、地图异步加载与路径结果。BigWorld 行为见 BW-10；对比 KBEngine `navigation_mesh_handle`、`navigation_tile_handle` 与 `loadnavmesh_threadtasks` 的具体差异见 BW-29-01。
- **KB-07 纳入** · Watcher、组件状态、日志和网络、原生、Python profile。对应 BW-12、BW-23、BW-25。
- **KB-08 纳入** · Telnet 式交互诊断和内存查看体验。对应 BW-24，使用 Boundless 本地只读接口。
- **KB-09 纳入** · Bots 与内部测试、CI 构建、VitePress 文档和跨平台发行。对应 BW-15、BW-17~19、BW-22。
- **KB-10 纳入** · SDK 模板与 Unity/Unreal 客户端接入。Boundless 的独立 SDK 和多引擎示例见 BW-21。
- **KB-11 纳入** · `kbcmd`、GUIConsole、WebConsole、pycluster 的状态查询、日志和运维工作流。Boundless 已计划部分见 BW-24/25/27，额外工作流的取舍见 BW-29-02。
- **KB-12 复核** · Redis 数据库实现、Interfaces 对接及订单/回调、邮件辅助。对应 BW-29-03。
- **KB-13 复核** · LoginApp 的 `clientsdk_downloader`、JavaScript SDK 模板、Python assets 模板与类型存根。对应 BW-29-04。
- **KB-14 复核** · `xlsx2py` 表格转换、安装脚本和新建 assets 工程脚手架。对应 BW-29-05。

## 3. Boundless 实施清单

### 编号和实施口径

只有四段式编号（例如 `BW-10-04-03`、`OPT-10-01-02`）是独立执行项。每项通过该行验收并登记证据后勾选。三段式编号是任务分组，用于定位和引用。

- `BW-模块-任务` 按功能编号。第一轮逐项对齐原版，并标明 Boundless 新增的交付能力。第二轮用 `OPT-模块-任务` 对应同一功能域。执行顺序见文末里程碑。
- 第一轮由 Rust 承载原生引擎、协议、进程、存储和客户端运行时，由 Python 承载原版脚本 API、游戏逻辑及适合的工具。先沿用参考版本的 CPython 3.13 行为。嵌入和原生扩展的具体实现待最小原型验证。
- 一比一以外部可观察行为验收。保留适用的进程边界、消息编号及线格式、配置默认值、数据格式、脚本 API 与回调次序、状态转换和错误语义。Rust 内部用等价实现表达 C++ 宏、对象生命周期和内存管理。平台、编译宏与依赖版本进入验收记录。
- 每项先定位构建目标及调用方，采集原版结果，再实现、比对并留存证据。对时间、随机值、地址等易变字段固定输入或显式归一化。遇到原版不可运行、未启用或缺资源的能力，记录原因和可获得的证据，保留待验状态。
- 下文源码路径默认相对于 `../BigWorld/programming/bigworld/`。版本以 Git 跟踪内容为准。工作区中的 `game/`、RPM 和完整资源下载可作为补充样本，使用前分别记录版本和来源。

### BW-00 基准、构建与兼容性测试

源码和资料 `programming/Makefile`、`build/`、`cmake/`、`vcpkg.json`、[TESTING.md](../BigWorld/TESTING.md)、[源码导读](../BigWorld/docs/zh/analysis/source-code-guide.md)。

#### BW-00-01 构建目标清册

展开 Make/CMake、发布脚本及条件编译，列出可执行文件、库、扩展和平台矩阵。逐项标明已参编、条件启用、仅源码、仅发行物或缺依赖。整理 `third_party/`、资源许可证和 NOTICE，确认哪些复用依赖、哪些需替代实现。

- [ ] **BW-00-01-01 展开 Linux Make 构建目标**。**验收** 清册包含二进制、库及所属规则。
- [ ] **BW-00-01-02 展开 CMake 与 Windows 目标**。**验收** 清册记录平台及启用宏。
- [ ] **BW-00-01-03 登记发行脚本与 RPM 目标**。**验收** 标出独有交付物及版本。
- [ ] **BW-00-01-04 登记第三方依赖与资源来源**。**验收** 每项有许可证、NOTICE 及复用决策。

#### BW-00-02 原版运行基准

记录二进制哈希、编译选项、配置、Python 标准库和数据库版本。复跑上游单测入口并记录实际参编用例。获取单机集群、最小客户端、Windows 客户端及工具的可运行基准。

- [ ] **BW-00-02-01 采集原版运行环境**。**验收** 记录提交、二进制哈希、构建参数、Python 和 DB 版本。
- [ ] **BW-00-02-02 复跑原版单测**。**验收** 保存参编列表、执行命令及结果。
- [ ] **BW-00-02-03 录制单机集群启动基线**。**验收** 留存拓扑、配置和日志。
- [ ] **BW-00-02-04 录制最小客户端登录**。**验收** 留存请求和回调序列。
- [ ] **BW-00-02-05 复测 Windows 客户端和工具基线**。**验收** 逐目标保存结果或缺口。

#### BW-00-03 Boundless 构建入口

建立 Cargo workspace、Python 模块及启动入口，固定工具链和依赖。为本地资源、配置、原版二进制和测试夹具设独立路径。验收为干净检出可构建、启动、关闭并保留日志。

- [x] **BW-00-03-01 创建 Cargo workspace**。**验收** 干净检出可枚举和编译成员。
  - 证据：`cargo build --workspace` 9 个成员 0 警告（根 Cargo.toml）。
- [ ] **BW-00-03-02 固定 Rust、Python 工具链与依赖**。**验收** 本地与 CI 解析同一版本。
- [ ] **BW-00-03-03 配置 Python 模块构建与导入**。**验收** 安装后能导入最小模块。
- [ ] **BW-00-03-04 分离资源、配置、原版产物和测试路径**。**验收** 同机两套环境互不覆盖。
- [x] **BW-00-03-05 提供最小启动与关闭入口**。**验收** 进程退出且留存启动日志。
  - 证据：`boundless-server loginapp` 退出码 0 并打印 `[loginapp] starting`，未知角色退出码 1（apps/server）。

#### BW-00-04 差分验收工具

建立二进制帧、Python 回调序列、资源产物、实体快照及数据库行的基准样本。支持原版全栈、新版全栈和替换单个组件三种模式。按兼容边界报告字段差异、超时及回调顺序差异。

- [ ] **BW-00-04-01 采集协议二进制夹具**。**验收** 记录消息 ID、方向及原始字节。
- [ ] **BW-00-04-02 采集 Python 回调夹具**。**验收** 保留顺序、参数与可忽略字段。
- [ ] **BW-00-04-03 采集资源、实体和数据库夹具**。**验收** 每份可追溯原版版本与输入。
- [ ] **BW-00-04-04 实现原版与新版各自全栈的对照运行**。**验收** 同一输入产生两份证据。
- [ ] **BW-00-04-05 实现单组件替换运行**。**验收** 报告注明被替换角色。
- [ ] **BW-00-04-06 实现差异报告**。**验收** 注入字段、超时和顺序差异时分别可定位。

#### BW-00-05 功能覆盖台账

逐项核对 `lib/`、`server/`、`server/tools/`、`client/`、`tools/`、`common/`、`examples/` 和 FantasyDemo 的构建目标与用户功能。记录「原版路径、启用条件、功能编号、验证夹具、覆盖状态」。对第三方 SDK、发行物中的独有内容和缺失资源单独记录来源与验证缺口。每个已启用目标都要有可运行验收或可追踪的阻塞原因。

- [ ] **BW-00-05-01 建立覆盖台账字段**。**验收** 每行含原版目标、启用条件、任务号、夹具及状态。
- [ ] **BW-00-05-02 映射引擎库和服务端目标**。**验收** 构建清册中的每项都有索引。
- [ ] **BW-00-05-03 映射客户端和工具目标**。**验收** 编辑器、导出器及离线工具均有索引。
- [ ] **BW-00-05-04 映射公共代码、示例与 FantasyDemo**。**验收** 脚本及资源入口可追溯。
- [ ] **BW-00-05-05 核查发行物、SDK 和缺失资源**。**验收** 缺口有来源与阻塞原因。
- [ ] **BW-00-05-06 复核台账完整性**。**验收** 每个启用目标有运行证据或显式缺口。

### BW-01 基础运行时、配置和资源

源码 `lib/cstdmf/`、`lib/math/`、`lib/resmgr/`、`lib/server/bwservice.hpp`、`lib/server/server_app.cpp`、`lib/build/`、`lib/memhook/`。

#### BW-01-01 公共类型和数学

移植 ID、时间戳、字符串与容器等被外部使用的语义，以及向量、矩阵、四元数、包围盒、坐标变换。比对轴向、数值边界、精度和二进制布局。

- [x] **BW-01-01-01 定义实体及空间 ID 的宽度与哨兵值**。**验收** 边界字节序列与原版一致。
  - 证据：crates/foundation ids.rs 测试钉死宽度、哨兵与字节序。
- [x] **BW-01-01-02 移植时间戳转换**。**验收** 单位、截断与回绕夹具一致。
  - 证据：crates/foundation time.rs 测试（from_micros 往返、saturating_duration_since）。
- [x] **BW-01-01-03 验证字符串和容器的外部语义**。**验收** 空值和顺序夹具一致。
  - 证据：crates/foundation stream.rs 测试（LE 定宽读写、容量追踪、空值）。
- [x] **BW-01-01-04 实现向量、矩阵和四元数运算**。**验收** 固定变换中的轴向和精度吻合。
  - 证据：math/{vector,matrix,quaternion}.rs 测试（点积累加序、+90° yaw 轴向、slerp 短弧）。
- [x] **BW-01-01-05 实现包围盒与空间坐标转换**。**验收** 边界点判定及布局吻合。
  - 证据：crates/foundation math/boundbox.rs 测试，行覆盖 100%。

#### BW-01-02 流与基础编解码

复现 BinaryStream、定长和变长整数、字符串、内存流、压缩流及读越界报告。以原版生成的样本双向读写。

- [ ] **BW-01-02-01 实现定长整数和浮点读写**。**验收** 原版夹具逐字节往返。
- [ ] **BW-01-02-02 实现变长整数与长度前缀**。**验收** 跨长度边界值逐字节吻合。
- [ ] **BW-01-02-03 实现字符串与内存流**。**验收** 空值、嵌入零和剩余长度吻合。
- [ ] **BW-01-02-04 实现压缩流**。**验收** 双方可解码同一夹具。
- [ ] **BW-01-02-05 实现截断和越界报告**。**验收** 异常输入产生同类流错误。

#### BW-01-03 DataSection 和资源路径

覆盖 XML/Binary/Packed/ZIP、目录与文件系统搜索顺序、重复节点索引、路径映射、缓存、更新和保存语义。比对同名覆盖、空节点及循环读写产物。

- [x] **BW-01-03-01 实现 XML DataSection**。**验收** 重复子节点和空值查询与原版一致。
  - 证据：crates/resources datasection.rs 80 测试钉死 xml_section.cpp 语义（重复子节点取首、空值返回空串、转义表、保存布局）；llvm-cov 全部核心 crate 行覆盖 100%。
- [x] **BW-01-03-02 实现 Binary/Packed 读写**。**验收** 原版生成物被正确解析且能往返。
  - 证据：packed.rs 手搭字节级夹具复现原版生成物（magic 0x62a14e45、排序字符串表双 NUL、错位一格记录布局 R0.dataPos=own end|type、child k 的 end|type 存 R(k+1)）并解析通过；写入→读取全树往返（int 全宽/bool/float/vector/矩阵 row0-3/加密 blob/base64）字节精确；120 测试全绿。原版为 Windows 工程不可在 Linux 运行，以 packed_section.cpp 逐行精读钉死语义（含 addBestGuess 分支顺序 empty→bool→大u64→int→float→blob→string）。llvm-cov：packed.rs 99.84%（仅 3 行为原版自身 asString↔asInt64 无限递归分支，注释留档），base64/binary/special_chars/datasection 100%。
- [x] **BW-01-03-03 实现 ZIP 资源挂载**。**验收** 压缩和未压缩条目可查。
  - 证据：zip.rs 手搭 ZIP 字节夹具（本地头/中央目录/尾记录逐字节自拼）验证解析，STORE 与 DEFLATE 条目均可 read_file 查询；get_file_type_ex 嵌套 zip 探测（未加载流式嗅探压缩与未压缩、已载入内存字节嗅探）；updateFile 重名编码（"name 1"/"name 2"）与 duplicates 解析、erase 索引重建、clear、createZip 字节精确对照（固定 DOS 时间戳）、bCompressed_ 锁存防二次压缩回归。29 测试全绿。zip_file_system.cpp 3010 行逐行精读钉死语义，忠实保留原版怪癖：根目录无 fileMap 行（getFileType("") 返回 NOT_FOUND）、尾点路径 erase(size-2) 连切两字符、readFile 不剥前缀而 getFileType 剥、updateFile 行需 createZip 后才可读（localHeaderOffset<0 早退）、createZip 的 isValid=pData_!=NULL 剔除未加载行、坏本地头签名退化到零头从 offset+30 读入名字字节。llvm-cov：zip.rs 行 99.38%/区域 100%（仅余防御分支与 unwrap panic 区域），workspace 0 警告。嵌套 zip 节点（childNode/tag_ 转发）与 ZipSection 数据节层属后续集成任务。

- [x] **BW-01-03-04 实现资源根搜索和路径映射**。**验收** 同名资源优先级吻合。
  - 证据：file_system.rs/multi_file_system.rs/bwresource.rs 移植 unix_file_system.cpp + multi_file_system.cpp + bwresource.cpp。同名资源优先级吻合已钉死：MultiFileSystem 按加入顺序 first-hit-wins（getFileType/getFileTypeEx/readFile/readFile(dirPath,index)），delPath+refresh 后落到第二根、addPath(atIndex=0) 前插恢复优先级；readDirectory 为跨根 union（同名列出两次）且 dirCache_ 键剥一个尾斜杠、写后 flushModificationMonitor 才失效。postAddPaths 路径映射：*.zip 与 dir/res.zip → ZipArchiveFs（包目录本体不再挂载，包内文件遮蔽包外同名文件）、目录 → NativeFileSystem、损坏 zip 丢弃。resolveToAbsolutePath 跳过 zip（空绝对路径）、目录补尾斜杠、空路径走原生分支。searchPathsForFile/searchForFile 递归重写全路径（依赖 EDITOR_ENABLED 空基根，enable_editor_default_root 对应移植）。忠实保留原版怪癖：pathIsRelative("")=false、getFilePath=glibc dirname（"top.txt"→"."，根写只标记 "." 缓存槽、根列表缓存不失效）、".hidden" 扩展名为 "hidden"、formatSearchPath 剥一个尾斜杠、ensurePathExists 无尾斜杠路径丢最后一级、zip 会话内写行 flush 前 offset<0 不可读、flush 后 isValid 剔除未加载改名行。32 新测试全绿（workspace 291 测试、0 警告）。llvm-cov：bwresource.rs 99.25%、file_system.rs 98.97%、multi_file_system.rs 99.74%、crate 总 99.63%（余为花括号/测试夹具区域伪迹）。
- [x] **BW-01-03-05 实现 DataSection 缓存失效**。**验收** 覆盖写入后读取新值。
  - 证据：data_section_cache.rs 逐行对照 data_section_cache.cpp 移植 LRU 语义：add 跳过 0 字节/超预算条目、插入前 purgeLRU 腾位、已存在条目原位换 payload+bytes 不动链位、find 命中移 head 并计 hits/misses、clear 连统计一并清零、默认预算 DEFAULT_CACHE_SIZE=100KiB、unlinkNode 按位置分派（仅节点/head/tail/中段，中段必有两邻接直接重连，字节结算留在 remove/purgeLRU 以保 moveToHead 会计不变——首版在此处有 bug，由字节会计断言钉死）。BwResource 集成：open_section 按 DirSection::findChild+createAppropriateSection 语义（缓存优先、内容嗅探 '<' 判 XML 而非看扩展名、无效 XML 不缓存、BinSection/ZipSection/DirSection 分支留后续任务、makeNewSection 点名标签建空节并入缓存、无点标签属目录分支）、refresh_paths 复现 postAddPaths 的 BW_CACHE_SIZE getenv+atoi 语义（前导空白/符号/尾随垃圾、负值等效 0、缺失用默认）+setSize+clear、purge(recursive) 先递归子路径再删本路径（含空根路径全树遍历）、purge_all 连统计清零。验收流程钉死：open(miss 读盘)→覆写文件→open 仍返回旧 Arc（ptr_eq 证明缓存存在）→purge→open 返回新值；递归 purge 子树全部换新、未涉及文件保持缓存命中。16 新测试全绿（workspace 304 测试、0 警告）。llvm-cov：data_section_cache.rs 100%、bwresource.rs 99.32%（余为花括号伪迹与不可达 degenerate 分支）、resources crate 总 99.64%。
- [x] **BW-01-03-06 实现保存与更新**。**验收** 原版可读新版生成的代表性文件。
  - 证据：save_section/save_section_if_exists 复现 DirSection::saveChild + XMLSection::save/saveIfExists 流：序列化（write_to_string 已在 BW-01-03-01 与 writeToStream 逐字节对齐：属性行 level+2 缩进、叶值内联带尾 tab、空节点折叠 `<tag />`）→ MultiFileSystem::write_file/write_file_if_exists（writeFile 逐基尝试首个成功者并标记修改目录；Exists 变体逐基 get_file_type==File 定位后写，故双根持有同名文件时写第一根、仅次根持有时落到次根——与 writeFile 落空穿透语义分开钉死）→ 成功后新树回填缓存（原版靠 census 持有同一对象使缓存自然更新，移植版显式重加，等价 findChild 的 census-rejoin add）。验收「原版可读」：生成代表性文件（属性行、嵌套子节点、重复子节点、空节点、叶值）落盘字节与原版 writeToStream 布局逐字节一致，重解析（已钉死的 createFromBinary 语义）还原同树；save→update 流程钉死：改树重存后 open_section 即服务新树、purge 后从盘读回同值；失败路径（所有根都无法创建父目录/无根持有）不动缓存。isBinary 为 Windows 文本模式无 unix 等价、save-as（splitSaveAsFileName）与 census 记账属 DirSection/census 后续任务。3 新测试全绿（workspace 307 测试、0 警告）。llvm-cov：bwresource.rs 99.40%、multi_file_system.rs 99.75%、crate 总 99.63%（余为花括号伪迹/防御分支）。

#### BW-01-04 进程生命周期与配置

覆盖入口参数、`BWResource`、`BWConfig`、`bw.xml` 配置继承和覆盖、初始化、信号、退出码、分阶段关服。比对配置信息缺失、启动失败和正常关闭。

- [x] **BW-01-04-01 解析启动参数和资源根**。**验收** 同一命令得到相同配置路径。
  - 证据：init_from_args/init_from_sources 复现 BWResource::init + loadPathsXML 全链。argv 扫描与 bwParseCommandLine 对齐：`--res`/`-r` 取下一参数加入临时路径表、`-UID`/`-uid` 改写生效 uid（原版 setenv("UID")，移植版以 home_of_uid 闭包注入——`-uid 1001` 真正驱动 passwd/machined 查找；`-uid 2000` 查无 home 即失败，钉死旗标值驱动分支），循环上界 `i < argc-1` 与原版一致（末尾悬空旗标永不匹配、原样进剩余参数，compressArgs 等价收缩）。回退链与 loadPathsXML 相同：argv 路径 → BW_RES_PATH 环境（ ':' 分隔）→ ~/.bwmachined.conf 首个可用行（sscanf("%*[^;];%s") 语义：分号前段全弃、空头或空 token 不取；while(paths_.empty()&&fgets) 首行添入即停的门钉死——多行文件只用第一行）。addPaths 对齐：':' 切分、相对路径锚定 exe 目录后 canonicalize（fs::canonicalize）、不存在即弃。验收「同一命令得到相同配置路径」：同参数两次 init 产生逐项相同的规范化绝对路径列表（zip/目录根排序稳定）；坏 zip 被丢弃、坏 res.zip 目录根归零后重建迟到 add 仍成功。env 依赖经 Option 注入测试（cache_size_from/current_uid_from/init_from_sources，edition 2024 不安全 set_var 的绕法），真实环境薄包装 init_from_args/refresh_paths 保持生产路径被覆盖。6 新测试全绿（crate 205、workspace 314、0 警告）。llvm-cov：bwresource.rs 99.58%、crate 总 99.6%+（余为花括号伪迹）。
- [x] **BW-01-04-02 解析 bw.xml 继承与覆盖**。**验收** 并列配置的最终值吻合。
  - 证据：`server-runtime/src/config.rs` 移植 BWConfig（bwconfig.cpp）全语义。继承链与 init() 逐步对齐：个性化文件 `server/bw_<username>.xml`（getpwuid(getUserId())->pw_name，`-uid` 改写生效 uid 故由 resources 的 current_uid 驱动；/etc/passwd 无此 uid 则 None）起步，缺文件静默回退 `server/bw.xml`（isUserFilename 恰好消耗一环后清零）；`<parentFile>` 逐环推进（读经 DataSection::readString，XMLSection::asString 无视 default 故空值与缺失同样终止链）；打不开的环区分「存在但解析不了」（Could not parse）与「不存在」（Could not find），二者都记 badFilename、链止、isBad() 复开坏文件并报不可用。覆盖规则与 getSection 一致：链自前向后首中即得——子文件逐键遮蔽父文件、父文件只补子文件缺的键（验收「并列配置的最终值吻合」：三文件链 user→two→base 逐键断言最终值），SearchIterator（beginSearchSections）以链序枚举各环同名重复段并标注来源文件名，getChildrenNames 链序拼接。覆盖（+参数）与 processCommandLine 一致：`+key value` 成对写入链首段（writeString=openSection(makeNew)+setString，深路径中间层现建），`i<argc-1` 上界使悬空尾 `+key` 被忽略；无段时命令行失败（No configuration section，BW-01-04-03 退出码的输入）。g_inited 守卫：已初始化的二次 init 保留旧链只重放命令行。Rust Arc 不可变树的处理：改写克隆后经新增的 BwResource::readd_section（re-add 缓存、按序列化大小记账、不落盘）回填链首与缓存——复现 C++ 共享对象改写对 BWResource::openSection 的天然可见性（save_section 同款模式）。支撑：DataSection 新增 openSection/readString/writeString 三件套（空路径即自身、尾斜杠归斜杠前子段、连续斜杠 find_first_not_of 折叠、缺失中间层 makeNew 现建、空标签值不被 default 吞掉）。18 新测试全绿（resources 209、server-runtime 26、workspace 340、0 警告）。llvm-cov：config.rs 99.59%、datasection.rs 99.93%、bwresource.rs 99.58%（余为花括号伪迹/防御分支）。
- [x] **BW-01-04-03 实现应用初始化失败处理**。**验收** 缺配置和异常入口有对应退出码。
  - 证据：`server-runtime/src/server_app.rs` 移植 ServerAppConfig（server_app_config.cpp）+ ServerApp 生命周期（server_app.cpp）+ doBWMainT/BIGWORLD_MAIN 退出码管线（bwservice.hpp）。退出码与 doBWMainT 逐值对齐：配置选项 postInit 校验失败返回 **-1**（CONFIG_ERROR "Failed to initialise configuration options." 且 app 完全不启动）、app init/run 失败 **EXIT_FAILURE(1)**（runApp 的 init 门：失败即跳过 run、ERROR "Failed to initialise <name>"、fini 无条件执行——脚本化 app 用生命周期日志钉死 init/fini、init/run/fini 次序）、健康启动 **EXIT_SUCCESS(0)**。验收「缺配置有对应退出码」：BIGWORLD_MAIN 宏体丢弃 BWResource::init 与 BWConfig::init 的返回值——缺 server/bw.xml 不中止服务端进程，全部选项落默认照常跑出 0（isBad 记录坏文件，工具侧如 consolidate_dbs 的 EXIT_SUCCESS 早退语义已记录）；验收「异常入口有对应退出码」：serverMode 非法值（standalone/center/periphery 之外）→ -1。选项框架与 BW_OPTION_RO 静态集逐项对齐：gameUpdateHertz=10、personality=BWPersonality、serverMode=standalone、production=true（isProduction 静态读 "production" 路径）、timeSyncPeriod=60 及派生 timeSyncPeriodInTicks=secondsToTicks（max(lowerBound, floorf(s*hertz+0.5))，负值/零被下界 1 挡住）、useDefaultSpace、maxOpenFileDescriptors=-1、maxSharedDataValueSize=10240、maxMgrRegisterStagger 截到 1/hertz、numStartupRetries=60；每选项 BWConfig::update 首中即写、缺失则 per-option 警告并保默认（unread_options 记录）；printAll 捕获 `%-34s = %s`/`(default: %s)` 行格式。ServerApp::init 的 runFromMachined 判定保留原版 `if(strcmp(...))` 反直觉行为（非 `-machined` 参数即置真，注释标注为原版 bug）；Mercury dispatcher/interface、SignalProcessor 与 raiseFileDescriptorLimit 留待 BW-02/BW-01-04-04。8 新测试（server-runtime 34、workspace 348、0 警告）。llvm-cov：server-runtime lib 99.70%（余花括号伪迹）。
- [x] **BW-01-04-04 接入停止信号和控制事件**。**验收** 应用进入关服阶段。
  - 证据：`server-runtime/src/signals.rs` 移植 SignalProcessor（signal_processor.cpp）+ Signal::Set/Blocker（signal_set.hpp）。异步链与原版逐环对齐：C 层 handler（::signalHandler）只做 async-safe 两件事——置 PENDING_SIGNALS 位（1..=SIGMAX=SIGSYS，AtomicU32 复刻 sigset_t 的"只置位"契约、实时信号不跟踪）+ SIGQUIT 立即槽回调（machined watchdog 路径：引用直接从信号上下文解引用 handler 指针，Rust 以薄指针 AtomicPtr<QuitSlot>（具体 Box 包 Send+Sync 回调）复刻，无分配无日志）；注册侧 sigaction 全信号 mask + sa_flags=0（SA_SIGINFO 不支持——引用以报错剥离）。分发与原版同构：dispatch() 先 Blocker（pthread_sigmask SIG_SETMASK 全阻塞）再排空 1..=31 逐信号 dispatchSignal（快照迭代防重入）最后清零 pending 集；waitForSignals=sigwaitinfo+EINTR 重试（TableSynchroniser 同款）。SIGQUIT 特例保持：单一槽、二次注册 MF_ASSERT（should_panic 钉死）、clearSignalHandler(SIGQUIT) 与 clearSignalHandlers(SIGQUIT) 双路径（Drop 经 clear_quit_handler 复位 SIG_DFL——引用析构对全部注册信号 ::signal(SIG_DFL)，SIGQUIT 含在内）。HandlerId=handler 分配身份（Rc::as_ptr），同 handler 多信号共享身份——复刻引用以 handler 指针为键、clearSignalHandler(handler) 跨信号摘除的语义（clear_handler_everywhere 测试钉死跨信号一身份、保留其余、失末环复位 SIG_DFL）。signalNumberToString 保留 SIGRTMIN..SIGRTMAX→"SIGRTxxx"（glibc 函数调用）。Deviation 已注：SIGQUIT 槽收独立 Send+Sync 回调（Rust 无法从信号上下文安全重入 RefCell；引用同样只承诺"不阻塞不分配"）。
  - 证据：`server-runtime/src/dispatcher.rs` 移植 Mercury::EventDispatcher 引导子集 + FrequentTasks（event_dispatcher.cpp/frequent_tasks.cpp）。processContinuously→processUntilBreak：入口复位 breakProcessing_（processOnce 每次进入也复位——原版怪癖钉死：预置 break 不拦单次 pass）；循环体 frequent tasks →（BW-02 接 epoll/network pass，现以 1ms idle 防忙转，已注）；processUntilSignalled 加外部旗。FrequentTasks::process 的 isDirty_ 语义钉死：pass 内任务列表被改→本 pass 到此为止、下 pass 从头再来（tasks_dirty 以 Rc&lt;Cell&gt; 共享——BW-02 handler 可达的同一通道，测试先证 mutator 中断 follower、再证下 pass 补齐）；addFrequentTask/cancelFrequentTask（按 Rc 指针身份、返回是否在册）逐名对齐；FrequentTask::task_name 默认 "UnnamedTask"。SignalDispatchTask 复刻 SignalProcessor 构造器 `dispatcher_.addFrequentTask(this)`（析构 cancel 由 app 双方同亡替代），task_name="SignalProcessor"（profiler 标签）。
  - 证据：`server-runtime/src/server_app.rs` 增控制面。trait ServerApp 扩 shut_down/on_signalled 默认实现：shutDown=INFO 行（"ServerApp::shutDown: shutting down"）+ breakProcessing——break 旗在 app 手（mainDispatcher_ 成员摊平为 Rc&lt;Cell&gt;，Rust 语义下循环运行期间 app 处于 &amp;mut 借用、C++ 的成员自引用重入在 Rust 强制摊平为共享旗，已注）；onSignalled 默认仅 SIGINT 臂（引用 switch 无其他臂）。base_on_signalled 逐行对齐 server_app.cpp：SIGINT→signal(SIGINT,SIG_DFL)+raise（原注释"就为确保 Python 不装自己的 SIGINT handler"）；重发的 SIGINT 在 dispatch() 全阻塞窗口内 kernel-pending，测试用 sigtimedwait 消费复现"阻塞中停靠"。ServerAppSignalHandler/app_signal_handler 转发 dispatched 信号入 app.on_signalled；install_app_signal_handler 复刻 init 的 createSignalHandler()+enableSignalHandler(SIGINT) 两步。
  - 验收「应用进入关服阶段」双通道钉死：①控制事件（BW-02 Mercury shutDown 消息 handler 的替身 frequent task）→INFO+break→processUntilBreak 返回→do_bw_main_t 出 **EXIT_SUCCESS(0)**（日志序 init→control event→shutting down）；②停止信号中途到达（init 注册的 "stop signal" 任务在循环中 raise SIGTERM→下 pass SignalDispatchTask 排空→handler 记 "onSignalled SIGTERM"+break→出 0，pending 集清空）。信号测试全部持 disposition_guard 串行（signal disposition 进程全局、cargo 测试并行线程；poison 有意忽略）——测试稳定性 5 连跑全绿。环境事实已钉：libtest 在测试线程预阻塞 SIGINT（routes 测试先 SIG_UNBLOCK 复原真实服务端条件再 raise）；跨线程 raise 非确定（thread-directed 只达调用线程、process-directed 会被任意并行线程吞）故 waitForSignals 用「本线程阻塞后 raise→sigwait 消费」、EINTR 重试用 pthread_kill 定向主线程（SIGUSR2 handler 中断 sigwait→EINTR→重入→SIGUSR1 到达）。
  - 新 27 测试（server-runtime 61、workspace 375、0 警告；clippy 对新文件零告警）。llvm-cov：server-runtime lib **99.62%**（app.rs 100%、dispatcher.rs 100%、config.rs 99.61%、server_app.rs 99.85%、signals.rs 99.12%，余为花括号伪迹）。
- [x] **BW-01-04-05 实现分阶段退出**。**验收** 进程和后台任务按基线顺序结束。
  - 证据：`server-runtime/src/shutdown.rs` 移植 ShutDownStage（lib/server/common.hpp）+ 关服状态 + TaskManager 引导子集（cstdmf/bgtask_manager.cpp）。阶段枚举判别值 0..5 与 C enum 一致（即 BW-02 controlledShutDownArgs::stage 的线值），shutDownStageToString 逐串对齐（"SHUTDOWN_NONE/REQUEST/INFORM/DISCONNECT_PROXIES/PERFORM/TRIGGER"，switch default 臂 → "(unknown)"，线值可携带任意 int 故收 i32）；ShutdownState 复刻 shutDownStage_/shutDownTime_（初值 SHUTDOWN_NONE——common.hpp 注释"shutdown state variables are typically initialised to"，isShuttingDown=stage!=NONE）；handle_controlled_shut_down 为各 role app controlledShutDown 接收端的公共臂：INFORM 记阶段+截止 tick、REQUEST/PERFORM 仅记阶段、NONE/DISCONNECT_PROXIES/TRIGGER 忽略（default: break 臂；per-role 行为——ack、脚本事件、CellApp 拒收 PERFORM——随 BW-02/BW-04 role app 落地）。TaskManager 为 FORCE_MAIN_THREAD 构建面（bgtask_manager.cpp:695 原版自带的调试分支，注释标注）：add_background_task 当场内联跑 do_background_task、任务按文档模式"add themselves back"经 add_main_thread_task 排队完成态；tick 复刻 fgTaskListMutex_ grab/swap/give + 快照迭代（新任务入下一轮——mid-tick 排队等下一 tick 钉死；迭代完快照即亡=引用"Smartpointers ... deleted here"）；stopAll 的关服契约=引用尾部"do a tick to stop all foreground tasks"——最后一 tick 后仍排队的完成态在 stop_all 内按序跑完；线程池（BackgroundTaskThread/stopThreads/cancel/priority）按计划 BW-01-05 落地（文档已注）。
  - 证据：`server-runtime/src/server_app.rs` 退出管线补全。trait 扩 on_run_complete/prepare_for_shutdown 默认（后者为 interface_.prepareForShutdown 摊平——Mercury interface BW-02 落地前由 app 承载调用位）；base_run=基础 ServerApp::run 体（processUntilBreak→onRunComplete→true）；run_app 复现 runApp 全序：init 门→`---- %s is running ----` INFO→run→fini→prepareForShutdown（fini 与 prepareForShutdown 无条件执行，init 失败也走——引用 runApp 同构）；新增 bw_main_t=bwMainT 层（Mercury wiring 缺位已注）：do_bw_main_t 后无条件 INFO `%s has shut down.`（bwservice.hpp:145，**每条退出路径都打印**，含 -1 配置失败）；bigworld_main 改经 bw_main_t。
  - 验收「进程按基线顺序结束」：LifecycleApp 全钩子日志钉死健康序 init→run→onRunComplete→fini→prepareForShutdown 出 0；init 失败序 init→fini→prepareForShutdown 出 1（run/onRunComplete 完全不进）；run 失败同健康序出 1；bw_main_t 三出口（-1/1/0）全流经尾部行。
  - 验收「后台任务按基线顺序结束」：BgApp 日志序 init→bg work（pass1 内联后台相）→bg cleanup（pass2 tick 排空完成态）→late bg work（最后一 tick 后排队）→fini→late cleanup（**stopAll 尾 tick 在 fini 内补完**）→prepareForShutdown——迟到的完成态不跨过 fini、不活到 prepareForShutdown 之后。
  - 新 20 测试（server-runtime 81、workspace 395、0 警告）。llvm-cov：server-runtime lib **100.00%**（2408/2408，app/dispatcher/server_app/shutdown/signals/config 六文件全 100%，连上轮遗留花括号伪迹亦清零）——5 连跑逐跑独立 100% 且逐行 miss 集全等（确定性、无抖动）。

#### BW-01-05 Tick、定时器和后台队列

实现游戏时钟、Updatables、任务完成回主线程及关服排空。比对同 tick 多事件、取消、回调内重新注册，以及后台任务完成与销毁的先后。

- [x] **BW-01-05-01 推进游戏 Tick 和 Updatables**。**验收** 虚拟时钟下阶段顺序吻合。
  - 证据：`server-runtime/src/tick.rs` 移植 Updatable/Updatables（lib/server/updatable.hpp、updatables.cpp）+ TickState/advanceTime（server_app.cpp）。Updatables 逐支对齐：levelSize_[l] 为层末指针（"number of Updatables at that level or lower"）、层序更新（"lower level ... updated before higher level"，层内注册序）、adjustForAddedObject 的洞下沉移位算法（顶层尾进、逐层搬 front、每层 ++）、add 于 pass 中只挂尾部负 handle（-1-level）等 call 后统一 adjust；call 以**初始 size** 迭代（"not processed until next tick ... prevent infinite loops"）；remove 于 pass 中打 NULL 洞 + deletedObjects_ 计数、pass 后逐层压实（洞上移到层末、层末并入洞、handles 表同步、truncate 到 levelSize_.back()）；remove 未安置对象走尾部扫描（mid-call 添加者可被同 pass 后续 update 摘除且永不运行——专测钉死）；add 诊断三臂对齐：非法层 ERROR "Invalid level %d"+false、重复添加 ERROR "Already added."+false、handle 指向他处 CRITICAL（移植版 panic 同文案——引用为 abort）。引用自带单测 test_updatables.cpp 三段全复刻：simple pass（21 对象 3 层、visits 层序非降、numCalls==size）、add pass（全局交替加牌、numCalls==21、size→3*21/2=31、新对象等下一 tick）、delete pass（31 对象交替自删、被删对象仍记录 visit、size→origSize/2=15、幸存者续跑）。removalHandle_ 摊平为按 Rc::as_ptr 身份的侧表（注释标注；可观察的安置/摘除行为不变）；update 收 &mut Updatables（引用经成员/全局可达，同 doBackgroundTask(mgr) 摊平法）。
  - 证据：advanceTime 阶段序钉死（验收「虚拟时钟下阶段顺序吻合」）：timestamp() 注入（with_clock 闭包，默认单调纳秒 + stamps_per_second=1e9，摊平已注）驱动 HookApp 全钩子日志：tick1（lastAdvanceTime_==0）无 period 钩子，序 end-of-tick→**time_++**→start-of-tick→callUpdatables（层序）→processing-complete；tick2 前置 onTickPeriod(0.3)（0.3s@10Hz 精确捕获，hertz 随 state 传入断言）再进同序——基线序 period→end→++time→start→updates→complete 与引用逐行同构。onTickPeriod 基础体（base_on_tick_period："Last game tick took %.2f seconds. Expected %.2f."，阈值 tickPeriod*hertz>2.0）为 trait 默认；onEndOfTick/onStartOfTick/onTickProcessingComplete/onSetStartTime 空默认齐备（引用注释原样入档）。TickState 摊平 time_/lastAdvanceTime_/updatables_ 三成员（role app 内嵌，同 ShutdownState 惯例）；setStartTime 先 onSetStartTime(old,new) 再赋值（钩子见旧值，专测钉死）；gameTimeInSeconds=time/hertz。
  - 新 17 测试（server-runtime 98、workspace 412、0 警告）。llvm-cov：server-runtime lib **100.00%**（3015/3015，七文件含新 tick.rs 598 行全 100%）——7 连跑逐跑独立 100% 全等（此前一次 llvm-cov 批跑出现一过性空报告退出失败，重跑与后续 7 连跑全绿，无法事后归因、记录存疑）。
- [x] **BW-01-05-02 实现事件定时器**。**验收** 同 Tick 到期项顺序吻合。
  - 证据：`server-runtime/src/timers.rs` 移植 TimeQueueT/TimerHandle/TimerHandler（lib/cstdmf/time_queue.hpp、time_queue.ipp、timer_handler.hpp）+ DispatcherTimers（EventDispatcher 计时面）。Stamp 泛型（Copy+Default+PartialOrd+Add+Sub，别名 TimeQueue=u32 / TimeQueue64=u64 对齐引用双实例化）；Node 三态 PENDING/EXECUTING/CANCELLED；TimerHandle 按 Rc::ptr_eq 同一性比较、is_set/cancel/clearWithoutCancel（不含取消的解绑）齐备。TimerHandler 语义对齐：handleTimeout 纯虚 + onRelease 默认空；release=decTimerRegisterCount+onRelease——onRelease 在 handleTimeout 栈上**重入**调用（recorder 专测钉死 fired→released 序），numTimesRegistered_ 析构断言泄漏卫兵摊平消失（Rc 保活，注释标注）；TimerHandlerFn 闭包适配器带 pub name（profiler 标签）。owner_ 摊平 Weak<RefCell<Inner>>（节点指回队列）。
  - 证据：**堆原语逐行对齐 libstdc++**（验收「同 Tick 到期项顺序吻合」的核心）：拒绝 BinaryHeap（其并列序与 std 不同），手写 __push_heap/__adjust_heap/make_heap/pop_heap 四原语（isize 算术对 (len-1)/2、(len-2)/2）；`__adjust_heap` 洞下行取子规则 `if time(right) > time(left) second_child -= 1`——右子**严格更晚**才取右子，等期限取左子；偶长单子分支 `len&1==0 && second==(len-2)/2`；上浮阶段等键不上冒。以 **g++ 编译探针**（/tmp/heap_order.cpp 链真实 libstdc++）钉死并列弹出序：四等期限 A,B,C,D 弹出 **A,C,B,D**；混合 W10,X5,Y7,Z10,V10 弹 **X,Y,V,W,Z**——探针结果原样写进 same_tick 测试断言与注释。purge 后重堆同构：引用 std::partition+make_heap → Rust retain+make_heap（heapify 抹平序差，双测钉死：单父路径 + 10 存 4 全父遍历两层）。
  - 证据：process/trigger/cancel/purge 全语义：process(now) 弹 front 至 `time<=now || cancelled`，lateness=now-time 记 maxLateness、仅 >0 才 numLateTimers++（迟到面专测：1 处 0.1s 定时器 0.5s 后跑 5 发，max 400ms、num 4、nextExp 100ms）；triggerTimer PENDING→EXECUTING→handleTimeout→once-off（interval==0 且未取消）自动 cancel→未取消 time+=interval 回 PENDING（自取消专测：回调内 cancel 的句柄只发一次不重排；cancelled 守卫专测：直接调 trigger/cancel 不双发）；cancel 已取消即 no-op、非 pending/executing 断言、→CANCELLED、onRelease 重入、owner ++numCancelled_、`numCancelled_*2 > queue.len()` 触发 purge——**取消节点留在队列、len 含之**：10 项取消 6 → 12>10 才触发、4 幸存（初版 3/7 测试错在漏算残留节点，已按队列实态纠正）；purge 尾断言 numCancelled==0||1（1 = 处理中 once-off 不在队列）。
  - 证据：clear 真算法（首次摘录有误，重读 time_queue.ipp 31-150 纠正）：maxLoopCount=size；while !empty: unsafePopBack；`if (!isCancelled && maxLoopCount>0) { --maxLoopCount; --numCancelled_; cancel(); } else if (isCancelled()) { --numCancelled_; }`——前置 --numCancelled_ 抵消 cancel 内 ++，purge 断言天然成立；无循环后清零（自然排干）；numCancelled_ 为 int 可瞬负 → Rust isize。clear 专测钉死：未到期项全收 onRelease（fired 空、released 全序）、已取消项只减计数不再 onRelease。
  - 证据：DispatcherTimers 摊平（pTimeQueue_/clock_/numTimerCalls_）：addTimerCommon 断言 handler、`microseconds <= 0` 返回空 handle（专测）、interval=us→stamps（stamps_per_second=1e9，stamps_from_microseconds=(us/1e6)×sps：0.1s=100_000us）；addTimer 周期 / addOnceOffTimer interval=0；processTimers 累加 numTimerCalls；timerDeliveryTime（EXECUTING 节点=time+interval——回调内重注册查到期用，BW-01-05-03 地基）、timerIntervalTime、getTimerInfo、maxTimerLateness/numLateTimers/reset、adjustBy、lastProcessTime、nextExp；real_clock 默认单调纳秒、with_clock 注入虚拟钟。Dispatcher::with_timers/timers()/timers_mut()/add_timer/add_once_off_timer；processOnce 序钉死：breakProcessing_ 入口复位→frequent tasks→`if (!breakProcessing_) processTimers()`→network（frequent task 内 break 跳过 timer pass，recorder 空日志专测）；两 pass 虚拟钟测试（1e9→1.5e9）钉 frequent→timers 相对序。
  - 测试与覆盖：新 34 测试（server-runtime 132、workspace 446、0 警告）。timers 28 条：并列序（探针双序）、区分期限+周期项同 pass 重复、once-off 恰一发、周期取消即止、自取消、外取消跳过、取消的 front 不发、回调内新增 same-pass/next-pass、迟到面、purge（单父+全父两层）、clear 释放序、nextExp、adjustBy、info/deliveryTime、lastProcessTime、u32 队列同构、空 handle 惰性、real_clock 递增、dispatcher 两 pass、break 跳 timer pass、非正延迟、lateness 查询面、default≡new、clearWithoutCancel、句柄同一性+Debug、堆子选择、purge 重堆、守卫不双发、队列弃后取消安全。llvm-cov：TOTAL **99.56%**（4077 行 18 missed）——tick 99.43 / timers 99.55 / dispatcher 100 / shutdown 100 / app 100 / config 99.61 / server_app 99.80 / signals 98.72，全 ≥98% 门；逐文件核对残余 missed 行**全部为孤立 `}`**（llvm-cov 标点伪影，真实逻辑行零遗漏）。
- [x] **BW-01-05-03 实现取消与回调内重注册**。**验收** 验证不会重复触发旧句柄。
  - 证据：cancel 句柄语义纠正（time_queue.cpp TimerHandle::cancel）：引用 `pNode_ = NULL` **先于**触碰节点——cancel 后句柄立即 unset（isSet false）、二次 cancel 全惰性（连节点都不碰）；取消前持有的**副本**仍指向节点（C++ 拷贝共享 pNode_，移植版 clone 共享 Rc）但节点 cancelled 守卫使其不可触发、不可二次释放（专测钉死：stale copy cancel 不再 onRelease、trigger_timer 直接调也不发）。TimerHandle::cancel(&self)→(&mut self)，全调用点加 mut。
  - 证据：可变 timerIntervalTime（Node::intervalRef，引用 `TIME_STAMP & timerIntervalTime(handle)`）移植：摊平为 set_timer_interval_time setter（TimeQueueT + DispatcherTimers 双层），回调内改自身周期——post-callback `time += interval` 重排用新值：TimeKeeper 舞步专测（5 期触发→短到 3→回正 5，逐 pass 断言 next delivery 15/18/23/28，首 delivery 不受影响）。面外改（inputMasterReading 从消息处理器调 tracking timer——非回调路径）同样只影响重排：DispatcherTimers with_clock 专测 500ms→450ms，重排 1_500_000_000→1_950_000_000 钉死。规范用户档案 lib/server/time_keeper.cpp：inputMasterReading 慢/快阈值 ±(nominal/20+rtt/2)、increment=nominal/20、handleTimeout 的 sync-check 触发即回正 nominal；BW-02 TimeKeeper 实体落地时以此为准。
  - 证据：getTimerInfo cancelled 修复：引用 `if (!pNode->isCancelled()) return true`——移植版补 cancelled→None（此前漏）；cancelled 句柄已 unset（`?` 短路 None）与 stale copy（走 cancelled 分支）双路径均 None，专测分别钉死。
  - 证据：重注册模式专测（验收「不会重复触发旧句柄」）：①跨取消——回调内取消另一 pending 定时器：受害者永不触发、onRelease 同步发生在取消者回调内（log [canceller, victim-released]）、purge 在处理中按 len 含取消节点数学触发；②相位保持重注册——回调内先读 timerDeliveryTime（Executing 节点=time+interval）再建新句柄、后 cancel 旧句柄：迟到 delivery(12) 重排到原相位 15 而非 17（process(17)=0、nextExp(17)=3 钉死），旧句柄永不再触发；规范用户 cellapp.cpp:1205/baseapp.cpp:1440 的 timerDeliveryTime(gameTimer_)；③scheduleSyncCheck 防重卫兵——is_set 即拒绝（"busy"，引用 "existing timer ID" TRACE 早退）不定时器堆叠；check 触发先 clearWithoutCancel 再干活；清空后才可再武装（schedule→busy→check→schedule→check 全序列钉死；re-arm 相对 last_process_time+10，引用 deliveryIn 由时钟推导的摊平）。
  - 测试与覆盖：新 6 测试（server-runtime 138、workspace 452、0 警告）。llvm-cov：TOTAL **99.58%**——timers **99.63**（残 4 行逐行核对全为孤立 `}` 标点伪影）、tick 99.43 / dispatcher 100 / shutdown 100 / app 100 / config 99.61 / server_app 99.80 / signals 98.72，全 ≥98% 门。
- [x] **BW-01-05-04 将后台完成回调派发至主线程**。**验收** 线程 ID 和时序断言通过。
  - 证据：`task_manager.rs` 全量移植 `cstdmf::TaskManager`（bgtask_manager.hpp/.cpp）：`BackgroundTaskList::push` 排序插入「Insert with highest priority value tasks first」——从队尾回扫，`priority > iter->first` 停在该处插入，等值保持到达序（专测 MAX→DEFAULT→LOW 乱序入列、单 worker 依序抽走；等值到达序专测）；每次 push 放一张信号量票。
  - 证据：完成回调派发（本条验收核心）——worker 循环 `pull` → `doBackgroundTask` → `onBackgroundTaskFinished`（按 `Arc::ptr_eq` 从 active 摘除）；`addMainThreadTask` 任意线程可调（fg 列表带锁）；`tick` 原子交换 fgTaskList_ 后在**主线程**跑 `doMainThreadTask`。验收断言：bg 线程 ID ≠ 主线程、bg ≠ fg、fg == 主线程、log 序 ["bg","fg"]；mid-tick 新增的 completion 走 newTasks_ 等下一个 pass（leader→follower 链专测）。
  - 证据：线程生命周期：stop 哨兵（NULL task）→ worker 退出 → `addMainThreadTask(ThreadFinisher)` → 下一次 tick 的 `onThreadFinished` 才 `--numRunningThreads_`（故测试等待谓词内必须 tick；专测钉死）。startThreads 同时 ++numUnstoppedThreads_/numRunningThreads_；stopThreads 只减 unstopped 并补等量哨兵；setNumberOfThreads 增减对账（等值分支专测）。`pull` 空队列阻塞至 push 唤醒（专测覆盖等待分支）。
  - 证据：`stopAll(discard, wait)` 全序照抄：先 clearRealTasks（"Do this before stopping threads, because 'stop' tasks are pulled last"）→ stopThreads(unstopped) → MF_ASSERT 0 → 等待循环（tick；若仍有 running：discard 时从 active 尾弹 `stop()`、sleep 100ms）→ 再 clearRealTasks（"in case addBackgroundTask was called while we were waiting"）→ 收尾 tick。clearRealTasks 从**尾**弹、cancel 真任务、数哨兵并回推（"Keep stop (NULL) tasks or their threads will never be stopped"）。cancel/stop 双路径专测：排队任务被 cancel（不是 stop）、长任务被 stop，log 序 ["cancelled-pending-2","cancelled-pending-1","stopped-long"] 钉尾弹序。
  - 证据：flatten / Rust 形态：`BackgroundTask: Send + Sync` 且方法收 `&self`（C++ 的 volatile `stop_` 对应原子标志——整对象加 Mutex 会死锁 stopAll 等待循环里的 stop()）；`TaskPtr = Arc<dyn BackgroundTask>`；语义全部摊平到 TaskManager 上的方法；FORCE_MAIN_THREAD 式的内联建厂保留在 shutdown.rs 的关服路径（本模块为真线程池）。delayed 列表 `tick` 时**无排序追加到尾部**（push(List) 重载），且停放期不计入 numBgTasksLeft（专测）。~TaskManager → stopAll(true,true)（丢弃时停池专测）。
  - 测试与覆盖：新 18 测试（server-runtime 156、workspace 470、0 警告）。llvm-cov：TOTAL **99.59%**——task_manager **99.65**（残 2 行逐行核对：1 为 stopAll 等待循环的回边孤立 `}`、1 为 `}` 标点伪影，真实逻辑行零遗漏）、timers 99.63 / tick 99.43 / dispatcher 100 / shutdown 100 / app 100 / server_app 99.80 / config 99.61 / signals 98.72，全 ≥98% 门。
- [x] **BW-01-05-05 实现关服时队列排空**。**验收** 关服与任务完成并发时状态可复现。
  - 证据：`TaskManager::drain` 落地关服排空（~BaseApp 里 `bgTaskManager_.stopAll()` 之前 `extInterface().prepareForShutdown()`，baseapp.cpp:272/275 的顺序）：`stop_all(false, true)` **discard=false**——引用 `stopAll(true,…)` 的 clearRealTasks 会把排队任务 cancel 掉，那是丢弃不是排空；discard=false 时 stop 哨兵排在其后（"stop" tasks are pulled last），worker 先抽完真任务再退出。故 drain 不能直接复用 `stop_all_defaults`（true,true）。
  - 证据：drain 补上引用的收尾注释（"do a tick to stop all foreground tasks. Note that more foreground tasks could be added while it's stopping the current list"）——`stopAll` 只 tick 一次，drain 循环 tick 直到 `has_tasks()` 为空：退出途中 worker 排入的 completion（含 ThreadFinisher）在调用线程上一次不落地、一次不漏。
  - 证据：验收「并发时状态可复现」三测试钉死终态：①drain_ends_in_the_same_state_no_matter_the_interleaving——运行中任务+排队任务，终态 log=["worker","late-pending","done"]、num_running=0/unstopped=0/bg_left=0/has_tasks=false，二次 drain 幂等（关服可重复）；②drain_runs_completions_queued_after_the_first_tick——worker 从线程外排 completion（stopAll 的 tail tick 之后到达）仍被 drain 跑到；③drain_keeps_ticking_while_completions_arrive——completion 在 drain 自己的 tick 期间再链一个（"more foreground tasks could be added while it's stopping the current list"），drain 的 while 循环补上这一 pass（log=["first","done"]；此测试还证明 tail tick 之后到达的 completion 不丢）。
  - 证据：出口管线接线（server_app::tests::the_shutdown_drains_the_threaded_task_pool）——role app 在 run 期间用真线程池排 bg 任务，fini 里 drain，再 prepareForShutdown；线程 ID/时序断言用不变式而非固定序（bg 与 main 并发，worker 与 shutdown 的交织不钉死，但钉死：每阶段恰一次、work 先于自己的 completion、两个 completion 都在 "fini" 之后、prepareForShutdown 收尾）。do_bw_main_t 返回 EXIT_SUCCESS。
  - 证据：drain 语义与 stop_all 的分工——stop_all(true,*) 丢弃+停运行中任务（cancel/stop 双路径，BW-01-05-04）、drain 放行排队任务跑完；两者都以 `~TaskManager→stopAll()` 收尾，Drop 幂等。
  - 测试与覆盖：新 3 测试（task_manager 2 + server_app 1；server-runtime 160、workspace 474、0 警告；drain 相关 20 次重复 + 交织测试 30 次重复无 flake）。llvm-cov：TOTAL **99.61%**——task_manager **99.71**（残 2 行逐行核对：drain 的 `}` 回边与循环尾孤立 `}`，llvm-cov 标点伪影）、server_app 99.82 / timers 99.76 / tick 99.58 / dispatcher 100 / shutdown 100 / app 100 / config 99.61 / signals 98.72，全 ≥98% 门。

### BW-02 Mercury 网络、RPC 和连接基础

源码 `lib/network/`、`lib/connection/`、`common/py_network.cpp`。

#### BW-02-01 I/O 与事件分发

覆盖 Endpoint、EventPoller、EventDispatcher、`NetworkInterface`、socket 缓冲配置和定时器。记录网络、频繁任务与计时器同轮就绪时的顺序。

- [ ] **BW-02-01-01 实现 Endpoint 地址与 socket 操作**。**验收** IPv4/适用 IPv6 夹具通过。
- [ ] **BW-02-01-02 实现轮询器注册、注销与唤醒**。**验收** 空闲轮询可被正常停止。
- [ ] **BW-02-01-03 实现 NetworkInterface 派发**。**验收** 正确接口接收对应事件。
- [ ] **BW-02-01-04 应用 socket 缓冲配置**。**验收** 成功和不可满足时的结果吻合。
- [ ] **BW-02-01-05 对照 dispatcher 单轮顺序**。**验收** 定时器、频繁任务及网络事件排序吻合。

#### BW-02-02 接口注册和 RPC

从接口定义及宏展开提取消息 ID、固定或可变长度、handler、请求/应答 ID 与超时。比对内部和外部接口的字节序、包头和请求回调。

- [ ] **BW-02-02-01 提取内外部消息 ID 与长度表**。**验收** 同原版接口注册表比对。
- [ ] **BW-02-02-02 实现固定及变长消息帧**。**验收** 黄金包头及负载逐字节一致。
- [ ] **BW-02-02-03 实现请求关联与应答派发**。**验收** 交错请求各自收到正确响应。
- [ ] **BW-02-02-04 实现请求超时和取消**。**验收** 回调恰好一次并释放挂起状态。
- [ ] **BW-02-02-05 注册处理器与校验**。**验收** 未知消息与长度错误结果吻合。

#### BW-02-03 可靠 UDP

实现 Packet、Bundle、UDPChannel、ACK/序号窗口、重传、乱序处理、分片重组、Channel 生命周期及统计。双向混跑并覆盖丢包、延迟、重复包和序号回绕。

- [ ] **BW-02-03-01 实现 Packet/Bundle 编解码**。**验收** 真实 UDP 帧逐字节往返。
- [ ] **BW-02-03-02 实现 Channel 注册与关闭**。**验收** 重复创建和旧 Channel 回收吻合。
- [ ] **BW-02-03-03 实现 ACK 与序号窗口**。**验收** 序号回绕夹具通过。
- [ ] **BW-02-03-04 实现重传计时**。**验收** 单次可控丢包后仅交付一次。
- [ ] **BW-02-03-05 实现乱序与重复过滤**。**验收** 按原版顺序派发。
- [ ] **BW-02-03-06 实现分片重组**。**验收** 乱序片段可恢复且超时可回收。
- [ ] **BW-02-03-07 运行双向 UDP 混跑**。**验收** 两个方向可交换并正确统计重传。

#### BW-02-04 TCP 与外部传输

移植 TCPBundle/TCPChannel、帧边界、读写缓冲、连接过滤及在构建中启用的传输方式。对照短读写、背压、超时和断链。

- [ ] **BW-02-04-01 实现 TCPBundle 帧解析**。**验收** 短读与合并读取两类夹具通过。
- [ ] **BW-02-04-02 实现短写和待发送缓冲**。**验收** 背压下按序发出完整帧。
- [ ] **BW-02-04-03 接入原版启用的连接过滤**。**验收** 允许及拒绝分别匹配。
- [ ] **BW-02-04-04 实现超时和断链清理**。**验收** 挂起应答与队列状态吻合。
- [ ] **BW-02-04-05 核查其余已启用传输方式**。**验收** 每种有混跑夹具或阻塞记录。

#### BW-02-05 包过滤和加密互通

按原版启用条件复现过滤链、会话加密协商、包校验和受控限流，使用双方合法客户端互操作。对照正常握手、认证失败及流量达到配置上限时的结果。

- [ ] **BW-02-05-01 实现过滤链与包校验**。**验收** 过滤顺序及校验失败结果一致。
- [ ] **BW-02-05-02 对齐会话协商**。**验收** 原版和新版客户端可合法握手。
- [ ] **BW-02-05-03 校验认证失败时的返回状态**。**验收** 无残留已建立会话。
- [ ] **BW-02-05-04 实现配置限流**。**验收** 边界流量下的处置与原版吻合。

### BW-03 CPython 嵌入和脚本 API

源码 `lib/script/`、`lib/pyscript/`、`common/`、各进程 `script_bigworld.cpp`、[Python 迁移台账](../BigWorld/docs/python-313-migration.md)。

#### BW-03-01 解释器生命周期

对齐初始化、模块注册、资源导入路径、标准库加载、脚本日志、异常报告和退出顺序。验证各进程启动、空脚本和异常脚本。

- [ ] **BW-03-01-01 初始化并退出 CPython**。**验收** 重复运行独立进程均正常退出。
- [ ] **BW-03-01-02 配置标准库和资源导入路径**。**验收** 原版脚本可原样导入。
- [ ] **BW-03-01-03 按角色注册原生模块**。**验收** 实际导入结果与角色基线一致。
- [ ] **BW-03-01-04 接入脚本日志**。**验收** stdout 和异常落入预期日志入口。
- [ ] **BW-03-01-05 核查 Python 关服顺序**。**验收** 脚本清理回调满足宿主释放时序。

#### BW-03-02 Python 对象桥接

覆盖原生对象包装、属性与方法绑定、容器代理、引用保活、GC、GIL 和跨线程回调约束。比较对象身份、异常类型、回调析构与循环引用。

- [ ] **BW-03-02-01 实现原生包装对象的身份缓存**。**验收** 同一底层对象取回同一实例。
- [ ] **BW-03-02-02 实现属性和方法绑定**。**验收** 错误参数抛出约定异常。
- [ ] **BW-03-02-03 实现容器代理**。**验收** 修改代理可观察到原生状态同步。
- [ ] **BW-03-02-04 实现引用保活与循环 GC**。**验收** 对象释放只发生一次。
- [ ] **BW-03-02-05 约束 GIL 与跨线程回调**。**验收** 线程退出及异常后无悬挂回调。

#### BW-03-03 公共脚本模块

移植脚本数学与 Provider、时间和定时器、资源访问、网络/URL 请求（`lib/urlrequest/`）及共享模块。按进程盘点模块名、参数、返回值和异常，通过原样调用的 Python 测试。

- [ ] **BW-03-03-01 维护公共 Python 导出清单**。**验收** 模块、签名与进程范围可核对。
- [ ] **BW-03-03-02 移植数学对象及 Provider**。**验收** 运算和求值顺序匹配。
- [ ] **BW-03-03-03 绑定定时器模块**。**验收** 取消、重注册和返回值匹配。
- [ ] **BW-03-03-04 绑定资源访问**。**验收** 读写 DataSection 与继承路径匹配。
- [ ] **BW-03-03-05 绑定网络和 URL 请求**。**验收** 成功及超时回调匹配。
- [ ] **BW-03-03-06 回归公共脚本**。**验收** 原版测试可直接调用兼容模块。

#### BW-03-04 进程脚本入口与热重载

复现角色专属 `BigWorld` API、脚本启动回调和当前实际接通的重载行为。比较类替换、已有实例、失败后的状态和引用清理。以 [热重载生产化计划](../BigWorld/docs/upgrade-plan/hot-reload-productionization.md) 反查计划项与已落地代码。

- [ ] **BW-03-04-01 注册各角色 BigWorld API**。**验收** 角色可见方法清单匹配。
- [ ] **BW-03-04-02 触发脚本启动回调**。**验收** 顺序和参数与原版一致。
- [ ] **BW-03-04-03 核实原版重载入口及启用条件**。**验收** 记录可达及计划项目。
- [ ] **BW-03-04-04 移植类替换和现存实例更新**。**验收** 旧实例调用新方法。
- [ ] **BW-03-04-05 保证重载失败后的状态**。**验收** 失败后旧定义仍可调用且引用清理。

### BW-04 EntityDef、实体数据与 RPC 契约

源码 `lib/entitydef/`、`lib/entitydef_script/`、`lib/bwentity/`、`tools/process_defs/`。

#### BW-04-01 定义解析与编译

覆盖 `entities.xml`、`.def`、`alias.xml`、父类和接口、Service 定义、属性默认值与索引、Base/Cell/Client 方法排序。比对实体、属性、方法 ID 和定义摘要。

- [ ] **BW-04-01-01 解析实体和别名文件**。**验收** 实体 ID、别名解析及缺项错误吻合。
- [ ] **BW-04-01-02 合并父类和接口**。**验收** 继承冲突与优先级吻合。
- [ ] **BW-04-01-03 处理 Service 定义**。**验收** 公开方法与原版清单一致。
- [ ] **BW-04-01-04 分配属性和方法索引**。**验收** Base、Cell、Client 索引吻合。
- [ ] **BW-04-01-05 计算定义摘要**。**验收** 同输入得到与原版相同字节。

#### BW-04-02 数据类型和属性流

覆盖基本类型、ARRAY/TUPLE、FIXED_DICT、USER_TYPE、MAILBOX、嵌套属性和变化切片。比对 Python 类型转换、默认值、压包、广播和持久化字段。

- [ ] **BW-04-02-01 实现标量与默认值转换**。**验收** 数值边界和异常对照通过。
- [ ] **BW-04-02-02 实现 ARRAY/TUPLE**。**验收** 长度、元素及嵌套字节一致。
- [ ] **BW-04-02-03 实现 FIXED_DICT**。**验收** 字段次序和缺省行为一致。
- [ ] **BW-04-02-04 实现 USER_TYPE 与 MAILBOX**。**验收** 转换及序列化夹具一致。
- [ ] **BW-04-02-05 编码嵌套属性增量**。**验收** 修改一个 FIXED_DICT 子字段时，更新切片的路径和负载与原版逐字节一致。
- [ ] **BW-04-02-06 应用嵌套属性增量**。**验收** 接收原版更新切片后，只有目标字段改变，Python 通知携带正确旧值。
- [ ] **BW-04-02-07 提取持久化投影**。**验收** 数据库属性流与原版字节一致。

#### BW-04-03 可见性与调用约束

覆盖 exposed 方法、属性分发标志、客户端/服务端权限、参数校验、方法应答和 MailBox 引用。比对同一实体定义在 Base、Cell、Client 和 DB 的投影。

- [ ] **BW-04-03-01 实现 exposed 方法可见性**。**验收** 客户端只能调用允许方法。
- [ ] **BW-04-03-02 处理属性分发标志**。**验收** 同一属性的四类接收方与基线一致。
- [ ] **BW-04-03-03 验证参数与权限失败**。**验收** 返回同类错误且业务回调未执行。
- [ ] **BW-04-03-04 实现带返回值的方法应答**。**验收** 响应字段和超时行为一致。
- [ ] **BW-04-03-05 序列化 MailBox 引用**。**验收** 原版可恢复实体位置与类型。

#### BW-04-04 定义工具和 SDK 契约

复现 `process_defs` 的命令行、诊断及产物。核对 `bwentity` 和客户端集成示例依赖的定义接口与生成结果。

- [ ] **BW-04-04-01 兼容 process_defs 参数**。**验收** 帮助、退出码和选项有基准。
- [ ] **BW-04-04-02 兼容定义诊断**。**验收** 坏 .def 显示对应文件和字段。
- [ ] **BW-04-04-03 复现生成产物**。**验收** 对照 ID、摘要及引用关系。
- [ ] **BW-04-04-04 验证 bwentity 与集成示例**。**验收** 用产物成功加载脚本定义。

### BW-05 集群控制、进程发现和恢复

源码 `server/tools/bwmachined/`、`lib/network/machine_guard.cpp`、`server/reviver/`、`server/{baseappmgr,cellappmgr,dbappmgr}/`。

#### BW-05-01 Machined 与服务发现

复现 MachineGuard 查询、进程注册/注销、发现、出生/死亡通知、主机和进程统计，以及原版工具所用的受控进程管理入口。用原版组件和新版守护进程做双向互测。

- [ ] **BW-05-01-01 实现 MachineGuard 查询**。**验收** 相同请求返回兼容负载。
- [ ] **BW-05-01-02 登记与注销进程**。**验收** 发现结果即时反映变化。
- [ ] **BW-05-01-03 发送出生及死亡通知**。**验收** 订阅者收到同顺序通知。
- [ ] **BW-05-01-04 采集主机和进程统计**。**验收** 公共指标字段满足原版查询。
- [ ] **BW-05-01-05 对齐受控进程管理入口**。**验收** 合法生命周期操作对应原版退出状态。
- [ ] **BW-05-01-06 原版守护进程与新版角色双向联调**。**验收** 两种替换方向均可发现。

#### BW-05-02 Reviver 外部恢复

覆盖组件探活、优先级、异常重启、重新注册和关服抑制。验收为进程退出和失联后恢复流程与原版等价，且正常停机不会触发误重启。

- [ ] **BW-05-02-01 采集探活与过期**。**验收** 延迟、失联及恢复三种状态可复现。
- [ ] **BW-05-02-02 实现重启优先级**。**验收** 多个目标失败时按基线顺序处理。
- [ ] **BW-05-02-03 异常退出后重启并重新登记**。**验收** 角色恢复可服务状态。
- [ ] **BW-05-02-04 识别正常关服**。**验收** 关闭期间不会误重启。

#### BW-05-03 BaseAppMgr 调度

覆盖 BaseApp 注册/移除、负载报告、创建目标选择、登录条件、全局状态转发和关闭协调。比较多实例下的调度和失败回退。

- [ ] **BW-05-03-01 登记与移除 BaseApp**。**验收** 调度列表状态一致。
- [ ] **BW-05-03-02 接收负载报告并挑选创建目标**。**验收** 多实例夹具选择吻合。
- [ ] **BW-05-03-03 处理登录条件失败与回退**。**验收** 返回预期拒绝状态。
- [ ] **BW-05-03-04 转发全局状态**。**验收** BaseApp 接收相同更新。
- [ ] **BW-05-03-05 协调关服**。**验收** 工作进程按阶段退出。

#### BW-05-04 CellAppMgr 分区和负载均衡

覆盖 Space/Cell 创建与销毁、BSP 切分、边界报告、负载采样、拆并 Cell、跨主机分配和 viewer 读接口。用原版轨迹对照 split/merge 和重分配。

- [ ] **BW-05-04-01 创建和销毁 Space/Cell**。**验收** 空空间与活跃空间状态吻合。
- [ ] **BW-05-04-02 实现 BSP 切分和边界报告**。**验收** 固定负载序列边界一致。
- [ ] **BW-05-04-03 实现 Cell 合并**。**验收** 低负载时实体归属与基线一致。
- [ ] **BW-05-04-04 实现跨主机分配**。**验收** 候选主机与失败回退匹配。
- [ ] **BW-05-04-05 提供 viewer 读接口**。**验收** 展示拓扑与实际分配一致。

#### BW-05-05 DBAppMgr 发现和主从角色

覆盖 DBApp 注册、主节点选取、负载/状态转发，以及死亡后的拓扑和服务通知。比对已登录实体和未完成请求的恢复结果。

- [ ] **BW-05-05-01 登记 DBApp 并选取主节点**。**验收** 加入顺序固定时选择吻合。
- [ ] **BW-05-05-02 转发 DB 负载与状态**。**验收** 对端可读到最新值。
- [ ] **BW-05-05-03 处理节点死亡和角色切换**。**验收** 拓扑与服务通知吻合。
- [ ] **BW-05-05-04 核查未完成请求和在线实体**。**验收** 主节点更换后结果可对照。

#### BW-05-06 集群阶段与故障矩阵

对齐启动就绪条件、时间同步、受控关服及经理/工作进程失联通知。逐角色验证重启、网络隔离、进程恢复及重复通知，记录哪些故障原版支持自动恢复。

- [ ] **BW-05-06-01 实现启动就绪条件**。**验收** 缺少必要角色时保持未就绪。
- [ ] **BW-05-06-02 实现进程时间同步**。**验收** 固定样本的游戏时间漂移在允许范围。
- [ ] **BW-05-06-03 实现关服阶段广播**。**验收** 各角色报告及退出次序吻合。
- [ ] **BW-05-06-04 验证各角色异常退出与重启**。**验收** 结果记入故障矩阵。
- [ ] **BW-05-06-05 验证隔离、重连和重复通知**。**验收** 实体/会话终态可复现。

### BW-06 LoginApp、BaseApp 会话与客户端连接

源码 `server/loginapp/`、`server/baseapp/{proxy.cpp,login_handler.cpp,pending_logins.cpp}`、`server/dbapp/*login*`、`lib/{connection,connection_model}/`。

#### BW-06-01 LoginApp 公共登录入口

复现登录参数和版本检查、挑战的启用和应答、请求缓存、DB 转发、过载及失败状态。比对正常和失败登录的响应状态及线格式。

- [ ] **BW-06-01-01 解析登录请求参数和版本**。**验收** 原版样本得到相同接受或拒绝。
- [ ] **BW-06-01-02 实现登录挑战协商**。**验收** 启用与禁用路径均能互通。
- [ ] **BW-06-01-03 缓存重复请求**。**验收** 重发得到一致结果。
- [ ] **BW-06-01-04 转发 DB 登录并处理超时**。**验收** 响应状态与原版匹配。
- [ ] **BW-06-01-05 返回过载和其他失败原因**。**验收** 错误码及负载一致。

#### BW-06-02 账号认证与重复登录

串通 LoginApp、DBApp billing、BaseAppMgr 和目标 BaseApp。覆盖认证拒绝、登录记录、重复登录/重登和账号实体选择。

- [ ] **BW-06-02-01 接通 Billing 账号认证**。**验收** 成功和拒绝均对照。
- [ ] **BW-06-02-02 选择账号实体并派发目标 BaseApp**。**验收** 正确账号绑定正确实体。
- [ ] **BW-06-02-03 维护在线登录记录**。**验收** 断开或拒绝不会遗留记录。
- [ ] **BW-06-02-04 处理重复登录与重登**。**验收** 既有会话和新会话终态吻合。

#### BW-06-03 BaseApp 接续与 Proxy

复现登录地址切换、pending login/session key、Channel 绑定、Proxy 与玩家实体绑定、断线/踢出/重新连接和会话清理。比较回调和状态顺序。

- [ ] **BW-06-03-01 发出 BaseApp 接续地址**。**验收** 客户端按原版顺序切换。
- [ ] **BW-06-03-02 绑定 pending login 与 session key**。**验收** 无效或过期票据被拒绝。
- [ ] **BW-06-03-03 绑定 Channel、Proxy 和玩家**。**验收** 登录完成序列吻合。
- [ ] **BW-06-03-04 实现主动断线和踢出**。**验收** 发送对应原因并清理状态。
- [ ] **BW-06-03-05 实现重连及会话回收**。**验收** 重试不会产生双 Proxy。

#### BW-06-04 客户端连接状态机

复现连接模型、服务端发现、切服、RPC/属性消息派发、时间同步、上传移动输入及下载流。用 headless 与原版客户端分别验证登录、进入世界、断开及重登。

- [ ] **BW-06-04-01 实现服务端发现**。**验收** 按夹具返回同一服务列表。
- [ ] **BW-06-04-02 实现连接和切服**。**验收** 状态机转移匹配。
- [ ] **BW-06-04-03 派发 RPC 与实体属性**。**验收** 回调数据及排序吻合。
- [ ] **BW-06-04-04 同步游戏时间并上报移动**。**验收** 时间偏移与输入字节一致。
- [ ] **BW-06-04-05 接入客户端下载流**。**验收** 接收分块和最终通知吻合。
- [ ] **BW-06-04-06 执行原版与 headless 客户端联调**。**验收** 登录、进世界、退出及重登均记录。

### BW-07 BaseApp 实体与服务

源码 `server/baseapp/`、`server/baseapp_extensions/`、`lib/appmgr/`。

#### BW-07-01 Base 实体创建与销毁

覆盖动态创建、从 DB 装载、ID 分配、初始化、定时器、驻留、销毁及失败回调。比对内存和持久化状态、重复回调及 ID 归还。

- [ ] **BW-07-01-01 动态创建 Base 并分配 ID**。**验收** 类型、初始化参数和回调吻合。
- [ ] **BW-07-01-02 按 DBID 装载 Base**。**验收** 已有与缺失记录分别符合原版。
- [ ] **BW-07-01-03 注册 Base 定时器**。**验收** 销毁时对应定时器解除。
- [ ] **BW-07-01-04 销毁驻留实体并归还 ID**。**验收** 重复销毁不造成重复回收。
- [ ] **BW-07-01-05 处理创建及装载失败**。**验收** 失败回调一次且状态可重试。

#### BW-07-02 Cell 配对与 MailBox

覆盖 `createCellEntity`、`cellData`、Cell 创建完成/丢失通知、跨进程方法调用、Mailbox 路由与迁移后重定位。

- [ ] **BW-07-02-01 构造并发送 cellData**。**验收** 目标 Cell 收到一致初始化负载。
- [ ] **BW-07-02-02 处理 Cell 创建成功与失败**。**验收** 回调及 Base 状态一致。
- [ ] **BW-07-02-03 处理 Cell 丢失通知**。**验收** Base 清退旧 Cell 引用。
- [ ] **BW-07-02-04 调用跨进程 Mailbox 方法**。**验收** 目标和响应与原版匹配。
- [ ] **BW-07-02-05 跟踪迁移后 Mailbox 地址**。**验收** 调用抵达新 Real 所在角色。

#### BW-07-03 全局实体、Service 与共享数据

复现 global bases、服务实例启动、注册和分发、SharedData 异步更新与删除。按原版部署方式验证跨 BaseApp/CellApp 顺序与恢复。

- [ ] **BW-07-03-01 实现 global bases 注册与查询**。**验收** 跨 BaseApp 读结果吻合。
- [ ] **BW-07-03-02 启动并注册 Service**。**验收** 相同配置产生相同可用服务。
- [ ] **BW-07-03-03 分发服务调用**。**验收** 多个实例中行为可对照。
- [ ] **BW-07-03-04 传播 SharedData 更新和删除**。**验收** 跨 Base/Cell 事件顺序以及恢复结果吻合。

#### BW-07-04 备份、接管和归档

移植 backup hash/发送、备份接收、offloaded backup、Archiver、CellApp 死亡处理和恢复。分别对照故障前、故障窗口和恢复后的实体/会话状态。

- [ ] **BW-07-04-01 计算 backup hash 与目标表**。**验收** 拓扑变化时映射可对照。
- [ ] **BW-07-04-02 传输并应用 Base 备份**。**验收** 接管后实体属性吻合。
- [ ] **BW-07-04-03 处理 offloaded backup**。**验收** 迁移后旧备份按基线清理。
- [ ] **BW-07-04-04 实现 Archiver 周期归档**。**验收** 重启可从持久化状态恢复。
- [ ] **BW-07-04-05 处理 CellApp 死亡**。**验收** Base 会话及实体终态与原版对照。

#### BW-07-05 下载与回放写入

覆盖客户端流式数据下载、回放头、压缩块、tick 写入和完成回调。与 Cell 采集及客户端回放读取联验。

- [ ] **BW-07-05-01 分块发送客户端资源**。**验收** 块顺序和完成回调吻合。
- [ ] **BW-07-05-02 写入回放头及元数据**。**验收** 原版读取器可解析。
- [ ] **BW-07-05-03 写入压缩 Tick 块**。**验收** 已知事件序列可无损读取。
- [ ] **BW-07-05-04 处理结束和失败**。**验收** 文件关闭与回调次数符合基线。

### BW-08 CellApp、Space、AOI 与实体迁移

源码 `server/cellapp/`、`server/cellapp_extensions/`、`lib/server/`；设计索引 [实体迁移](../BigWorld/docs/zh/architecture/entity-migration-offload.md)、[AOI/Witness/Ghost](../BigWorld/docs/zh/architecture/aoi-witness-ghost.md)。

#### BW-08-01 Cell/Space 生命周期

覆盖 Space 创建、拓扑变化、空间数据、物理空间后端及卸载、Cell 注册与销毁。对照多个 Space、空 Space 和 Cell 回收。

- [ ] **BW-08-01-01 创建和销毁 Space**。**验收** 空 Space 回收行为吻合。
- [ ] **BW-08-01-02 创建与注册 Cell**。**验收** 管理器和 CellApp 状态一致。
- [ ] **BW-08-01-03 传播空间数据和拓扑变化**。**验收** 多 Space 不串数据。
- [ ] **BW-08-01-04 挂载和卸载物理 Space**。**验收** 引用在销毁后释放。

#### BW-08-02 Real 实体与控制器

复现实体创建、Tick、Controller/Extra 挂接、移动/转向/加速、定时器、寻路、乘客、传送与销毁。比对轨迹、控制器持久状态和回调次序。

- [ ] **BW-08-02-01 创建 Real 实体并推进 Tick**。**验收** 脚本回调顺序吻合。
- [ ] **BW-08-02-02 挂接与清除 Controller/Extra**。**验收** 反复注册不会留存旧对象。
- [ ] **BW-08-02-03 移植移动、转向、加速控制**。**验收** 固定输入轨迹吻合。
- [ ] **BW-08-02-04 实现定时器与寻路控制器接续**。**验收** 退出或迁移后状态满足基线。
- [ ] **BW-08-02-05 处理乘客和传送**。**验收** 父子变换、脱离和目标位置吻合。
- [ ] **BW-08-02-06 销毁 Real**。**验收** 实体状态和活动控制器正确清理。

#### BW-08-03 AOI 和 Witness

移植 RangeList/RangeTrigger、Witness、EntityCache、优先级/带宽调度、进入/离开事件、位置和属性同步。覆盖半径边界、实体移动、自身实体及多个观察者。

- [ ] **BW-08-03-01 移植 RangeList 与触发边界**。**验收** 半径临界进入和离开事件吻合。
- [ ] **BW-08-03-02 管理 Witness 与 EntityCache**。**验收** 首次观察和重复进入结果一致。
- [ ] **BW-08-03-03 按带宽与优先级选择更新**。**验收** 固定负载下发送序列吻合。
- [ ] **BW-08-03-04 同步位置与属性**。**验收** 自身和其他观察者收到各自投影。
- [ ] **BW-08-03-05 复测多个观察者**。**验收** 互不污染缓存和预算。

#### BW-08-04 Ghost 复制和缓冲

覆盖 Ghost 建立、跟随和销毁、属性/位置复制、Real 地址追踪、消息缓冲和重放。验证邻 Cell 穿越期间客户端看到的实体序列。

- [ ] **BW-08-04-01 创建与销毁 Ghost**。**验收** 两相邻 Cell 保持正确数量。
- [ ] **BW-08-04-02 复制属性和位置**。**验收** Ghost 与对应 Real 快照吻合。
- [ ] **BW-08-04-03 跟踪 Real 地址**。**验收** 路由更新后抵达正确进程。
- [ ] **BW-08-04-04 缓冲与重放切换期消息**。**验收** 交付顺序与去重结果吻合。

#### BW-08-05 Offload、跨区传送和故障

移植迁移判定、Real/Ghost 角色转换、切换时重定向、控制器和回调状态迁移、CellApp 死亡处理。验证两 Cell 双向穿越、迁移中消息和目标失联。

- [ ] **BW-08-05-01 实现 Offload 判定**。**验收** 越过测试边界时按阈值迁移。
- [ ] **BW-08-05-02 切换 Real/Ghost 角色**。**验收** 两端任意观测点可定位唯一 Real。
- [ ] **BW-08-05-03 转移控制器及回调状态**。**验收** 移动过程连续。
- [ ] **BW-08-05-04 重定向切换期间消息**。**验收** 调用到达迁移目标且响应一次。
- [ ] **BW-08-05-05 实现跨 Space 传送**。**验收** 新旧 Space 的可见集吻合。
- [ ] **BW-08-05-06 验证迁移目标失联和恢复**。**验收** 角色及消息状态对照通过。

#### BW-08-06 感知、观测和回放采集

覆盖 vision/proximity/noise、cell viewer、负载节流、回放事件采集和 tick 边界，并比对掉帧或资源慢加载时的状态。

- [ ] **BW-08-06-01 实现 proximity/vision 触发**。**验收** 进出区域和遮挡回调吻合。
- [ ] **BW-08-06-02 实现 noise 感知**。**验收** 不同半径与距离的触发对照。
- [ ] **BW-08-06-03 提供 Cell Viewer 查询**。**验收** 与实时 Space/Cell 状态匹配。
- [ ] **BW-08-06-04 实现负载节流**。**验收** 慢加载及超预算 Tick 有可对照结果。
- [ ] **BW-08-06-05 记录回放事件**。**验收** Tick 边界与事件排序一致。

### BW-09 DBApp、持久化和数据库运维

源码 `server/dbapp/`、`server/dbapp_extensions/`、`lib/{db,db_storage,db_storage_mysql,db_storage_xml,sqlite}/`、`server/tools/{sync_db,consolidate_dbs,transfer_db,snapshot_helper,clear_auto_load,remove_db}/`。

#### BW-09-01 DBApp 实体操作

复现 get/load/write/delete/lookup、DBID 分配、在线实体记录、按类型查找和回调。对照写入、调用重试、并发登录与重复载入。

- [ ] **BW-09-01-01 实现 get/load**。**验收** 存在和缺失 DBID 的返回匹配。
- [ ] **BW-09-01-02 实现 write**。**验收** 创建与更新可从独立连接复读。
- [ ] **BW-09-01-03 实现 delete**。**验收** 删除后的查询和重复删除结果匹配。
- [ ] **BW-09-01-04 实现 lookup**。**验收** 按名称或类型返回对应记录。
- [ ] **BW-09-01-05 分配 DBID 并维护在线状态**。**验收** 并发登录无重复占用。
- [ ] **BW-09-01-06 对照重试回调**。**验收** 响应次数和错误与原版一致。

#### BW-09-02 MySQL 持久化

还原属性到表和列的映射、字节与字符集、账户表、自定义类型、事务、锁、后台任务和同实体操作顺序。既有数据库可读写，比较 SQL 结构、行内容及事务提交/回滚结果。

- [ ] **BW-09-02-01 映射实体表、列与索引**。**验收** 与旧 MySQL schema 对照。
- [ ] **BW-09-02-02 映射账号及自定义类型**。**验收** 原版数据库行可读写。
- [ ] **BW-09-02-03 验证字节与字符集**。**验收** Unicode 和二进制往返无损。
- [ ] **BW-09-02-04 实现事务提交与回滚**。**验收** 失败写入无部分状态。
- [ ] **BW-09-02-05 串行化同实体后台任务**。**验收** 并发请求最终顺序吻合。
- [ ] **BW-09-02-06 校验锁冲突与连接恢复**。**验收** 结果可重试且状态一致。

#### BW-09-03 XML 后端和 SQLite 二级库

覆盖 XML 存储、BaseApp 二级库、归档整合、auto-load 和重启恢复。用原版生成的数据执行导入、读写、合并和恢复。

- [ ] **BW-09-03-01 实现 XML 持久化**。**验收** 原版 XML 可读写。
- [ ] **BW-09-03-02 读取原版 SQLite 二级库**。**验收** 已知记录无损导入。
- [ ] **BW-09-03-03 合并归档**。**验收** 同实体版本取舍符合基线。
- [ ] **BW-09-03-04 恢复 auto-load 实体**。**验收** 重启后实体集合吻合。

#### BW-09-04 Billing 与扩展

移植标准、Python、自定义和实际参编的其他认证后端及扩展入口。对照成功、失效、重复请求和依赖不可用时的错误状态。

- [ ] **BW-09-04-01 移植标准认证**。**验收** 合法和无效账号状态吻合。
- [ ] **BW-09-04-02 接入 Python Billing**。**验收** 接口及异常与原脚本契约一致。
- [ ] **BW-09-04-03 接入已参编的自定义扩展**。**验收** 按目标验证可用与依赖缺失。
- [ ] **BW-09-04-04 验证重复认证**。**验收** 并发请求不污染登录记录。

#### BW-09-05 运维工具

复现建表和同步、dry-run、快照、传输、二级库合并、清理 auto-load 及移除辅助工具。比较命令行、退出码、输出和操作前后数据。

- [ ] **BW-09-05-01 移植 sync_db**。**验收** 原版 schema 上的 DDL 差异可复核。
- [ ] **BW-09-05-02 移植 dry-run**。**验收** 报告计划且数据库前后相同。
- [ ] **BW-09-05-03 移植快照工具**。**验收** 原版读者可读取结果。
- [ ] **BW-09-05-04 移植 transfer_db**。**验收** 目标数据和退出码吻合。
- [ ] **BW-09-05-05 移植二级库合并工具**。**验收** 原版输入合并结果吻合。
- [ ] **BW-09-05-06 移植 clear_auto_load 和 remove_db 入口**。**验收** 用隔离样本校验各自操作及退出码。

#### BW-09-06 PostgreSQL 持久化

作为 Boundless 新增的 DBApp 主存储后端，与 MySQL、XML 共用存储接口，通过配置选择。实现实体/账号字段映射、DBID 分配、索引与约束、事务、并发锁和连接恢复。复用存储契约测试，覆盖创建、装载、更新、删除、账号重登、auto-load 和二级库归档合并。以原版 MySQL 路径的实体状态、错误语义与回调结果作为行为基准。

- [ ] **BW-09-06-01 配置 PostgreSQL 主存储选择**。**验收** DBApp 启动时报告所用后端。
- [ ] **BW-09-06-02 映射实体 schema 与字段**。**验收** Unicode、二进制和嵌套类型可往返。
- [ ] **BW-09-06-03 映射账号表和唯一约束**。**验收** 重复账号产生合同错误。
- [ ] **BW-09-06-04 实现 DBID 分配**。**验收** 并发创建无重复且重启连续可用。
- [ ] **BW-09-06-05 实现实体读写删查**。**验收** 共用存储契约用例通过。
- [ ] **BW-09-06-06 提交 PostgreSQL 事务**。**验收** 已提交的实体更新可从另一连接读回。
- [ ] **BW-09-06-07 回滚 PostgreSQL 事务**。**验收** 写入中途返回错误时，实体和关联记录从独立连接读出旧值。
- [ ] **BW-09-06-08 处理 PostgreSQL 并发写冲突**。**验收** 两个写入同一实体的请求按存储契约返回结果，重试后没有重复记录。
- [ ] **BW-09-06-09 实现断链恢复**。**验收** 数据库恢复后新请求成功。
- [ ] **BW-09-06-10 验证账号重登**。**验收** 会话状态与 MySQL 基准相符。
- [ ] **BW-09-06-11 对接 auto-load 与 SQLite 归档**。**验收** 重启恢复对应实体。

#### BW-09-07 PostgreSQL 数据库运维

提供建库、schema 同步与 dry-run、备份恢复和现有 MySQL 数据导入。用原版样本核对实体 ID、账号关联、二进制字段、默认值和下一个可分配 DBID。验证迁移失败时的状态和重试行为。

- [ ] **BW-09-07-01 初始化 PostgreSQL 新库**。**验收** 从空库生成可启动 schema。
- [ ] **BW-09-07-02 提供 schema dry-run**。**验收** 数据库不变且报告拟执行语句。
- [ ] **BW-09-07-03 应用 schema 更新**。**验收** 旧样本升级后可读。
- [ ] **BW-09-07-04 导出并恢复备份**。**验收** 空目标库恢复相同记录。
- [ ] **BW-09-07-05 导入 MySQL 实体及账号**。**验收** ID、关系和二进制字段对照。
- [ ] **BW-09-07-06 校准导入后的 DBID 分配**。**验收** 新建实体不冲突。
- [ ] **BW-09-07-07 验证迁移失败与重跑**。**验收** 下次执行不重复导入。

### BW-10 世界资源、碰撞和导航

源码 `lib/{chunk,chunk_loading,chunk_scene_adapter,compiled_space,space,scene,terrain,physics2,waypoint,waypoint_generator,navigation_recast,delegate_interface}/`、`common/{py_chunk.cpp,py_terrain.cpp,py_physics2.cpp}`。

#### BW-10-01 Chunk 和空间加载

复现室内外 Chunk、边界/Portal、映射、装载和卸载、空间索引及场景适配。按加载轨迹比对绑定状态和对象集合。

- [ ] **BW-10-01-01 解析室内外 Chunk 标识和坐标**。**验收** 同图对象集合吻合。
- [ ] **BW-10-01-02 解析 Portal 和 Chunk 边界**。**验收** 相邻关系与原版一致。
- [ ] **BW-10-01-03 装载相邻 Chunk**。**验收** 流式进出时绑定序列吻合。
- [ ] **BW-10-01-04 卸载 Chunk 及场景对象**。**验收** 查询结果和资源释放符合基线。
- [ ] **BW-10-01-05 实现空间索引**。**验收** 跨 Chunk 与空白区查询返回正确集合。

#### BW-10-02 编译空间和场景查询

覆盖 compiled space 读取、渲染/查询对象注册、静态和动态场景、委托接口及实体空间映射。比对由原版转换器产出的资源读入结果。

- [ ] **BW-10-02-01 解析 compiled space 格式**。**验收** 读入原版转换器生成的样本。
- [ ] **BW-10-02-02 注册静态场景对象**。**验收** 固定空间查询返回同一对象集合。
- [ ] **BW-10-02-03 注册动态对象**。**验收** 移动和移除后查询结果匹配。
- [ ] **BW-10-02-04 实现委托接口与实体映射**。**验收** 实体可定位对应场景对象。
- [ ] **BW-10-02-05 验证渲染与查询投影**。**验收** 同一编译空间两种消费者均可加载。

#### BW-10-03 地形与物理

覆盖高度、洞、材质、碰撞三角形、BSP、空间树、射线/扫掠及 Portal 状态。对照边界点、重叠面、不同材质和坐标系。

- [ ] **BW-10-03-01 读取 Terrain2 高度图**。**验收** 格点和插值坐标吻合。
- [ ] **BW-10-03-02 读取地形洞**。**验收** 洞内外查询对照。
- [ ] **BW-10-03-03 查询主材质和碰撞标志**。**验收** 同一点返回相同标识。
- [ ] **BW-10-03-04 读取 BSP 与碰撞三角形**。**验收** 原版场景可查询。
- [ ] **BW-10-03-05 实现射线命中**。**验收** 首个交点、法向和材质对照。
- [ ] **BW-10-03-06 实现扫掠碰撞**。**验收** 重叠面和边界条件吻合。
- [ ] **BW-10-03-07 处理 Portal 通行与坐标变换**。**验收** 开关前后结果与基线一致。

#### BW-10-04 Waypoint 寻路运行时

对齐 `lib/waypoint/` 的 NavLoc 定位、waypoint set 邻接、按 girth 选路、局部与跨 Chunk 搜索、路径缓存及 Portal 限制。用相同资源比较路径点、不可达、搜索距离上限和缓存失效。

- [ ] **BW-10-04-01 解码 waypoint set 和邻接**。**验收** 旧图节点与连接正确恢复。
- [ ] **BW-10-04-02 实现 NavLoc 定位与投影**。**验收** 多层位置和边界与原版一致。
- [ ] **BW-10-04-03 按 girth 选择路径**。**验收** 窄门对不同角色返回正确可达性。
- [ ] **BW-10-04-04 实现 Chunk 内搜索**。**验收** 固定起终点路径点逐点比对。
- [ ] **BW-10-04-05 实现跨 Chunk waypoint set 搜索**。**验收** 同图 Portal 访问顺序吻合。
- [ ] **BW-10-04-06 返回 waypoint 不可达结果**。**验收** 封闭 Portal 后，同一对起终点返回与原版一致的失败结果。
- [ ] **BW-10-04-07 限制 waypoint 搜索距离**。**验收** 在原版阈值附近分别验证可达和超限结果。
- [ ] **BW-10-04-08 实现路径缓存失效**。**验收** Portal 或资源变化后重新选路。

#### BW-10-05 NavGen 与导航资源生成

分别追踪 `lib/waypoint_generator/` 的 flood/generator、`lib/navigation_recast/recast_generator.cpp` 及 `tools/navgen/` 的启用条件和输出。覆盖地形、斜坡、室内外接缝和不同 girth，验证旧资源读取和新版生成物往返。原版 NavGen 有 Windows 图形入口。

- [ ] **BW-10-05-01 核查 waypoint flood 与 Recast 的构建入口**。**验收** 记录算法、格式及启用条件。
- [ ] **BW-10-05-02 移植 flood 生成**。**验收** 斜坡和室内外交界产物可对照。
- [ ] **BW-10-05-03 移植选用的 Recast 生成**。**验收** 固定输入生成可读导航资源。
- [ ] **BW-10-05-04 实现原版 NavGen 图形工作流**。**验收** 同图可选择目标、生成和保存。
- [ ] **BW-10-05-05 验证旧导航资产读取**。**验收** 固定地图可寻路。
- [ ] **BW-10-05-06 验证新产物往返**。**验收** 由支持该格式的原版目标重载。

#### BW-10-06 CellApp 与 Python 寻路

沿 `entity_navigate.cpp`、`navmesh_navigation_system.cpp`、`navigation_controller.cpp` 对齐 `navigate`、`navigateStep`、`canNavigateTo`、`navigateFollow`、`navigatePathPoints`、`getStopPoint`、`waySetPathLength` 与移动/加速 API。核对轨迹、控制器取消、成功/失败回调和跨 Cell 迁移后的状态。

- [ ] **BW-10-06-01 实现 navigate**。**验收** 固定路线到达时触发对应回调。
- [ ] **BW-10-06-02 实现 navigateStep**。**验收** 单步目标、返回值与越界行为吻合。
- [ ] **BW-10-06-03 实现 canNavigateTo**。**验收** 可达与不可达样本结果吻合。
- [ ] **BW-10-06-04 实现 navigateFollow**。**验收** 目标移动后跟随轨迹对照。
- [ ] **BW-10-06-05 实现 navigatePathPoints**。**验收** 成功路径与无路径错误对照。
- [ ] **BW-10-06-06 实现 getStopPoint**。**验收** 同一位置、方向和停止距离输入返回原版停止点。
- [ ] **BW-10-06-07 实现 waySetPathLength**。**验收** 跨两个 waypoint set 的路径长度返回值与原版一致。
- [ ] **BW-10-06-08 实现 moveToPoint**。**验收** 逐 tick 对照位置、终点容差和到达回调。
- [ ] **BW-10-06-09 实现 moveToEntity**。**验收** 目标实体移动时，跟随结果和停止条件与原版一致。
- [ ] **BW-10-06-10 实现 accelerateToPoint**。**验收** 逐 tick 比较速度、位置和到达结果。
- [ ] **BW-10-06-11 实现 accelerateAlongPath**。**验收** 多段路径拐点的减速与终点行为与原版一致。
- [ ] **BW-10-06-12 实现 accelerateToEntity**。**验收** 目标移动及销毁时的控制器结果与原版一致。
- [ ] **BW-10-06-13 取消活动寻路控制器**。**验收** 取消后下一 tick 停止推进，回调序列与原版一致。
- [ ] **BW-10-06-14 迁移活动寻路控制器**。**验收** 移动中穿越 Cell 边界后，目标位置和回调次数与原版一致。

#### BW-10-07 地图端到端验收

以带地形高低差、障碍、Portal 和两个 Chunk 的固定地图，覆盖编辑与生成、服务端加载、客户端查询、Bots 穿越和导航调试显示。对照服务端坐标、路径、地形洞与碰撞结果。绑定已启用的原版目标逐平台验收。

- [ ] **BW-10-07-01 固定测试地图和期望值**。**验收** 含坡、洞、障碍和两个相邻 Chunk。
- [ ] **BW-10-07-02 执行地图编辑和生成**。**验收** 新版导出后可被基准目标消费。
- [ ] **BW-10-07-03 从服务端加载该地图**。**验收** 坐标、地形和 Portal 状态吻合。
- [ ] **BW-10-07-04 从客户端查询该地图**。**验收** 固定射线及导航显示吻合。
- [ ] **BW-10-07-05 执行 Bot 穿越**。**验收** 记录两 Cell 的移动和进入离开事件。
- [ ] **BW-10-07-06 运行平台验收矩阵**。**验收** 每个适用原版目标有结果及证据。

### BW-11 客户端、渲染与交互

源码 `client/`、`lib/{moo,model,duplo,romp,camera,input,ashes,particle,post_processing,fmodsound,speedtree,scaleform,web_render,emptyvoip,open_automate}/`。

#### BW-11-01 应用生命周期与输入

移植窗口、设备初始化、帧阶段、输入、设置、相机、帧率/LOD 调节和调试视图。复测窗口/全屏、焦点切换、退出及设备重建。

- [ ] **BW-11-01-01 实现窗口创建、全屏与退出**。**验收** 三种切换无设备泄漏。
- [ ] **BW-11-01-02 初始化与重建设备**。**验收** 重建后测试场景可继续出帧。
- [ ] **BW-11-01-03 对齐帧阶段**。**验收** 更新、绘制及脚本回调顺序吻合。
- [ ] **BW-11-01-04 接入输入及焦点**。**验收** 失焦与恢复时状态符合基线。
- [ ] **BW-11-01-05 应用画面设置与 LOD**。**验收** 固定阈值下发生相同切换。
- [ ] **BW-11-01-06 接入相机和调试视图**。**验收** 固定操作下视角一致。

#### BW-11-02 客户端实体与运动

覆盖 Player、客户端独有实体、UDO 工厂、实体选择、移动滤波、预测和修正、场景实体绑定。回放固定服务端输入，对照轨迹和 Python 回调。

- [ ] **BW-11-02-01 切换 Player 实体**。**验收** 脚本事件顺序吻合。
- [ ] **BW-11-02-02 创建本地实体和 UDO**。**验收** 原版脚本构造路径成功。
- [ ] **BW-11-02-03 实现实体选择与场景绑定**。**验收** 选择命中及解绑行为一致。
- [ ] **BW-11-02-04 移植移动滤波**。**验收** 输入样本的轨迹吻合。
- [ ] **BW-11-02-05 应用服务端预测修正**。**验收** 位置收敛与回调符合基线。

#### BW-11-03 回放与资源下载

用原版录像校验头/元数据/压缩块/checksum 及 tick 读取，覆盖播放、跳转、结束和下载分片。新版产物交回原版支持的读取入口验收。

- [ ] **BW-11-03-01 解析回放头及元数据**。**验收** 已知原版录像读出相同字段。
- [ ] **BW-11-03-02 解码压缩块和值校验**。**验收** 正确帧可读取且坏块有对应错误。
- [ ] **BW-11-03-03 按 Tick 播放与跳转**。**验收** 目标 Tick 的实体状态吻合。
- [ ] **BW-11-03-04 处理回放结束**。**验收** 只发一次完成事件。
- [ ] **BW-11-03-05 重组下载分片**。**验收** 内容及回调顺序吻合。

#### BW-11-04 渲染和世界表现

迁移 Moo 的模型/骨骼动画、材质/纹理/特效、光照、地形渲染、环境、植被、粒子、后处理及相机。逐项核对资产格式、渲染状态与固定镜头截图。记录材质转换和视觉容差，分别验收三端。

- [ ] **BW-11-04-01 加载模型和骨骼动画**。**验收** 原版资产关节姿态吻合。
- [ ] **BW-11-04-02 绑定材质、纹理与特效**。**验收** 固定材质样图符合视觉容差。
- [ ] **BW-11-04-03 渲染光照与阴影**。**验收** 固定场景对照。
- [ ] **BW-11-04-04 渲染地形与环境**。**验收** 固定镜头对照。
- [ ] **BW-11-04-05 渲染植被和粒子**。**验收** 固定种子序列可重放。
- [ ] **BW-11-04-06 移植后处理与相机效果**。**验收** 启用与关闭模式均有对照。
- [ ] **BW-11-04-07 验证各端图形后端**。**验收** 三端生成可对比截图与状态。

#### BW-11-05 GUI、音频和集成

覆盖 Python GUI、界面事件、迷你地图、声音、VOIP、网页嵌入及 SDK 条件功能。以原版样本比较接口和行为，给缺 SDK 的功能保留兼容待办。

- [ ] **BW-11-05-01 复现 Python GUI 与交互事件**。**验收** 回调顺序及参数吻合。
- [ ] **BW-11-05-02 实现迷你地图**。**验收** 地图投影及标记位置正确。
- [ ] **BW-11-05-03 接入声音**。**验收** 控制与播放事件对照。
- [ ] **BW-11-05-04 检查 VOIP 分支**。**验收** 启用目标通过契约或登记缺 SDK。
- [ ] **BW-11-05-05 接入网页嵌入**。**验收** 原版可用工作流有对照。
- [ ] **BW-11-05-06 核查其他条件集成**。**验收** 每个有效目标有平台和证据。

### BW-12 观测、运维接口和测试客户端

源码 `lib/{cstdmf,network,server,testing,unit_test_lib}/`、`server/tools/{message_logger,bw_profile,bots}/`。

#### BW-12-01 Watcher

复现路径树、类型信息、读/写/调用、远程请求、转发和统计字段。比较原版监控工具的查询、返回值、权限和错误。

- [ ] **BW-12-01-01 构建 Watcher 路径树及类型**。**验收** 同路径的值与类型匹配。
- [ ] **BW-12-01-02 移植 get/set**。**验收** 读写结果和拒绝状态吻合。
- [ ] **BW-12-01-03 移植 callable Watcher**。**验收** 参数和返回值匹配。
- [ ] **BW-12-01-04 实现远端查询与转发**。**验收** 多节点同一路径正确路由。
- [ ] **BW-12-01-05 核对统计字段**。**验收** 稳定快照和错误状态与原版一致。

#### BW-12-02 MessageLogger

移植日志接入协议、组件注册、字段和查询接口、文件日志数据库及条件启用的 MongoDB 后端。用相同事件比较分段、轮转和查询结果。

- [ ] **BW-12-02-01 接入日志协议与组件注册**。**验收** 原版组件能提交固定事件。
- [ ] **BW-12-02-02 实现日志字段和查询**。**验收** 筛选与排序吻合。
- [ ] **BW-12-02-03 实现文件日志和分段轮转**。**验收** 读入原版样本且输出对照。
- [ ] **BW-12-02-04 接入条件 MongoDB 后端**。**验收** 启用构建通过写查夹具。

#### BW-12-03 Profiler 与故障注入兼容入口

覆盖原版 profiler 帧和计数器、`_bw_profile` Python 模块及输出、网络模拟接口和可控时间。内部采集与性能分析交付见 BW-23，测试夹具与故障注入框架见 BW-18。

- [ ] **BW-12-03-01 移植帧和计数器兼容层**。**验收** 同负载下关键字段与原版吻合。
- [ ] **BW-12-03-02 复现 _bw_profile 启停接口**。**验收** 导入、状态和回调对照。
- [ ] **BW-12-03-03 复现原版 JSON dump**。**验收** 固定事件产物兼容。
- [ ] **BW-12-03-04 复核原版网络模拟及可控时间入口**。**验收** 测试夹具可调用。

#### BW-12-04 Bots

复现配置、批量创建、登录、退出、运动脚本及指标。跑登录潮、稳定移动和集群重启，记录可重复的负载和差分结果。

- [ ] **BW-12-04-01 读取原版 Bots 配置**。**验收** 参数默认值和失败状态吻合。
- [ ] **BW-12-04-02 移植批量添加与移除**。**验收** 同指令产生相同活跃集合。
- [ ] **BW-12-04-03 移植 Bots 登出及脚本控制**。**验收** 事件和运动轨迹吻合。
- [ ] **BW-12-04-04 回归原版 Bots 指标**。**验收** 登录、稳态和重启均有基线。

### BW-13 资源管线、编辑器与导出器

源码 `tools/`、`lib/{asset_pipeline,controls,gizmo,guimanager,guitabs,ual,visual_manipulator}/`。

#### BW-13-01 资产编译管线

移植 dependency/discovery/cache、batch/JIT compiler 和转换器注册。逐类验证 space、visual、primitive、texture、effect、BSP、配置输入及增量重跑的产物。

- [ ] **BW-13-01-01 实现资产发现和依赖图**。**验收** 修改选定输入只重排对应目标。
- [ ] **BW-13-01-02 实现增量缓存**。**验收** 第二次同输入可复用。
- [ ] **BW-13-01-03 移植 batch compiler**。**验收** 按目标生成原版可读文件。
- [ ] **BW-13-01-04 移植 jit compiler 与转换器注册**。**验收** 按需生成且可查转换器。
- [ ] **BW-13-01-05 逐类验证 space/visual/primitive 转换**。**验收** 各有一份往返样本。
- [ ] **BW-13-01-06 逐类验证 texture/effect/BSP/配置转换**。**验收** 各有一份往返样本。

#### BW-13-02 离线工具

覆盖 `assetprocessor`、`offline_processor`、`res_packer`、`resourcechecker`、`navgen`。按原版构建入口验证命令行或图形工作流、错误和可被原版读取的产物。导航生成与运行时联验见 BW-10-05/07。

- [ ] **BW-13-02-01 移植 assetprocessor 工作流**。**验收** 标准样本输出和错误可比。
- [ ] **BW-13-02-02 移植 offline_processor 工作流**。**验收** 输出可被原版消费。
- [ ] **BW-13-02-03 移植 res_packer**。**验收** 打包前后资源可等价读回。
- [ ] **BW-13-02-04 移植 resourcechecker**。**验收** 同一坏资源得到对应诊断。
- [ ] **BW-13-02-05 回归 NavGen 入口**。**验收** 可调用 BW-10 的导航生成。

#### BW-13-03 WorldEditor

复现地图打开/保存、Chunk/Portal/地形编辑、对象放置、选择和变换、属性编辑、撤销重做、资源浏览和空间发布，用同一地图跑编辑后往返测试。

- [ ] **BW-13-03-01 实现地图打开与保存**。**验收** 旧图编辑后原版可重载。
- [ ] **BW-13-03-02 实现 Chunk 与 Portal 编辑**。**验收** 修改后相邻关系对照。
- [ ] **BW-13-03-03 实现地形编辑**。**验收** 保存后高度、洞和材质吻合。
- [ ] **BW-13-03-04 实现对象放置与变换**。**验收** 坐标、属性和选中事件对照。
- [ ] **BW-13-03-05 实现撤销与重做**。**验收** 前后对象快照均吻合。
- [ ] **BW-13-03-06 实现资源浏览与空间发布**。**验收** 发布物可启动测试空间。

#### BW-13-04 ModelEditor 和 ParticleEditor

覆盖模型、材质、动画检查和保存，粒子创建、预览和序列化。以旧资源编辑、保存，再由原版打开验收。

- [ ] **BW-13-04-01 实现模型打开及保存**。**验收** 新版保存的旧模型可由原版读取。
- [ ] **BW-13-04-02 实现材质编辑**。**验收** 导出字段及渲染样图可比。
- [ ] **BW-13-04-03 实现动画检查**。**验收** 固定时间的骨骼姿态匹配。
- [ ] **BW-13-04-04 实现粒子编辑及预览**。**验收** 固定种子的可见行为吻合。
- [ ] **BW-13-04-05 保存粒子资源**。**验收** 旧编辑器可重载。

#### BW-13-05 DCC 与外部集成

盘点 Max 动画/模型、Maya 模型导出、`bw_world_machine_import`、`bwlauncher` 和插件系统的实际目标。验证导出语义和文件，单独列宿主及 SDK 的平台矩阵。

- [ ] **BW-13-05-01 验证 Max 模型导出**。**验收** 同输入模型文件可读。
- [ ] **BW-13-05-02 验证 Max 动画导出**。**验收** 骨骼与时间关键帧一致。
- [ ] **BW-13-05-03 验证 Maya 模型导出**。**验收** 同场景导出的资产可读。
- [ ] **BW-13-05-04 验证 World Machine 导入**。**验收** 地形尺寸、采样和材质吻合。
- [ ] **BW-13-05-05 验证 launcher**。**验收** 按配置启动同一产品入口。
- [ ] **BW-13-05-06 核实插件宿主 SDK**。**验收** 每个启用组合有样本或缺口。

#### BW-13-06 原版编辑器公共库与调试资产

将 `worldeditor_ui`、`modeleditor_core`、`editor_shared`、`exporter_common`、`tools/common`、`plugin_system` 逐一对应到编辑器、导出器或资源管线。核对 `tools/visual_studio/visualizers` 的类型视图，验证 Rust 调试环境的等价字段展示。

- [ ] **BW-13-06-01 映射 worldeditor_ui 和 modeleditor_core**。**验收** 公共编辑接口有明确归属。
- [ ] **BW-13-06-02 映射 editor_shared、exporter_common 与 tools/common**。**验收** 共享代码依赖可追溯。
- [ ] **BW-13-06-03 核对 plugin_system**。**验收** 加载成功与失败行为可复现。
- [ ] **BW-13-06-04 迁移调试字段视图**。**验收** Rust 实体等价字段可在 IDE 中展开。

### BW-14 FantasyDemo、SDK 示例与发行

源码 `../fantasydemo/`、`examples/`、`scripts/generate-fantasydemo-assets.py`、仓库根 `scripts/package-daily*` 和发行物。

#### BW-14-01 Demo 脚本和数据

复用已跟踪的 Account、Avatar、Guard、Space、ClientOnly、实体定义和 server 配置。比对登录、创建角色、进入场景、移动、AOI、NPC、退出后重登。登记生成资源和完整资源包的来源、版本及可验证路径。

- [ ] **BW-14-01-01 加载原版实体定义与配置**。**验收** 五类示例定义摘要吻合。
- [ ] **BW-14-01-02 运行账号登录及角色创建**。**验收** 回调与持久化状态吻合。
- [ ] **BW-14-01-03 运行入场、移动和 AOI**。**验收** 同一轨迹的观察事件可比。
- [ ] **BW-14-01-04 运行 Guard 和 Space 脚本**。**验收** 行为与基准相符。
- [ ] **BW-14-01-05 验证退出重登**。**验收** 读回此前角色位置及属性。
- [ ] **BW-14-01-06 登记资源生成与完整包**。**验收** 每个样本可追溯至来源。

#### BW-14-02 SDK 和扩展样例

将 Python simple client、Android/iOS 客户端和 CellApp Controller/Extra 扩展示例纳入 `examples/sdk/` 兼容验证。SDK 实现与游戏引擎适配由 BW-21 交付。

- [ ] **BW-14-02-01 运行 Python simple 示例**。**验收** 与新服务端完成登录。
- [ ] **BW-14-02-02 运行原版 Android 示例互通测试**。**验收** 保存协议结果或宿主缺口。
- [ ] **BW-14-02-03 运行原版 iOS 示例互通测试**。**验收** 保存协议结果或宿主缺口。
- [ ] **BW-14-02-04 移植 Controller/Extra 样例**。**验收** 注册、回调及卸载可验。

#### BW-14-03 Web Integration

盘点 FantasyDemo PHP 页面、BigWorld Web 接口及 RPM 可核实的交付内容，以 Python 实现已确认的登录、角色、物品和拍卖路径，比较会话、数据和错误语义。

- [ ] **BW-14-03-01 核对 PHP 页面与 RPM**。**验收** 列出能够验证的接口和页面。
- [ ] **BW-14-03-02 移植登录与会话**。**验收** 同样的会话状态可继续访问。
- [ ] **BW-14-03-03 移植角色与物品查询**。**验收** 样本列表和错误语义一致。
- [ ] **BW-14-03-04 移植拍卖工作流**。**验收** 提交和查询状态匹配。

#### BW-14-04 可安装发布物

为三端分别提供运行时、Python 模块/标准库、配置样例、工具与资源安装布局。比对公开配置和 CLI，测试干净安装、升级保留数据、启动与卸载。

- [ ] **BW-14-04-01 布置原生二进制和 CLI**。**验收** 安装后的命令可执行。
- [ ] **BW-14-04-02 随包交付 CPython 模块与标准库**。**验收** 无开发环境可导入。
- [ ] **BW-14-04-03 布置配置及样例资源**。**验收** 首次启动可定位。
- [ ] **BW-14-04-04 执行干净安装烟测**。**验收** 三端能启动和停止。
- [ ] **BW-14-04-05 执行升级与卸载**。**验收** 验证用户数据保留行为。

### BW-15 Linux、Windows、macOS 原生支持

新版原生支持三端。暂定首批构建目标为 Linux x86-64、Windows x86-64、macOS arm64。其余架构按平台矩阵列项。核心角色和工具使用同一协议、资源、脚本及数据库契约。平台专属代码集中封装。原版可运行两端提供基准，macOS 等没有同端原版时以可追溯的原版样本和跨平台混跑验收。

#### BW-15-01 平台接口

封装事件轮询和唤醒、单调时钟、信号/控制事件、子进程与退出状态、文件锁、内存映射、动态库和系统目录。对照大小写、路径编码、原子替换、IPv4/IPv6 和断网行为。

- [ ] **BW-15-01-01 封装事件轮询和唤醒**。**验收** 三端可停止空闲循环。
- [x] **BW-15-01-02 封装单调时钟**。**验收** 单调性及 Tick 单位一致。
  - 证据：crates/platform clock.rs 测试（monotonic 不回退、微秒单位、起点近零）。
- [ ] **BW-15-01-03 封装进程控制及退出码**。**验收** 三端可正常关服并回收子进程。
- [ ] **BW-15-01-04 封装文件锁及原子替换**。**验收** 并发更新夹具通过。
- [ ] **BW-15-01-05 封装内存映射与共享库查找**。**验收** 三端加载相同测试资源。
- [ ] **BW-15-01-06 校验路径大小写、编码和系统目录**。**验收** 代表性夹具可寻址。
- [ ] **BW-15-01-07 校验网络地址与断网行为**。**验收** IPv4/IPv6 和失联状态可复测。

#### BW-15-02 Linux

验证原生服务端全角色、Bots、编辑器/客户端、资产工具及发布包。保留旧 EL7 互通基准，另记录 Boundless 自身的发行版及系统库要求。

- [ ] **BW-15-02-01 跑 Linux 服务端全角色**。**验收** 拓扑能够启动和关闭。
- [ ] **BW-15-02-02 跑 Linux Bots 与离线工具**。**验收** 样例输出和负载报告有效。
- [ ] **BW-15-02-03 跑 Linux 客户端和编辑器**。**验收** 固定地图可显示并编辑。
- [ ] **BW-15-02-04 验证 Linux 安装包**。**验收** 干净系统可安装和启动。
- [ ] **BW-15-02-05 验证旧 EL7 互通**。**验收** 记录该基线配置和结果。

#### BW-15-03 Windows

验证原生服务端全角色、客户端、编辑器及产物。处理进程停止、DLL/CPython 查找、路径与驱动，并核对原版 Windows 客户端和编辑器样本。

- [ ] **BW-15-03-01 跑 Windows 服务端全角色**。**验收** 能处理控制事件并关服。
- [ ] **BW-15-03-02 验证 DLL 和 CPython 发现**。**验收** 已安装布局中模块可导入。
- [ ] **BW-15-03-03 跑 Windows 客户端与编辑器**。**验收** 固定样本与原版对照。
- [ ] **BW-15-03-04 跑 Windows 工具和发布包**。**验收** 路径编码和启动烟测通过。

#### BW-15-04 macOS

验证原生服务端全角色、客户端、编辑器及产物。验证 CPython 加载、应用资源路径、图形设备行为和退出回收，并与 Linux/Windows 服务端互通。

- [ ] **BW-15-04-01 跑 macOS 服务端全角色**。**验收** 能启动、停止并回收。
- [ ] **BW-15-04-02 验证 CPython 与应用资源布局**。**验收** 从应用包可加载。
- [ ] **BW-15-04-03 跑 macOS 客户端和编辑器**。**验收** 设备状态和固定地图测试通过。
- [ ] **BW-15-04-04 跑 macOS 工具和发布包**。**验收** 干净安装后可执行。
- [ ] **BW-15-04-05 验证与 Linux 和 Windows 互通**。**验收** 跨端登录及实体同步通过。

#### BW-15-05 三端 CI 和混合部署

三个原生 runner 分别构建、跑协议/资源/Python/持久化测试和实际进程启动测试。持久化矩阵覆盖 MySQL、PostgreSQL、XML 主存储及 SQLite 二级库，三端 DBApp 均连接真实 PostgreSQL 实例验证登录、写档和重启加载。再组合三端客户端与服务端、替换独立服务进程，核对版本标识、结果和关服顺序。

- [ ] **BW-15-05-01 配置三端原生 runner**。**验收** 每端独立构建和启动。
- [ ] **BW-15-05-02 在三端执行协议、资源与 Python 测试**。**验收** 各端分别留存结果。
- [ ] **BW-15-05-03 配置 MySQL、XML 与 SQLite 测试矩阵**。**验收** 每种模式产生独立结果。
- [ ] **BW-15-05-04 配置 PostgreSQL 真实后端矩阵**。**验收** 各端完成登录、写档和重启读档。
- [ ] **BW-15-05-05 验证三端客户端连接不同平台服务端**。**验收** 记录每种支持组合。
- [ ] **BW-15-05-06 验证混部替换单角色**。**验收** 记录版本和关服顺序。

#### BW-15-06 平台图形和宿主依赖

维护渲染后端和 DCC/插件支持表。三端验证等价的客户端与编辑工作流，宿主专属插件在可用宿主测试，并用导出文件检验其他平台消费能力。

- [ ] **BW-15-06-01 维护图形后端列表**。**验收** 每个平台记录能力及依赖。
- [ ] **BW-15-06-02 维护 DCC 宿主矩阵**。**验收** 逐插件记录可用宿主。
- [ ] **BW-15-06-03 验收三端客户端与编辑流程**。**验收** 同一地图有可比证据。
- [ ] **BW-15-06-04 验证外部导出的跨端读取**。**验收** 各适用消费者可打开。

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

#### BW-16-01 Workspace 依赖边界

服务与客户端运行时依赖共享契约及各自所需的基础库，服务进程间调用走 Mercury 契约。跨角色共用的消息结构按发送方/接收方接口归入 `contracts`，避免进程 crate 互相依赖。依赖方向为 `apps → 角色或工具运行时 → 契约与领域库 → 基础层`。

- [ ] **BW-16-01-01 建立应用与角色 crate 依赖单向关系**。**验收** 依赖图无环。
- [ ] **BW-16-01-02 抽取进程间消息契约**。**验收** 调用方无需依赖对端服务 crate。
- [ ] **BW-16-01-03 验证独立构建**。**验收** 指定服务能单独编译。

#### BW-16-02 平台与外部依赖边界

OS 调用进 `platform`，图形后端进 `rendering`，数据库驱动进 `storage`，CPython FFI 进 `python-host`。应用入口装配具体实现，通过 workspace 依赖图检查循环及跨层引用。

- [ ] **BW-16-02-01 收敛系统调用至 platform**。**验收** 依赖扫描确认调用位置。
- [ ] **BW-16-02-02 隔离图形后端**。**验收** 服务端构建不加载图形后端。
- [ ] **BW-16-02-03 隔离数据库驱动**。**验收** 可单独启用后端。
- [ ] **BW-16-02-04 隔离 CPython FFI**。**验收** 绑定与解释器所有权有测试。
- [ ] **BW-16-02-05 接入依赖检查**。**验收** 跨层违规产生可定位报告。

#### BW-16-03 Python 布局与资源交付

实现各角色的模块注册和部署搜索路径，保留 `BigWorld` 等公开导入名称及原版资源定位。用 Demo 原样导入测试检验构建目录和安装目录。

- [ ] **BW-16-03-01 布置按角色加载的 Python 模块**。**验收** 角色之间公开 API 正确隔离。
- [ ] **BW-16-03-02 实现原版导入名称和路径**。**验收** Demo 无需改导入。
- [ ] **BW-16-03-03 复测构建目录及安装目录**。**验收** 两种启动均找到同一资源。

#### BW-16-04 源码映射与扩展接口

登记原版构建目标、功能编号、新 crate/模块及测试位置。编辑器和工具复用世界/资源/转换库。明确 Rust 原生扩展与 Python 扩展各自入口，验证加载和错误传播。

- [ ] **BW-16-04-01 登记原版目标到新版目录**。**验收** 每个目标有模块与测试路径。
- [ ] **BW-16-04-02 统一编辑器和工具的资源入口**。**验收** 两类应用可加载同一产物。
- [ ] **BW-16-04-03 定义 Rust 扩展入口**。**验收** 成功及错误能正确传播。
- [ ] **BW-16-04-04 定义 Python 扩展入口**。**验收** 导入失败可定位且状态可清理。

### BW-17 Bots 压测

原版入口为 `server/tools/bots/`。BW-12-04 负责原版 Bots 行为对齐，本模块负责场景化压测和结果验收。

#### BW-17-01 Bot 运行时与控制

复用客户端连接层，覆盖批量加减、标签分组、Python 行为驱动、beeline 和巡逻路径，以及原版 Watcher 控制接口。验证原版和新版 Bots 均可登录并完成同一移动轨迹。

- [ ] **BW-17-01-01 接入客户端连接层**。**验收** Bot 可登录及退出。
- [ ] **BW-17-01-02 提供批量增减与标签过滤**。**验收** 控制命令只影响指定 Bot。
- [ ] **BW-17-01-03 移植 Python 行为入口**。**验收** 按脚本推进移动。
- [ ] **BW-17-01-04 实现 beeline**。**验收** 与原版固定路线对照。
- [ ] **BW-17-01-05 实现巡逻路径**。**验收** 路点次序和回环匹配。
- [ ] **BW-17-01-06 接入 Watcher 控制**。**验收** 原版支持调用路径可互测。

#### BW-17-02 场景与执行器

在 `tests/load/scenarios/` 保存固定种子、账号池、并发量、爬升速率、预热及持续时间。覆盖登录洪峰、稳态移动、密集 AOI、跨 Cell、断线重连和长稳运行，记录负载生成端的资源使用。

- [ ] **BW-17-02-01 定义场景输入**。**验收** 固定种子、账号池和并发配置可加载。
- [ ] **BW-17-02-02 实现爬升、预热和持续时间**。**验收** 执行器输出各阶段时间。
- [ ] **BW-17-02-03 实现登录洪峰场景**。**验收** 可重复发送指定速率。
- [ ] **BW-17-02-04 实现稳态移动场景**。**验收** 固定种子生成相同轨迹。
- [ ] **BW-17-02-05 实现密集 AOI 场景**。**验收** 记录目标密度及可见数。
- [ ] **BW-17-02-06 实现跨 Cell 场景**。**验收** 报告穿越成功率。
- [ ] **BW-17-02-07 实现断线重连场景**。**验收** 统计恢复时长。
- [ ] **BW-17-02-08 实现长稳场景**。**验收** 保存压测进程资源使用。

#### BW-17-03 指标和回归

输出机器可读结果，按场景汇总登录成功率、在线数、延迟分位数、tick 超时、带宽及资源使用。短场景进 PR smoke，容量和长稳场景进入定时或手动任务，结合测得的基线设阈值。

- [ ] **BW-17-03-01 输出机器可读场景结果**。**验收** 版本、拓扑和种子可追溯。
- [ ] **BW-17-03-02 聚合登录成功率及在线数**。**验收** 用已知输入核对统计。
- [ ] **BW-17-03-03 聚合延迟分位数与超时**。**验收** 固定样本验证 p95/p99。
- [ ] **BW-17-03-04 聚合带宽与资源使用**。**验收** 报告指标口径和采样周期。
- [ ] **BW-17-03-05 设置 PR smoke**。**验收** 低负载失败可复跑。
- [ ] **BW-17-03-06 提供容量和长稳作业**。**验收** 阈值取自已记录基线。

### BW-18 内部测试框架

原版入口为 `lib/unit_test_lib/`、`lib/testing/` 和 [TESTING.md](../BigWorld/TESTING.md)。Rust 测试支持库归 `crates/test-support/`，Python 套件归 `tests/python/`。

#### BW-18-01 通用夹具和用例映射

实现资源根目录、临时工作区、固定随机种子、配置与数据库夹具。用 Rust 原生测试和 pytest 复刻模块测试，登记原版用例到新版用例的对应关系。

- [ ] **BW-18-01-01 提供资源根和临时目录夹具**。**验收** 并行测试目录隔离。
- [ ] **BW-18-01-02 提供固定随机种子**。**验收** 重复调用给出同一序列。
- [ ] **BW-18-01-03 提供配置和数据库夹具**。**验收** 测试可从已知状态启动。
- [ ] **BW-18-01-04 映射原版单测**。**验收** Rust 和 pytest 用例可通过原始编号追溯。

#### BW-18-02 多进程测试框架

对应原版 `multi_proc_test_case`，提供按拓扑启动、按条件等待就绪、独立端口和数据库、日志采集、失败现场保留及进程清理。验证并行执行时隔离和三端清理。

- [ ] **BW-18-02-01 按拓扑启动进程**。**验收** 依赖顺序可观察。
- [ ] **BW-18-02-02 按就绪条件等待**。**验收** 超时能明确定位角色。
- [ ] **BW-18-02-03 分配独立端口与数据库**。**验收** 两个测试并跑互不干扰。
- [ ] **BW-18-02-04 保留日志与失败目录**。**验收** 失败报告可打开原始产物。
- [ ] **BW-18-02-05 清理整个进程树**。**验收** 三端测试退出无残留进程。

#### BW-18-03 可控时间与消息故障

对应原版 `lib/testing/` 消息过滤能力，提供虚拟时钟、手动推进 tick、丢包、延迟、乱序与进程故障注入。验证定时器、重传、迁移和恢复行为可重复。

- [ ] **BW-18-03-01 实现手动时钟**。**验收** 同一序列推进得到相同 Tick。
- [ ] **BW-18-03-02 实现确定性丢包**。**验收** 指定包触发预期重传。
- [ ] **BW-18-03-03 实现延迟及乱序**。**验收** 按指定顺序交付。
- [ ] **BW-18-03-04 提供进程故障夹具**。**验收** 可复现迁移或恢复。
- [ ] **BW-18-03-05 接通可控时钟取消测试**。**验收** 定时器重复用例稳定。

#### BW-18-04 本地与 CI 共用入口

提供按模块执行、全量测试、兼容差分、覆盖率和结果归档入口。文档记录调用、依赖、夹具来源和失败复现方式。

- [ ] **BW-18-04-01 提供单模块和全量入口**。**验收** 同一模块可单独执行。
- [ ] **BW-18-04-02 提供兼容差分入口**。**验收** 输出可定位差异。
- [ ] **BW-18-04-03 提供覆盖率入口**。**验收** 报告能映射源码。
- [ ] **BW-18-04-04 提供结果归档**。**验收** CI 和本地采用同一目录及格式。
- [ ] **BW-18-04-05 编写失败重现说明**。**验收** 从产物可复跑相应用例。

### BW-19 VitePress 文档站

参考原版 `docs/.vitepress/config.mjs`、`docs/zh/` 和仓库根 `package.json`。Boundless 文档站位于 `docs/`。

#### BW-19-01 站点骨架

配置导航、侧栏、搜索、静态资源和构建入口。在 CI 校验链接与生产构建，并生成可预览的站点产物。

- [ ] **BW-19-01-01 构建 VitePress 配置及导航**。**验收** 关键页面可访问。
- [ ] **BW-19-01-02 配置搜索与静态资源**。**验收** 搜索命中且资源路径可用。
- [ ] **BW-19-01-03 接入链接检查**。**验收** 坏内部链接使检查失败。
- [ ] **BW-19-01-04 生成预览与生产产物**。**验收** 两种入口都能加载站点。

#### BW-19-02 用户与开发文档

编写三端安装、Demo、独立服务启动、Python 实体开发、客户端 SDK 和引擎接入指南。每条快速开始使用干净环境验证。

- [ ] **BW-19-02-01 编写三端安装指南**。**验收** 各端干净环境可复现。
- [ ] **BW-19-02-02 编写 FantasyDemo 快速开始**。**验收** 按步骤可登录进入世界。
- [ ] **BW-19-02-03 编写独立服务指南**。**验收** 命令能启动并停止单角色。
- [ ] **BW-19-02-04 编写实体开发指南**。**验收** 示例脚本有可观测回调。
- [ ] **BW-19-02-05 编写 SDK 与引擎接入指南**。**验收** 示例包含第一次 RPC 验证。

#### BW-19-03 架构及运维参考

维护进程拓扑、协议兼容矩阵、配置和 API、数据库迁移、Bots 压测、内部测试框架和发布指南，链接对应任务及可运行样例。

- [ ] **BW-19-03-01 维护进程拓扑和兼容矩阵**。**验收** 可追溯目标及版本。
- [ ] **BW-19-03-02 维护配置、CLI 和 API 参考**。**验收** 条目链接到可执行示例。
- [ ] **BW-19-03-03 编写数据库迁移和恢复说明**。**验收** 样本恢复步骤可执行。
- [ ] **BW-19-03-04 编写 Bots 压测说明**。**验收** 示例能产出报告。
- [ ] **BW-19-03-05 编写测试和发布说明**。**验收** 本地可复跑对应 CI 步骤。

### BW-20 VS Code 插件

Boundless 新增交付，独立位于 `extensions/vscode/`，内部拆为 `src/`、`syntaxes/`、`snippets/` 和 `test/`。

#### BW-20-01 工程识别与语言配置

识别资源根、`entities.xml`、`alias.xml`、实体 `.def` 和 Base/Cell/Client 脚本目录。提供语法、片段和实体定义到脚本的跳转；诊断复用 `process_defs` 或同一 EntityDef 检查入口。

- [x] **BW-20-01-01 识别工作区资源根**。**验收** 单根与多根均定位配置。
  - 证据：原版结果——`lib/entitydef/constants.hpp` 把 `entitiesFile()=scripts/entities.xml`、`entitiesBasePath()=scripts/base`、`entitiesCellPath()=scripts/cell`、`entitiesClientPath()=scripts/client`、`entitiesServicePath()=scripts/service`、`entitiesDefsPath()=scripts/entity_defs` 定义为相对 res 根的路径，`HasScriptOrTagDistributionDecider::canBeOnCell`（`entity_description.cpp:72-88`）先用 `<Distribution>` 标签、否则探测 `scripts/cell/<Name>.py` 是否存在。新版结果——`findResourceRoots`（`extensions/vscode/src/workspace/resourceRoot.ts`）以 `<root>/scripts/entities.xml` 为标记、BFS 取最浅 res 根并回填同一组常量，多根按 folder 顺序各出一份并记住各自来源 folder。证据链接——`pnpm vitest` 82 例全绿（含 `test/resourceRoot.test.ts` 8 例）且 `tsc` 0 错误；实跑 `node /tmp/bwroots.cjs`：单根在 `examples/client_integration/python/simple` 下定位到 `…/simple/res` 并解析出 ClientServer=5 个实体，无 res 根的工作区返回空，多根（docs 目录 + `/tmp/bwprobe` + simple）跳过 docs 后按序定位 2 个 res 根。
- [x] **BW-20-01-02 配置 .def 与配置文件语法**。**验收** 示例高亮类型可检查。
  - 证据：原版结果——原仓库无编辑器集成，实体配置是纯 XML（`entities.xml`、`entity_defs/alias.xml`、`entity_defs/*.def`），类型与标志的合法取值来自 `lib/entitydef/data_types.cpp` 的 `FORCE_LINK` 类型表与 `data_description.cpp:72-84` 的 `s_entityDataFlagMappings`。新版结果——`extensions/vscode/syntaxes/entities.tmLanguage.json`（`source.entities`）把数据类型映为 `support.type.entity-type`、数据标志为 `support.constant.entity-flag`、布尔为 `constant.language.boolean`、`<implementedBy>` 为 `variable.other.implemented-by`；`package.json` 的 `languages`/`grammars` 把 `.def` 与 `entities.xml`、`alias.xml`、`services.xml`、`capabilities.xml`、`user_data_objects.xml`（均取自 `lib/entitydef/constants.hpp` 的路径常量）绑到同一语言。证据链接——`pnpm test` 95 例全绿（含 `test/grammar.test.ts` 13 例）且 `pnpm run check` 对 `src` 与 `test` 两套 tsconfig 均 0 错误；取证用 VS Code 同款引擎（`vscode-textmate` 8.0.0 + `vscode-oniguruma` 的 `onig.wasm`）实跑 `node /tmp/bwscopes.cjs`，对原版 `Account.def` 逐行输出高亮类型，得到上述三类且逐行可核对（如第 4 行 `STRING`→`support.type.entity-type`、第 5 行 `BASE_AND_CLIENT`→`support.constant.entity-flag`）。真引擎还查出并修掉两处语法缺陷：值规则漏吞开标签的 `>` 导致 `>` 被当成标志，以及紧凑写法下 `\S+` 越界吞掉 `</Flags>`。
- [x] **BW-20-01-03 提供实体和脚本片段**。**验收** 插入内容可被 EntityDef 读取。
  - 证据：原版结果——原仓库无编辑器片段，模板只能从 `examples/client_integration/python/simple/res/scripts/entity_defs/*.def` 抄写；段落结构以 `lib/entitydef/data_description.cpp`、`entity_description.cpp`、`method_description.cpp` 的解析为准（`Properties`/`Flags`/`Persistent`/`Identifier`/`Indexed`/`Unique`、`Volatile`、`Distribution`、`LoDLevels`、`BaseMethods`/`CellMethods`/`ClientMethods`、service 用 `<Methods>`）。新版结果——`extensions/vscode/snippets/entities.json` 15 个片段（`bw-entity`、`bw-service`、`bw-prop`、`bw-prop-persistent`、`bw-prop-array`、`bw-prop-user-type`、`bw-base-method`、`bw-cell-method`、`bw-client-method`、`bw-service-method`、`bw-method-arg`、`bw-volatile`、`bw-distribution`、`bw-lod`、`bw-alias`），由 `package.json` 的 `contributes.snippets` 绑到 `entities` 语言。证据链接——`pnpm test` 100 例全绿（含 `test/snippets.test.ts` 5 例）且 `pnpm run check` 0 错误；关键验收直接落在测试里：把每个片段的 TextMate 占位符替换成默认值、按片段归属包成最小 `.def`，喂给 `parseEntityDef` + `checkEntityDef`（别名表用原版 `alias.xml`），断言 0 个 error 级诊断，`bw-alias` 单独用 `AliasRegistry.parse` 验证，另断言插入文本无残留 `${n}` 占位符。
- [x] **BW-20-01-04 接入定义跳转**。**验收** 测试文件定位到目标脚本。
  - 证据：原版结果——引用端配对 `.def` 与脚本只看路径：`EntityDef::Constants` 的 `entitiesBasePath()/entitiesCellPath()/entitiesClientPath()=scripts/{base,cell,client}` 加上 `.def` 文件名；`HasScriptOrTagDistributionDecider`（`entity_description.cpp:72`）以 `scripts/<role>/<Name>.py` 是否存在决定该组件可跑；Python 侧方法实现是同名 `def <method>(`。新版结果——`extensions/vscode/src/workspace/navigation.ts` 的 `definitionTargets`：`<Parent>` 值跳父实体 `.def`；光标落在方法元素内跳对应组件脚本并用 `symbolLine` 定位 `def <name>`（0 基行号）；否则按 base/cell/client（service def 则 service）列出存在的脚本；`extension.ts` 以 `vscode.languages.registerDefinitionProvider("entities", …)` 接入，多根工作区取最深覆盖根。顺带修掉移植时引入的 `xmlSection.ts` 偏移缺陷：`nameOffset` 原多加一，与文件头注释"指向节点名首字符"不符，现与原文对齐。证据链接——`pnpm test` 109 例全绿（含 `test/navigation.test.ts` 9 例）且 `pnpm run check` 0 错误；对原版示例树实跑 `node tools/verify/navigation.cjs`（编译产物驱动，目标 `examples/client_integration/python/simple/res`）：光标在 `<Properties>` 给出 base/cell/client 三个脚本，`logOff`→`scripts/base/ClientAvatar.py` 第 36 行、`fromCell`→`scripts/client/ClientAvatar.py` 第 24 行、`cellTick`→`scripts/client/ClientAvatar.py` 第 51 行，与直接 grep 原树得到的 `def` 行号逐一相同。
- [x] **BW-20-01-05 接入 EntityDef 诊断**。**验收** 坏属性的范围和消息可见。
  - 证据：原版结果——`process_defs` 对同一批坏定义报出（`sh tools/verify/referenceBaseline.sh` 实跑）：`badflags.def`→`ERROR: [DataDescription] DataDescription::parse: Invalid Flags section 'WRONG' for hp`；`badtype.def`→`ERROR: [DataDescription] DataType::buildDataType: Could not find MetaDataType 'NOPE'` 后跟 `DataDescription::parse: Unable to find data type 'NOPE' for Fixture.hp`。新版结果——诊断直接复用同一个 EntityDef 检查入口：`def/checker.ts` 的 `checkEntityDef`（规则逐条对照 `data_description.cpp:169-331`、`entity_description.cpp:289-340`、`method_description.cpp`）负责产出绝对偏移，`extensions/vscode/src/workspace/diagnostics.ts` 的 `checkDefText` 把偏移翻成行列范围（`rangeAt`/`lineStarts`），并把解析失败收成覆盖全文的 `XML parse error`；`extension.ts` 用 `vscode.languages.createDiagnosticCollection("boundless-entitydef")` 在打开/改动/保存时发布，别名表取自资源根的 `alias.xml`，`hasScript` 按引用端的 `HasScriptOrTagDistributionDecider` 探测 `scripts/<role>/<Name>.py`。证据链接——`pnpm test` 120 例全绿（含 `test/diagnostics.test.ts` 11 例）且 `pnpm run check` 0 错误；对与原版同一批 fixture 实跑 `node tools/verify/diagnostics.cjs`：`badflags.def` 出 1 条 error，锚在 5:5（该行即 `\t\t\t<Flags>WRONG</Flags>`），消息与原版逐字一致；`badtype.def` 出 2 条 error，均锚在 4:5（该行即 `\t\t\t<Type>NOPE</Type>`），两条消息与原版两行完全对应；`good.def` 零诊断。

#### BW-20-02 开发命令和调试

提供任务入口启动/停止本地集群、查看运行状态、运行选定测试和 Bots 场景、查看进程日志。接入 BW-27 的 LSP/DAP 和 BW-28 的文档 MCP，验证 Python 附加调试与 Rust 调试启动配置。

- [x] **BW-20-02-01 接入本地集群启动停止**。**验收** 测试不会留下进程。
  - 证据：原版结果——引用端每角色一个进程（`bigworld_app baseapp` 等），`BW_RES_PATH` 导出 res 根；本版 `crates/server-runtime/src/app.rs` 接受同样角色名、`crates/resources/src/bwresource.rs:195` 读 `BW_RES_PATH`。新版结果——`extensions/vscode/src/workspace/cluster.ts` 的 `ClusterControl`（注入 `ProcessRunner`）按角色表起停角色，`roleSpec` 复刻引用端启动行（`BW_RES_PATH=<resRoot>`、`<binary> <role>`、日志在 `<logDir>/<role>.log`），`src/workspace/processRunner.ts` 用 detached 进程组启动、SIGTERM→SIGKILL 收树，Windows 走 `taskkill /t /f`；`extension.ts` 提供 `boundless.startCluster`/`stopCluster` 命令。证据链接——`pnpm test` 133 例全绿（含 `test/cluster.test.ts` 8 例，其中真实子进程用例：detached 起一个 node 进程、`stopAll()` 后 `isRunning` 为假且无遗留）；实跑 `node tools/verify/cluster.cjs`（编译产物驱动，脚本随插件入库 `extensions/vscode/tools/verify/`）：起停两个角色后 `pgrep -f` 标记进程为零。
- [x] **BW-20-02-02 接入状态和日志查看**。**验收** 列出当前角色且定位对应日志。
  - 证据：原版结果——`process_defs` 与各进程日志分离，`bigworld_app -p` 下角色日志落在 `log/`；本版沿用同一分离面。新版结果——`ClusterControl.roles()` 按角色表顺序给出 `{role, pid, commandLine, logPath}`；`boundless.showClusterStatus` 输出到 "Boundless Cluster" 通道，逐个角色列出 pid 与日志路径；`boundless.showClusterLogs` 用快速选择挑角色并打开对应日志文件（不存在时提示路径）。证据链接——`pnpm test` 全绿（含 `test/cluster.test.ts` "stops the role that was asked for and keeps the others" 与真实进程用例）；实跑 `node /tmp/bwcluster.cjs`：`status` 列出 baseapp（pid 700891）与 cellapp（pid 700893）及各自 `logPath`，读 `.boundless/logs/baseapp.log` 得 `baseapp started (pid 700891)`，`cellapp.log` 得 `cellapp started (pid 700893)`——路径与内容一一对应。
- [x] **BW-20-02-03 接入测试和 Bots 命令**。**验收** 失败时显示退出状态与结果路径。
  - 证据：原版结果——仓库自带检查入口是 `cargo test`（根 `Cargo.toml` workspace）与 Python 场景运行器；失败信息落在各自输出里。新版结果——`src/workspace/jobs.ts` 定义 `JobSpec`/`JobOutcome`/`formatOutcome`/`exitStatus`（区分退出码与信号、无状态三种失败面），`src/workspace/jobRunner.ts` 的 `nodeJobRunner()` 用 `execFile` 执行并把 `名称: 退出状态` 加输出尾部写进结果文件；`extension.ts` 的 `boundless.runTests`/`boundless.runBots` 命令经 `boundless.cluster.testsCommand`/`botsCommand`（默认 `cargo test`、bots 默认留空即"未配置"）取命令，向 "Boundless Cluster" 通道追加 `failed: exit code 7; results at <path>` 并在失败时聚焦通道。证据链接——`pnpm test` 全绿（含 `test/jobs.test.ts` 5 例：失败 job 报 `exit code 7` 并带结果路径、成功 job 报 ok、不存在的命令报 `no exit status`）；实跑 `node tools/verify/cluster.cjs` 尾部：`failing job -> failed: exit code 7; results at …/.boundless/reports/bots.log` 且该结果文件确实存在。
- [x] **BW-20-02-04 接入 LSP**。**验收** 工作区文件能诊断与跳转。
  - 证据：原版结果——引用端没有 LSP，编辑器消费 `.def` 只能靠构建期 `process_defs` 的批检查；诊断与跳转能力对编辑器不存在。新版结果——语言能力全部落在 vscode-free 服务层：诊断走 `checkDefText`（`src/workspace/diagnostics.ts`，规则来自与 `process_defs` 对齐的 `checkEntityDef`），跳转走 `definitionTargets`（`src/workspace/navigation.ts`，`HasScriptOrTagDistributionDecider` 的路径规则）；`extension.ts` 以 `registerDefinitionProvider` + `DiagnosticCollection` 发布，BW-27 的 LSP 可直接复用同一服务层而无须经过编辑器 API。证据链接——`pnpm test` 137 例全绿且 `pnpm run check` 0 错误；新增 `test/lsp.test.ts` 4 例在真实磁盘工作区（`node:fs` 落盘 + 真实 `nodeProbe`，无注入替身）验证：定位 res 根、坏 `<Flags>` 文档报 `property.flags-invalid`（消息含 `Invalid Flags section 'ON_NO_CELLS'`）而干净文档零诊断、`logOn` 方法元素跳到 `scripts/base/Clean.py` 第 5 行（0 基 4）、实体体跳到 base+client 两个真实存在的脚本。
- [x] **BW-20-02-05 验证 VS Code 的 Python 附加**。**验收** 能在 BaseApp 实体回调命中断点。
  - 证据：原版结果——引用端实体脚本是 `scripts/base/<Name>.py` 里的普通类方法（`ClientAvatar.logOff`），服务器内嵌 CPython（BW-03 同一宿主面），调试靠 IDE 附加。新版结果——`src/workspace/launchConfig.ts` 生成 `debugpy` 附加配置（每角色一个端口，`mergeLaunchJson` 只增不改地并进工作区 `launch.json`，8 个测试覆盖），`extension.ts` 的 `boundless.setupPythonDebug` 命令写入；角色脚本宿主用 CPython + debugpy（VS Code Python 扩展同一协议 DAP）。证据链接——`pnpm test` 145 例全绿且 `pnpm run check` 0 错误；实跑 `python3 tools/verify/pythonAttach.py` 7/7 通过：对运行中的脚本宿主（原版 `ClientAvatar.py`，仅 `BigWorld` 绑定打桩）做 DAP 附加，`setBreakpoints` 在第 36 行验证通过，恢复执行后 `stopped reason=breakpoint` 顶帧恰为 `ClientAvatar.py:36`（`logOff`），continue 后回调完整跑完（输出含 `ClientAvatar.logOff: 1` 与 `callback returned`），进程组清零。
- [ ] **BW-20-02-06 验证 VS Code 的 Rust 启动调试**。**验收** 能由启动配置运行 CellApp 并命中断点。
- [ ] **BW-20-02-07 提供文档 MCP 接入配置**。**验收** 查询返回当前工作区版本文档。

#### BW-20-03 打包与三端验证

提供 VSIX、扩展宿主集成测试及安装说明，在 Linux、Windows、macOS 用 Demo 工作区检查核心流程。源码和发布包在插件目录独立构建。

- [ ] **BW-20-03-01 独立构建 VSIX**。**验收** 可在干净编辑器安装。
- [ ] **BW-20-03-02 编写安装和配置说明**。**验收** 能从空工作区启用。
- [ ] **BW-20-03-03 验证三端扩展宿主**。**验收** Demo 工作区核心动作均通过。

### BW-21 多引擎客户端 SDK

原版互通基准见 `lib/{connection,connection_model,bwentity}/` 与 `examples/client_integration/`。Unity、Unreal、Godot 作为首批拟支持的引擎，具体支持版本随适配原型验证。

#### BW-21-01 公共核心 SDK

复用 Mercury、EntityDef 和 connection，提供登录、断开/重连、实体创建销毁、属性同步、RPC、移动输入及空间事件。用统一契约套件验证原版及新版服务端互通。

- [ ] **BW-21-01-01 实现 SDK 登录和断开**。**验收** 原版及新版服务器均可登录退出。
- [ ] **BW-21-01-02 实现重连**。**验收** 恢复会话后能调用方法。
- [ ] **BW-21-01-03 暴露实体创建销毁与空间事件**。**验收** 生命周期顺序一致。
- [ ] **BW-21-01-04 暴露属性同步**。**验收** 代表性嵌套属性更新可读。
- [ ] **BW-21-01-05 暴露双向 RPC**。**验收** 参数与应答可对照。
- [ ] **BW-21-01-06 暴露移动输入**。**验收** 服务端收到相同输入数据。

#### BW-21-02 ABI 与 Python SDK

定义原生句柄、回调线程、内存归属、错误码和版本协商，生成公开头文件。验证加载、断线和销毁。Python 客户端覆盖原版 simple 示例。

- [ ] **BW-21-02-01 定义 ABI 句柄与释放约定**。**验收** 重复释放错误可识别。
- [ ] **BW-21-02-02 定义回调线程和事件派发**。**验收** 固定用例可断言线程。
- [ ] **BW-21-02-03 定义 ABI 版本及错误码**。**验收** 版本不匹配可诊断。
- [ ] **BW-21-02-04 生成并验证公开头文件**。**验收** C 示例能加载及调用。
- [ ] **BW-21-02-05 交付 Python SDK**。**验收** 原版 simple 示例能登录、接收事件并退出。

#### BW-21-03 Unity 适配

实现 C# 封装、主线程事件派发和 Unity 包。示例验证登录、实体生成、属性更新、移动、RPC 与场景退出。

- [ ] **BW-21-03-01 绑定 Unity C# 接口**。**验收** 编辑器加载通过。
- [ ] **BW-21-03-02 派发主线程事件**。**验收** 回调线程断言通过。
- [ ] **BW-21-03-03 打包 Unity 适配**。**验收** 干净项目可导入。
- [ ] **BW-21-03-04 运行 Unity 示例**。**验收** 登录、实体、移动和 RPC 各有结果。

#### BW-21-04 Unreal 适配

实现插件、C++/Blueprint 调用接口和游戏线程派发。用同一 SDK 契约和示例验证组件生命周期。

- [ ] **BW-21-04-01 打包 Unreal 插件**。**验收** 空项目可加载模块。
- [ ] **BW-21-04-02 交付 C++ 调用**。**验收** 示例可登录与发出 RPC。
- [ ] **BW-21-04-03 公开 Blueprint 接口**。**验收** 蓝图可订阅实体事件。
- [ ] **BW-21-04-04 派发游戏线程回调**。**验收** 组件销毁后不再派发。
- [ ] **BW-21-04-05 运行 Unreal 示例**。**验收** 组件登录、同步和退出通过。

#### BW-21-05 Godot 适配

实现 GDExtension、脚本绑定和信号派发。用同一 SDK 契约和示例验证节点生命周期。

- [ ] **BW-21-05-01 打包 GDExtension**。**验收** 干净项目可加载。
- [ ] **BW-21-05-02 绑定 Godot 脚本方法**。**验收** 脚本可登录和发 RPC。
- [ ] **BW-21-05-03 派发信号**。**验收** 目标节点收到实体事件。
- [ ] **BW-21-05-04 运行 Godot 示例**。**验收** 离开场景后连接和回调已清理。

#### BW-21-06 SDK 发布与矩阵

按引擎、版本、操作系统和架构发布并标注支持情况。对照 Python 与原版 Android/iOS 示例验证协议面。各引擎用同一套服务端、实体定义和行为夹具完成端到端跑通。

- [ ] **BW-21-06-01 维护引擎版本及平台矩阵**。**验收** 每个支持项有构建结果。
- [ ] **BW-21-06-02 发布 ABI 和各引擎包**。**验收** 安装后可导入。
- [ ] **BW-21-06-03 验证原版 Android/iOS 样例协议**。**验收** 保存独立结果或缺口。
- [ ] **BW-21-06-04 运行统一引擎互通夹具**。**验收** 每个引擎有登录与同步证据。

### BW-22 CI 工具与发布验证

参考原版仓库的 `.github/workflows/`、`scripts/package-daily*` 和 `scripts/check-docs-links.py`。脚本放在 `ci/`，工作流调用相同入口。

#### BW-22-01 构建与静态检查入口

固定工具链与锁文件，提供 Rust 格式、lint、编译、Python 检查和 TypeScript 检查入口。按变更路径选择检查，文档变更触发站点检查。

- [ ] **BW-22-01-01 固定 CI 工具链和锁文件**。**验收** 本地解析结果一致。
- [ ] **BW-22-01-02 接入 Rust 格式、lint 与构建**。**验收** 各有可复跑入口。
- [ ] **BW-22-01-03 接入 Python 检查**。**验收** 失败定位模块。
- [ ] **BW-22-01-04 接入 TypeScript 检查**。**验收** 失败定位扩展源码。
- [ ] **BW-22-01-05 配置变更路径条件**。**验收** 文档和代码改动触发对应作业。

#### BW-22-02 测试矩阵与服务装配

编排三端原生 runner、真实 MySQL/PostgreSQL 后端、XML/SQLite、原版兼容差分和 SDK/插件测试。每套任务可在本地复跑，归档测试结果、覆盖率、服务日志及失败夹具。

- [ ] **BW-22-02-01 配置三端 runner**。**验收** 每端产出启动测试记录。
- [ ] **BW-22-02-02 编排 MySQL 和 PostgreSQL**。**验收** 可隔离起库并执行存储用例。
- [ ] **BW-22-02-03 编排 XML 与 SQLite**。**验收** 各有独立结果。
- [ ] **BW-22-02-04 接入兼容性差分**。**验收** 替换路径可在本地复跑。
- [ ] **BW-22-02-05 接入 SDK 与插件用例**。**验收** 产物能在宿主中加载。
- [ ] **BW-22-02-06 归档覆盖率、日志和夹具**。**验收** 失败可取回证据。

#### BW-22-03 压测与回归报告

PR 跑短 smoke，定时或手动任务跑容量、稳定性和故障场景。生成不同版本可比较的报告，记录硬件、拓扑、数据库后端及场景版本。

- [ ] **BW-22-03-01 接入 PR 短压测**。**验收** 小规模场景产出结果。
- [ ] **BW-22-03-02 接入定时容量与长稳任务**。**验收** 可配置并发和持续时间。
- [ ] **BW-22-03-03 接入手动故障场景**。**验收** 复现失败时有拓扑和日志。
- [ ] **BW-22-03-04 生成对比报告**。**验收** 版本、硬件、场景及 DB 信息齐全。

#### BW-22-04 发布物流水线

构建并归档三端二进制与安装包、VitePress 站点及匹配的文档索引、SDK 包和 VSIX。校验版本、散列和打包内容，使用干净环境对安装/导入/运行做 smoke。

- [ ] **BW-22-04-01 编排三端安装包**。**验收** 每端烟测可安装和启动。
- [ ] **BW-22-04-02 发布文档与版本化索引产物**。**验收** 版本元数据一致。
- [ ] **BW-22-04-03 归档各引擎 SDK**。**验收** 可以从空项目导入。
- [ ] **BW-22-04-04 归档 VSIX**。**验收** 可以在干净宿主安装。
- [ ] **BW-22-04-05 校验发布元数据与散列**。**验收** 改动产物会检测失败。

### BW-23 内置 Profiler 与性能分析

原版锚点为 `lib/cstdmf/profiler.cpp`、`server/tools/bw_profile/bw_profile.cpp` 和 `client/profiler_app.cpp`。`_bw_profile` 是原版 Python 模块，Boundless 另提供独立采集 CLI。

#### BW-23-01 原生和 Python 采集

记录 Rust 分段计时、调用次数与线程，采集 Python 调用耗时，并在统一单调时钟下关联进程、tick 和请求。用固定脚本验证嵌套调用、线程退出和异常后的事件闭合。

- [ ] **BW-23-01-01 记录 Rust 区间与计数器**。**验收** 固定调用的次数和嵌套匹配。
- [ ] **BW-23-01-02 记录 Python 调用耗时**。**验收** 已知脚本可定位函数。
- [ ] **BW-23-01-03 关联时钟及进程、Tick、请求**。**验收** 跨角色时间线可追踪同一请求。
- [ ] **BW-23-01-04 处理异常和线程退出**。**验收** 每个计时区间正常闭合或有缺失标记。

#### BW-23-02 有界采集会话

按进程和类别启停，覆盖 CPU 采样、tick、RPC、网络流量和 DB 等待。实现持续时间、缓冲上限和丢失记录，复测超时、取消、断连及无采集时开销。

- [ ] **BW-23-02-01 控制采集启停和类别**。**验收** 只返回选中进程及事件。
- [ ] **BW-23-02-02 采集 CPU 样本**。**验收** 固定负载热点可定位。
- [ ] **BW-23-02-03 采集 Tick 与 RPC**。**验收** 固定请求可定位等待。
- [ ] **BW-23-02-04 采集网络及 DB 等待**。**验收** 测试延迟能反映在对应事件。
- [ ] **BW-23-02-05 限制持续时间和缓冲**。**验收** 到期停止并报告丢失。
- [ ] **BW-23-02-06 处理取消及断连**。**验收** 本地会话按配置回收。

#### BW-23-03 性能产物和比较

保留原版可核对的 JSON 输出语义，另导出时间线、火焰图及按函数、线程、tick 和消息的聚合。配合 Bots 记录版本、场景和拓扑，复测同负载下 p95/p99、热点与资源消耗差异。

- [ ] **BW-23-03-01 输出兼容 JSON**。**验收** 原版读者可处理代表性样本。
- [ ] **BW-23-03-02 输出时间线**。**验收** 可按线程过滤同一采集。
- [ ] **BW-23-03-03 输出火焰图数据**。**验收** 热点栈能对应脚本或 Rust 函数。
- [ ] **BW-23-03-04 输出函数、消息及 Tick 统计**。**验收** 固定输入聚合吻合。
- [ ] **BW-23-03-05 联动 Bots 对比**。**验收** 报告 p95/p99 及资源差异与运行版本。

### BW-24 交互式内存诊断

参考 BigWorld 的 `lib/cstdmf/memory_debug.cpp` 与本地 KBEngine 的 [Telnet 控制台实现](../kbengine/kbe/src/lib/server/telnet_handler.cpp)。Boundless 采用类似 Telnet 的交互体验，命令通过本地诊断接口执行。

#### BW-24-01 内存快照

汇总各进程 RSS、分配量、分配高水位、实体/缓存对象计数及可用的 Python 对象统计。记录各平台的指标口径和采集成本，区分实时值与累计值。

- [ ] **BW-24-01-01 采集各进程 RSS**。**验收** 三端输出单位统一。
- [ ] **BW-24-01-02 采集实时分配量**。**验收** 按夹具分配并释放后，读数回到测试允许区间。
- [ ] **BW-24-01-03 采集分配高水位**。**验收** 夹具释放对象后，高水位保留本轮峰值。
- [ ] **BW-24-01-04 统计实体和缓存对象**。**验收** 预定增减后计数吻合。
- [ ] **BW-24-01-05 统计 Python 对象**。**验收** 已知对象生命周期可观测。
- [ ] **BW-24-01-06 登记指标来源和成本**。**验收** 实时值及累计值可区分。

#### BW-24-02 交互查询

支持列出诊断目标、按进程或子系统查询、生成快照、比较两次快照和导出 JSON。默认查询范围为本机且只读，按需授权访问，输出结构化统计。

- [ ] **BW-24-02-01 列出本地诊断进程**。**验收** 仅显示获授权目标。
- [ ] **BW-24-02-02 查询选定进程或子系统**。**验收** 结果含目标和采集时间。
- [ ] **BW-24-02-03 生成内存快照**。**验收** 返回稳定快照 ID。
- [ ] **BW-24-02-04 比较两次快照**。**验收** 按字段展示差异。
- [ ] **BW-24-02-05 导出 JSON**。**验收** 与交互输出的数值相同。
- [ ] **BW-24-02-06 验证访问边界**。**验收** 未获授权查询被拒且接口保持只读。

#### BW-24-03 增长排查

用可控分配和释放夹具验证差值、分配热点和回收趋势，配合长稳压测报告快照时间、采集开销和对象增长。

- [ ] **BW-24-03-01 构造已知分配及释放夹具**。**验收** 差值可预测。
- [ ] **BW-24-03-02 定位分配热点**。**验收** 采样能识别已知来源。
- [ ] **BW-24-03-03 验证回收趋势**。**验收** 释放后活跃对象数回落。
- [ ] **BW-24-03-04 接入 Bots 长稳采样**。**验收** 报告快照耗时、增长和采集开销。

### BW-25 进程和集群运行状态

原版锚点为 Watcher、MachineGuard、`server/loginapp/status_check_watcher.cpp` 与 `server/cellapp/cell_viewer_server.cpp`。

#### BW-25-01 状态采集

聚合进程版本、就绪、心跳和负载、玩家与实体数、Space/Cell 分布、AOI、网络与 DB 队列以及故障恢复阶段。标注时间、来源与采集有效期。

- [ ] **BW-25-01-01 采集进程版本、就绪和心跳**。**验收** 来源和时间可追踪。
- [ ] **BW-25-01-02 采集玩家及实体数**。**验收** 与测试拓扑状态吻合。
- [ ] **BW-25-01-03 采集 Space/Cell 和 AOI**。**验收** 迁移前后归属可查。
- [ ] **BW-25-01-04 采集网络和 DB 队列**。**验收** 已知积压在快照可见。
- [ ] **BW-25-01-05 记录故障恢复阶段**。**验收** 故障演练中状态随事件变化。

#### BW-25-02 查询与视图

提供单进程/整集群 CLI、JSON 快照和实时刷新视图，支持按角色、Space、Cell 定位并关联 Watcher、日志和 profile 产物。供 CLI、CI 和 VS Code 复用同一查询契约。

- [ ] **BW-25-02-01 提供单进程与集群 CLI**。**验收** 可按角色筛选目标。
- [ ] **BW-25-02-02 提供 JSON 快照**。**验收** CLI、CI 与 IDE 可解析同一字段。
- [ ] **BW-25-02-03 提供实时刷新视图**。**验收** 状态更新时可按时间定位。
- [ ] **BW-25-02-04 关联 Space/Cell、Watcher 和日志**。**验收** 同一对象可交叉定位。
- [ ] **BW-25-02-05 关联 profile 产物**。**验收** 诊断输出包含对应会话 ID。

#### BW-25-03 故障呈现

验证未就绪、过载、心跳过期、节点掉线、部分查询失败和恢复，区分不可达、陈旧和零值。状态输出用于集群 smoke。

- [ ] **BW-25-03-01 显示未就绪和过载**。**验收** 两个状态分别可见。
- [ ] **BW-25-03-02 标识心跳过期**。**验收** 显示陈旧时间且与零值区分。
- [ ] **BW-25-03-03 处理节点失联或部分查询失败**。**验收** 健康节点结果仍可读。
- [ ] **BW-25-03-04 验证恢复**。**验收** 节点重新出现且状态更新。
- [ ] **BW-25-03-05 接入集群 smoke**。**验收** 未就绪用例给出可诊断失败。

### BW-26 数据分析与报表

性能采集由 BW-23 负责，本模块处理游戏和运营事件数据。先交付事件文件及离线分析链路，数据仓库选型留待实际数据量和查询需求明确。

#### BW-26-01 事件契约与采集

为登录、会话、空间移动和业务自定义事件定义版本、事件 ID、时间与来源，提供 Python 上报接口和批量导出。验证重试去重、晚到数据、版本兼容及字段脱敏。

- [ ] **BW-26-01-01 定义登录、会话与移动事件**。**验收** 版本、事件 ID 和来源可验证。
- [ ] **BW-26-01-02 实现 Python 自定义事件上报**。**验收** 未知字段按契约处理。
- [ ] **BW-26-01-03 实现批量导出与边界脱敏**。**验收** 样本敏感字段不泄露。
- [ ] **BW-26-01-04 处理重复与晚到**。**验收** 固定乱序输入得到同一结果。

#### BW-26-02 分析工具

提供校验、筛选、聚合和报表 CLI，覆盖在线/并发人数、会话时长、登录漏斗、空间热度及自定义事件。以固定数据集验收各指标的计数、时间窗口和导出结果。

- [ ] **BW-26-02-01 提供校验与筛选 CLI**。**验收** 坏事件和有效筛选可复现。
- [ ] **BW-26-02-02 计算在线与并发**。**验收** 固定会话窗口结果正确。
- [ ] **BW-26-02-03 计算会话时长**。**验收** 跨日及未结束会话规则有样本。
- [ ] **BW-26-02-04 计算登录漏斗**。**验收** 重试不重复计数。
- [ ] **BW-26-02-05 计算空间热度**。**验收** 固定位置夹具落入正确分桶。
- [ ] **BW-26-02-06 提供自定义事件聚合和导出**。**验收** 同输入报表可复算。

#### BW-26-03 数据作业回归

验证重复导入、失败续跑、日期切分和大批量输入。将查询示例、指标口径及使用说明纳入 VitePress。

- [ ] **BW-26-03-01 验证重复导入**。**验收** 聚合结果不增加。
- [ ] **BW-26-03-02 验证失败续跑**。**验收** 恢复后与一次性输入结果一致。
- [ ] **BW-26-03-03 验证日期切分和大批次**。**验收** 边界与批次大小不会改变计数。
- [ ] **BW-26-03-04 编写指标口径及查询例**。**验收** VitePress 示例能独立执行。

### BW-27 开发工具协议

独立协议服务由 `integrations/` 交付。VS Code 插件按 BW-20 消费这些接口。

#### BW-27-01 LSP

为 EntityDef 与资源引用提供诊断、补全、定义跳转、引用查找和悬浮信息。复用 EntityDef 解析和工作区索引，验证修改、重命名、多根工作区及协议请求取消。

- [ ] **BW-27-01-01 实现 EntityDef 增量索引**。**验收** 文件编辑和删除可刷新。
- [ ] **BW-27-01-02 提供诊断**。**验收** 坏定义定位到正确范围。
- [ ] **BW-27-01-03 提供实体属性补全**。**验收** 字段上下文只返回同一实体允许的候选。
- [ ] **BW-27-01-04 提供定义悬浮信息**。**验收** 悬浮在属性类型上显示正确的 EntityDef 类型及文档。
- [ ] **BW-27-01-05 提供实体定义跳转**。**验收** 从脚本引用跳到正确的 .def 文件与字段范围。
- [ ] **BW-27-01-06 提供跨文件引用查找**。**验收** 从定义定位多个脚本中的使用位置。
- [ ] **BW-27-01-07 处理重命名和多根**。**验收** 同名文件不会串结果。
- [ ] **BW-27-01-08 处理请求取消**。**验收** 长操作可取消。

#### BW-27-02 DAP

复用可用的 Python 与 Rust 调试适配器，提供启动/附加、多进程目标发现和源码映射。以 BaseApp 与 CellApp 用例验收断点、栈帧、变量查看和断开后的进程清理。

- [ ] **BW-27-02-01 接入 Python 启动和附加**。**验收** BaseApp 断点可命中。
- [ ] **BW-27-02-02 接入 Rust 启动和附加**。**验收** CellApp 断点可命中。
- [ ] **BW-27-02-03 发现多进程调试目标**。**验收** 可区分同角色实例。
- [ ] **BW-27-02-04 实现源码映射**。**验收** 符号定位到本地正确文件。
- [ ] **BW-27-02-05 验证栈和变量查看**。**验收** 固定断点数据正确。
- [ ] **BW-27-02-06 验证断开清理**。**验收** 被附加进程按配置继续或退出。

#### BW-27-03 工具互通

为运行状态、进程选择、测试和压测交付稳定 CLI 与机器可读输出。用 VS Code 和独立协议测试客户端验证错误传递、三端路径、版本兼容和长任务取消。

- [ ] **BW-27-03-01 稳定状态查询及进程选择输出**。**验收** 独立客户端可解析。
- [ ] **BW-27-03-02 稳定测试和压测 CLI 输出**。**验收** VS Code 可定位产物。
- [ ] **BW-27-03-03 处理三端路径与版本差异**。**验收** 错误信息可读且可区分。
- [ ] **BW-27-03-04 实现长任务取消**。**验收** 执行器退出并报告取消。

### BW-28 文档知识库 MCP

文档来源为 BW-19 VitePress 的 Markdown、公开 API 参考和已登记的示例。MCP 服务位于 `integrations/mcp/docs/`。

#### BW-28-01 版本化文档索引

按 Git 版本、语言、文档路径及标题锚点建立索引，从 VitePress 公开源生成。验证章节拆分、增量重建、删除和版本切换。

- [ ] **BW-28-01-01 提取公开 VitePress 章节**。**验收** 标题、锚点及语言可定位。
- [ ] **BW-28-01-02 按 Git 版本建索引**。**验收** 两个版本同名文档可区分。
- [ ] **BW-28-01-03 更新修改过的文档章节**。**验收** 增量刷新后搜索命中新文本及当前锚点。
- [ ] **BW-28-01-04 移除删除的文档章节**。**验收** 增量刷新后检索结果和链接都不再包含被删除章节。
- [ ] **BW-28-01-05 验证公开边界**。**验收** 私有夹具不会进入索引。

#### BW-28-02 MCP 文档查询

提供文档搜索、按版本读取和链接解析，返回来源、路径、章节和版本。首发支持本地 stdio，由 VS Code 或其他 MCP 客户端接入。

- [ ] **BW-28-02-01 提供 stdio MCP 会话**。**验收** 客户端能列出搜索和读取入口。
- [ ] **BW-28-02-02 提供文档搜索**。**验收** 固定查询命中预期章节。
- [ ] **BW-28-02-03 提供版本化读取**。**验收** 返回路径、版本、章节和源链接。
- [ ] **BW-28-02-04 解析 VitePress 链接**。**验收** 读取结果的锚点可打开。

#### BW-28-03 检索和发布验收

用实体属性、Cell 迁移、PostgreSQL 运维等真实问题验证命中章节及链接。CI 在索引过期、引用失效或内容超出公开文档范围时报告问题。

- [ ] **BW-28-03-01 验证 EntityDef、Cell 迁移和 PostgreSQL 查询**。**验收** 三项均命中对应参考。
- [ ] **BW-28-03-02 接入索引时效检查**。**验收** 改文档不重建时检查失败。
- [ ] **BW-28-03-03 接入链接及公开范围检查**。**验收** 坏链接和越界样本被识别。

### BW-29 KBEngine 差异和可选能力复核

按前面的 KBEngine 清单逐项记录「已有等价、列入后续实现、暂缓」及依据。确认列入后给每个新增能力分配实施编号和验收样例。

#### BW-29-01 地图导航差异

验证 KBEngine `navigation/` 中的 Recast/Detour、平铺数据及 CellApp 异步加载是否在当前构建与脚本入口可达。用跳点、跨 tile、不可达及异步加载期的地图夹具与 BW-10 对照，记录旧 KBEngine 导航资源导入是否进入范围。

- [ ] **BW-29-01-01 核对 Recast/Detour 构建及脚本入口**。**验收** 记录可达性及目标。
- [ ] **BW-29-01-02 对照 KBEngine 平铺导航查询**。**验收** 跨 tile 与不可达样本有结果。
- [ ] **BW-29-01-03 对照异步地图加载**。**验收** 加载期调用状态有证据。
- [ ] **BW-29-01-04 记录旧导航资源导入决策**。**验收** 列明纳入或暂缓及理由。

#### BW-29-02 运维工具差异

按 `kbcmd`、GUIConsole、WebConsole、pycluster 分别核对进程发现、日志、状态、profile 和集群管理，记录对应 BW-23~27 或可选独立控制台需求。

- [ ] **BW-29-02-01 复核 kbcmd**。**验收** 为已确认命令关联任务或记范围项。
- [ ] **BW-29-02-02 复核 GUIConsole**。**验收** 为状态及管理工作流记录差距。
- [ ] **BW-29-02-03 复核 WebConsole**。**验收** 为日志、状态和 profile 记录差距。
- [ ] **BW-29-02-04 复核 pycluster**。**验收** 为集群管理工作流判定范围。

#### BW-29-03 存储与外部服务差异

分别复核 Redis 后端、`interfaces` 外部服务、订单/回调和邮件辅助的适用场景及启用条件。若纳入，逐项定义兼容性和端到端验收。

- [ ] **BW-29-03-01 复核 Redis 后端**。**验收** 记录构建可达性及纳入决策。
- [ ] **BW-29-03-02 复核 Interfaces 外部服务**。**验收** 记录入口、用途及取舍。
- [ ] **BW-29-03-03 复核订单与回调**。**验收** 记录端到端路径和范围。
- [ ] **BW-29-03-04 复核邮件辅助**。**验收** 记录依赖和范围。

#### BW-29-04 SDK 和 Python 开发差异

分别复核在线 SDK 下载、JavaScript 模板、Python assets 模板、类型存根。比对 BW-21/27 已覆盖工作流后登记剩余项。

- [ ] **BW-29-04-01 复核 LoginApp SDK 下载**。**验收** 登记支持传输与范围。
- [ ] **BW-29-04-02 复核 JavaScript SDK 模板**。**验收** 登记兼容性需求与取舍。
- [ ] **BW-29-04-03 复核 Python assets 模板**。**验收** 登记已有工作流和缺口。
- [ ] **BW-29-04-04 复核类型存根**。**验收** 登记生成及 IDE 接入范围。

#### BW-29-05 工作流差异

分别复核 `xlsx2py`、安装脚本和新工程脚手架。确认是否纳入，并为选中的功能登记样例输入、输出与平台。

- [ ] **BW-29-05-01 复核 xlsx2py**。**验收** 记录代表性表格及输出和范围。
- [ ] **BW-29-05-02 复核安装脚本**。**验收** 登记与 BW-14/15 的重合及差距。
- [ ] **BW-29-05-03 复核 assets 脚手架**。**验收** 登记可复用输入输出及范围。

## 4. 第二轮 · 保持兼容的优化

每项从同一版本的第一轮验收结果起步。使用相同拓扑、资源和脚本前后对比，报告 p50/p95/p99 tick 与 RPC 时延、稳态吞吐、CPU、RSS、带宽、数据库负载或对应工具耗时。基准环境、目标与性能阈值由 BW-00 实测确定。对外行为变更须另行版本化，并在混部与回滚用例中验证。

### OPT-01-01 基础运行时

优化时钟、队列、内存分配和资源缓存。比较 tick 尾延迟、分配次数和加载峰值，复跑时序与缓存失效用例。

- [ ] **OPT-01-01-01 测量基础运行时**。**验收** 保存 Tick、分配数和加载峰值基线。
- [ ] **OPT-01-01-02 优化时钟和任务调度**。**验收** 对比 Tick 尾延迟并复跑时序夹具。
- [ ] **OPT-01-01-03 优化分配和资源缓存**。**验收** 对比分配次数及加载峰值并复跑失效夹具。

### OPT-02-01 网络基线

评估缓冲复用、批量收发与减少复制。比较 RPC RTT、网络吞吐、丢包恢复和内存占用，混跑旧线协议。

- [ ] **OPT-02-01-01 测量网络基线**。**验收** 保存 RTT、吞吐和丢包恢复记录。
- [ ] **OPT-02-01-02 评估缓冲复用**。**验收** 报告分配与峰值内存。
- [ ] **OPT-02-01-03 评估批量收发和消除复制**。**验收** 报告吞吐与尾延迟。
- [ ] **OPT-02-01-04 复跑版本混部**。**验收** 原版和新版 UDP/TCP 请求均通过。

### OPT-03-01 Python 桥接基线

减少 Python/native 转换和跨语言回调开销，衡量 GIL 占用与回调耗时，复跑引用生命周期、异常及热重载。

- [ ] **OPT-03-01-01 测量 Python 桥接基线**。**验收** 保存转换、GIL 和回调耗时。
- [ ] **OPT-03-01-02 优化 Python/native 转换**。**验收** 以前后对照验证耗时变化。
- [ ] **OPT-03-01-03 优化跨语言回调**。**验收** 报告 GIL 占用与尾延迟。
- [ ] **OPT-03-01-04 复跑引用、异常及重载用例**。**验收** 行为一致。

### OPT-04-01 EntityDef 启动和编解码

缓存解析结果并评估预生成编解码器，衡量启动和序列化开销，锁定定义摘要和产出字节。

- [ ] **OPT-04-01-01 测量 EntityDef 启动和编解码**。**验收** 保存基线。
- [ ] **OPT-04-01-02 缓存定义解析**。**验收** 比较冷/热启动。
- [ ] **OPT-04-01-03 评估预生成编解码器**。**验收** 报告序列化耗时。
- [ ] **OPT-04-01-04 复跑定义摘要与字节夹具**。**验收** 产物一致。

### OPT-05-01 录制负载热点和故障基线

用热点和故障场景测量负载上报、切分/合并频率与恢复窗口，再调优调度策略。检查实体迁移、关服和混部稳定性。

- [ ] **OPT-05-01-01 录制负载热点和故障基线**。**验收** 记录分配、回收与恢复窗口。
- [ ] **OPT-05-01-02 调优负载上报与拆并**。**验收** 比较固定场景的调度效果。
- [ ] **OPT-05-01-03 复跑迁移、关服和混部**。**验收** 实体和进程状态正确。

### OPT-06-01 登录与资源下载基线

降低登录排队、会话接续和资源下载压力。比较登录峰值、拒绝率和尾延迟，复跑重复登录、重连与兼容回放。

- [ ] **OPT-06-01-01 记录登录与资源下载基线**。**验收** 保存峰值、拒绝率和延迟。
- [ ] **OPT-06-01-02 优化登录排队及会话接续**。**验收** 在登录洪峰场景比较尾延迟。
- [ ] **OPT-06-01-03 优化下载**。**验收** 比较传输时长及内存。
- [ ] **OPT-06-01-04 复跑重复登录、重连及旧客户端**。**验收** 行为一致。

### OPT-07-01 备份和归档基线

优化备份序列化、归档和共享数据传播。比较带宽、队列峰值、恢复时长及恢复后的实体状态。

- [ ] **OPT-07-01-01 记录备份和归档基线**。**验收** 保存带宽、队列及恢复耗时。
- [ ] **OPT-07-01-02 优化备份序列化和归档**。**验收** 按同负载比较队列与恢复。
- [ ] **OPT-07-01-03 优化 SharedData 传播**。**验收** 量化消息量变化。
- [ ] **OPT-07-01-04 复跑接管**。**验收** 恢复实体和共享状态正确。

### OPT-08-01 密集 AOI 基线

基于实测评估 AOI 索引、Witness 调度及安全的空间并行，衡量密集区 tick、广播量和跨边界迁移时长。复核可见集和事件顺序。

- [ ] **OPT-08-01-01 记录密集 AOI 基线**。**验收** 保存 Tick、广播量和迁移耗时。
- [ ] **OPT-08-01-02 评估 AOI 索引与 Witness 调度**。**验收** 报告候选收益。
- [ ] **OPT-08-01-03 评估空间并行**。**验收** 报告跨边界时序及耗时。
- [ ] **OPT-08-01-04 复跑可见集、事件顺序和迁移**。**验收** 状态正确。

### OPT-09-01 分别采集 MySQL 与 PostgreSQL 基线

优化连接池、查询合并、写入批次和二级库整合。MySQL 与 PostgreSQL 各自使用同一数据集比较优化前后的时延和吞吐，复跑事务、重试与恢复。

- [ ] **OPT-09-01-01 分别采集 MySQL 与 PostgreSQL 基线**。**验收** 使用同一数据集。
- [ ] **OPT-09-01-02 评估连接池及查询合并**。**验收** 报告各后端延迟和吞吐。
- [ ] **OPT-09-01-03 评估写批次与归档整合**。**验收** 报告提交和恢复时长。
- [ ] **OPT-09-01-04 复跑事务、重试与恢复**。**验收** 数据一致。

### OPT-10-01 地图加载和查询基线

优化空间流式装载、碰撞走查和导航缓存。比较峰值内存、场景冷启动和查询尾延迟，核对坐标与碰撞结果。

- [ ] **OPT-10-01-01 测量地图加载和查询基线**。**验收** 保存冷启动、内存及尾延迟。
- [ ] **OPT-10-01-02 优化空间流式加载**。**验收** 报告峰值内存及加载速度。
- [ ] **OPT-10-01-03 优化碰撞或导航查询**。**验收** 报告缓存和尾延迟。
- [ ] **OPT-10-01-04 复跑地图结果**。**验收** 路径点和碰撞结果满足原版夹具。

### OPT-11-01 三端固定镜头基线

优化各端 GPU 上传、渲染批次、资源流与客户端帧调度。比较帧时间和显存，用固定镜头做视觉回归。

- [ ] **OPT-11-01-01 记录三端固定镜头基线**。**验收** 保存帧时间和显存。
- [ ] **OPT-11-01-02 评估上传与渲染批次**。**验收** 按端对比帧时间。
- [ ] **OPT-11-01-03 评估资源流和帧调度**。**验收** 报告切图和长帧。
- [ ] **OPT-11-01-04 复跑视觉回归**。**验收** 固定镜头满足容差。

### OPT-12-01 各观测通道开销

降低日志、Watcher、Profiler 的观测成本。验证背压、关闭观测后的开销及新旧工具查询兼容。

- [ ] **OPT-12-01-01 记录各观测通道开销**。**验收** 分离日志、Watcher 和 Profiler。
- [ ] **OPT-12-01-02 降低观测成本**。**验收** 报告启用和关闭时的差值。
- [ ] **OPT-12-01-03 复跑背压与工具互通**。**验收** 查询及数据记录符合契约。

### OPT-13-01 冷构建、增量与编辑器加载

优化依赖缓存、并行转换和编辑器加载。比较冷构建、增量构建与大地图编辑时间，校验可复现产物。

- [ ] **OPT-13-01-01 记录冷构建、增量与编辑器加载**。**验收** 形成固定资产基线。
- [ ] **OPT-13-01-02 评估缓存与并发转换**。**验收** 比较冷/热耗时。
- [ ] **OPT-13-01-03 优化地图载入**。**验收** 比较编辑等待时间。
- [ ] **OPT-13-01-04 复核资产哈希和格式**。**验收** 可复现且原版可读。

### OPT-14-01 发布包尺寸及首次运行

优化发布包和 Demo 首次启动，衡量体积和安装耗时，复跑干净安装、升级及脚本导入。

- [ ] **OPT-14-01-01 记录发布包尺寸及首次运行**。**验收** 三端有基线。
- [ ] **OPT-14-01-02 优化包布局或首次启动**。**验收** 报告体积和耗时差异。
- [ ] **OPT-14-01-03 复跑干净安装与升级**。**验收** 保留数据且脚本成功导入。

### OPT-15-01 采集各平台 I/O 及定时精度

按操作系统测量 I/O、定时精度和图形后端。使用同一协议与功能套件复测各端行为。

- [ ] **OPT-15-01-01 采集各平台 I/O 及定时精度**。**验收** 统一负载下产出报告。
- [ ] **OPT-15-01-02 采集各图形后端性能**。**验收** 记录设备、驱动与帧时。
- [ ] **OPT-15-01-03 检验平台针对性优化**。**验收** 相同功能和协议套件通过。

### OPT-16-01 采集依赖与增量构建基线

根据第一轮依赖图调整过重或高度耦合的 crate，比较增量编译耗时，保留接口和安装路径兼容。

- [ ] **OPT-16-01-01 采集依赖与增量构建基线**。**验收** 定位重编源及耗时。
- [ ] **OPT-16-01-02 评估拆分高耦合模块**。**验收** 同修改前后重编耗时可比。
- [ ] **OPT-16-01-03 复跑加载和安装路径**。**验收** 外部接口与产物兼容。

### OPT-23-01 关闭及开启采集的长稳基线

比较长稳压测中开启与关闭采集时的 CPU、内存、tick 尾延迟和丢失记录，优化采集和导出开销。

- [ ] **OPT-23-01-01 测量关闭及开启采集的长稳基线**。**验收** 留存 CPU、RSS、Tick 及丢事件数。
- [ ] **OPT-23-01-02 评估采集缓冲和导出**。**验收** 按同场景报告开销变化。
- [ ] **OPT-23-01-03 回归采集完整性**。**验收** 预期序列、取消及溢出统计正确。

### OPT-26-01 固定事件回放并采集查询基线

以相同事件回放优化批处理和查询，比较导入速率、查询时延及内存占用，核对报表一致性。

- [ ] **OPT-26-01-01 固定事件回放并采集查询基线**。**验收** 保留导入、查询及内存数据。
- [ ] **OPT-26-01-02 评估导入批大小**。**验收** 报告吞吐和峰值内存。
- [ ] **OPT-26-01-03 评估查询聚合**。**验收** 报告延迟变化。
- [ ] **OPT-26-01-04 复算样本报表**。**验收** 窗口及计数与优化前一致。

## 5. 实施顺序与里程碑

| 编号 | 交付 | 验收 |
| --- | --- | --- |
| M1 | BW-00/16/18 基础、BW-15-01 平台接口、BW-19 VitePress 骨架、BW-22 CI 入口，推进 BW-01/02/03/04 | 原版基线和源到目标映射可追溯。三端持续构建，资源、协议、脚本 API 和实体定义可差分。 |
| M2 | BW-05 基础发现与注册、BW-06/07/09 最小闭环、BW-08 最小 Space、BW-17 Bots smoke、BW-21 SDK 核心 | 原版 Python 示例和新版无界面客户端均可登录、创建或加载角色、进入世界、写存档和重登。先用 MySQL 建立原版差分基准，再让新版 DBApp 在 PostgreSQL 上通过同一闭环。 |
| M3 | BW-05/07/08 完整拓扑，推进 BW-10/12/17/18、BW-22 压测回归与 BW-23/24/25 诊断 | 两个 BaseApp 和两个 CellApp 的集群可跑 AOI、切分、迁移、恢复及受控关闭。Bots 长稳运行可同时采集 profile、内存快照和集群状态。 |
| M4 | BW-10/11/13/14 产品交付、BW-09-06/07 PostgreSQL 和 BW-15 三端验收、BW-19/20/21/22/26/27/28 工具链发布 | 同一脚本和资源可在三端部署，PostgreSQL 通过矩阵和迁移测试。数据报表可复算，文档站、VSIX、SDK、LSP/DAP 集成和匹配版本的 MCP 索引有端到端验证记录。 |
| M5 | 第二轮 OPT 项 | 对每项记录功能验收和优化前后的同口径测量，并复跑变更触及的兼容用例。 |

任务结果登记字段为「四段式任务编号、原版构建目标/源码、Boundless 模块、平台、输入夹具、原版结果、新版结果、证据链接、状态」。仅具备静态证据或存在平台/依赖缺口的项目持续记录，等待实际可执行结果。
