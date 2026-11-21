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
- [x] **BW-00-03-02 固定 Rust、Python 工具链与依赖**。**验收** 本地与 CI 解析同一版本。
  - 原版：BigWorld 无工具链固定面——构建依赖系统里恰好装着的编译器与 Python 2.7，没有声明处，也没有任何「两端解析同一版本」的校验。
  - 新版：Rust 半边——`rust-toolchain.toml` 钉 `channel = "1.100.0"`（本席位接手时已在，**未重做**），`ci/toolchain-check.sh`（BW-22-01-01 落地）校验 pinned==resolved（rustup override 让每台检出机器强制解析 pin）、`[workspace.package] rust-version` 声明存在即须与 pin 一致（当前未声明，如实记 `<none>`）、Cargo.lock 入库且 `cargo metadata --locked` 一致、`--offline` 从锁文件冷注册表全仓可编译——依赖固定即锁文件入库这条。Python 半边——本条目补齐：新增 `.python-version` 钉 `3.12`（与 rust-toolchain.toml 同型的单一声明源），ci.yml 所有 `setup-python` 改读 `python-version-file: .python-version`（原先三处各自硬编码 `3.12`，会各自漂移），toolchain 作业补 Python 步骤使 lane 里的 python3 就是 pin 解析出的解释器，lane 新增 python 腿：pin 文件缺失/空 → FAIL；**CI 里 python3 与 pin 不一致 → FAIL**（setup-python 已按文件解析，不一致只可能是 setup 步骤被删或镜像漂移）；本机默认 python3 不认 pin 文件（hermes 托管的 3.14.9）→ 两边版本如实写进 `toolchain.json`（`python_pinned`/`python_resolved`）并注明谁认这个文件，不假造本地一致。
  - 结果：本地 `ci/toolchain-check.sh --offline` rc=0（`python_pinned=3.12 python_resolved=3.14.9` 记录在案，rust 半边 pinned==resolved=1.100.0、members=10、offline build resolved）；`shellcheck -S warning` 与 `actionlint` 双绿。
  - 证据：CI run **44796370170**（HEAD `b45f330`）：`Toolchain pin and lockfile` **success**，Python 步骤照录 `Successfully set up CPython (3.12.16)`，lane 行 `python_pinned=3.12 python_resolved=3.12.16`——**CI 侧解析与 pin 逐字一致**，「本地与 CI 解析同一版本」在受管环境成立且有硬门；rust 半边的 CI 记录自 BW-22-01-01 起 toolchain 作业持续绿（run **44749719511** 起）。同 run 其余自有作业全 success，唯一红仍为 `rust-lint` 历史债。本地等价命令 rc=0。
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

- [x] **BW-02-01-01 实现 Endpoint 地址与 socket 操作**。**验收** IPv4/适用 IPv6 夹具通过。
  - 证据：IPv4 夹具通过（`127.0.0.1`/`0.0.0.0`/`255.255.255.255`/主机名 双路 `convert_address`、UDP 双向 sendto/recvfrom 四参重载、TCP listen→connect→accept 回传对端 ip/port、`recvAll` 满缓冲与 EOF 双分支、缓冲读写与不可满足请求回 false、错误队列 ECONNREFUSED+offender、`getnameinfo` 回解 loopback）；**IPv6 不适用**——参考源码全库无 `AF_INET6`/`IN6`（`grep` 验证），`Endpoint` 只建 `AF_INET`，`Address` 是 32 位 `ip`+16 位 `port`，无 V6 字段可承载；故 V6 夹具无对应参考行为，不假造。
  - 证据：`crates/platform/src/socket.rs` 是 OS 调用唯一落点（BW-16-02-01）：`socket/bind/connect/listen/accept/sendto/send/recvfrom/recv/getsockname/getpeername/close/setsockopt/getsockopt/FIONREAD/F_GETFL+F_SETFL/recvmsg(MSG_ERRQUEUE)/inet_aton` 等价物/getnameinfo。内核约定按引用钉死——地址是**主机序 u32**（`127.0.0.1` = `0x0100_007f` = `htonl(0x7F000001)`，与 endpoint.cpp 的 `lo` 地址一致），端口**全程保持网络序**（`bound.port` 直接回填 `sendto`，不做二次转换）；`FIONREAD`/`SO_*`/`MSG_ERRQUEUE` 常量同源。`setnonblocking` 用 `F_GETFL` 读回原标志再 `F_SETFL`（引用直写 `O_NONBLOCK` 会清掉其余标志，属偏离，已在文档注释里注明）。
  - 证据：`crates/mercury/src/endpoint.rs` 移植 `Mercury::Address`（basictypes.hpp/.ipp）与 `Endpoint`（endpoint.hpp/.ipp/.cpp）：`Address{ip,port,salt}` 全网络序、`salt` 恒为 0 且**不参与 ==/!=/<（手工 PartialEq/Eq/Ord，派生实现会把 salt 算进去——偏离已注释）**；`write_to_string` 按 `snprintf` 语义返回全长而写 min；`c_str`/`ip_as_string` 返回自有 `String`（引用靠两个轮换静态缓冲规避生命周期，Rust 所有权下该危险不存在，轮换随之消失——偏离已注释）；`NONE`/`is_none()` 只看 ip。
  - 证据：`Endpoint` 语义逐条对齐：`close` 在无 socket 时回 0、**仅在 close 成功时清 fd**（专测双 close 路径钉死「失败的 close 保留描述符」）；`detach` 只交出 fd 不关；`Drop`→`close`（故结构体不 `Clone`，已注释）；`getlocaladdress` **先清零出参**而 `getremoteaddress` 不清（两个测试分别钉死）；`recvfrom` 先清零且仅在结果≥0 时回写出参；`setbuffer_size` **set 后回读比较**（`get_buffer_size >= size`），故内核放大缓冲仍报 true、超大请求回 false；`recvall` 循环至满，len==0 打 "Connection lost"、len<0 打 "Got error"；`accept` 失败打 `"Endpoint::accept: accept failed (%s) for socket %d"`；`getbuffersize` 用 `debug_assert` 对应引用 `MF_ASSERT`。`readFromErrorQueue` 拆成 `find_recv_error`（走 cmsg 找 `IP_RECVERR`）+ `parse_queued_error`（读 `sock_extended_err`），以便用夹具驱动内核 bug 分支。
  - 证据：错误队列夹具经真实内核字节校准——先 dump `recvmsg(MSG_ERRQUEUE)` 的真实控制字节（`30 00…` 头 + `6f 00 00 00` = errno 111 在 `CMSG_DATA` 起点），确认内核把 `sock_extended_err` 放在数据**首部**（不是 offender 在前），据此修正夹具布局；`msg_namelen==0` 时走 `SO_EE_OFFENDER`（`(ee)+1`，即结构体后 48 字节）且**端口强制 0**（引用注释 "Kernel has a bug: recv_msg did not set msg_name."），两条分支均由夹具钉死。
  - 证据：范围界定——`getQueueSizes`（NetworkStats 读 `/proc/net/udp`、400ms 缓存）与 `getInterfaces`/`findDefaultInterface`/`findIndicatedInterface`/`getInterfaceFlags`/`getInterfaceAddress` 属 BW-02-01-03（NetworkInterface 派发）落地，本条不实现以免越界；`initNetwork`/`finiNetwork` 的 SIGPIPE 安装同属分发侧。
  - 证据：并行测试抖动两处根因逐一钉死。**(1) 释放即复用**：原先失败路径用「真 socket → `detach` → `close` → 把死号回填」驱动，`close` 一释放描述符号就被**并行测试线程的 `socket()` 立刻复用**，endpoint 存的号于是指向别人的对象（`local()` 读到他人真实地址、`recv_all` 报 `ENOTSOCK`(88)）。`--test-threads=1` 8/8 干净、并行 ~15-20% 失败即此。修法：`platform::socket::descriptor_limit()`（`getrlimit(RLIMIT_NOFILE)`，内核永不发放的号）+ 测试助手 `dead_fd()`，失败路径全改用之；`FIONREAD` 在管道上同样成功，故 `num_bytes_available` 的 -1 分支同改。**(2) 双持有（真正的连环杀手）**：`adopted` 与 `dead` 先后都指向 `raw`（`dead.set_file_descriptor(raw)`），两个 `Drop` 都去 close 同一个号——**第二次 close 落到别的测试线程刚开进的号上**。用临时 DIAG 抓到铁证：受害测试 `ep.fileno()=3` 而 `getsockopt` 回 `EBADF`、`snd=-1/rcv=-1`（fd 3 是最低空闲号，正是被误关的 `buffer_sizes` socket），同时解释了既见 `snd=1048576/rcv=212992`（别人的 socket）又见全 -1 的两种症状。修法：`adopted` 先 `detach()` 归还，`raw` 全测试只有单一持有者。
  - 测试与覆盖：新 20 测试（mercury）+ 16（platform）。全 workspace `cargo test --no-fail-fast` **502 通过 / 0 失败 / 0 警告**；`cargo build --workspace --all-targets` 0 警告；`cargo clippy -p mercury -p platform --all-targets` 对两 crate **0 警告**（顺带清掉 5 个 clippy 建议：`Default` 改 derive、`as_deref_mut` 在单次使用处去掉，两处「先清零再回填」的双段函数因借用两次必须保留 `as_deref_mut`，已留注释说明）。llvm-cov：platform/socket.rs **100%** 行、mercury/endpoint.rs **99.69%** 行、TOTAL **99.83%**，全 ≥98% 门。抖动复现：修 (1) 后 60 次并行跑仍 3 次失败（run 23/40/57，含 2 次整 run 挂死被 timeout 杀掉）；修 (2) 后累计 **140 次**并行全量跑（60+40+40，含 clippy 整理后复跑）**零失败**（原并行失败率 ~15-20%）。
- [x] **BW-02-01-02 实现轮询器注册、注销与唤醒**。**验收** 空闲轮询可被正常停止。
  - 证据：分层与门控——`crates/platform/src/poller.rs` 只装 `epoll_create1`/`epoll_ctl`/`epoll_wait`（OS 调用唯一落点，BW-16-02-01）+ `PollEvent`；`crates/mercury/src/poller.rs` 装 `EventPoller` 本体与 `EpollBackend`。两模块均 `#[cfg(target_os = "linux")]`，对应参考 `event_poller.cpp:21` 的 `#define HAS_EPOLL`（`__linux__ && !EMSCRIPTEN`）；参考的 `SelectPoller`/`PollPoller` 后端不落地，非 Linux 无对应。
  - 证据：抽象基类压平——参考 `EventPoller` 是抽象基类，四个 `doRegisterFor{Read,Write}`/`doDeregisterFor{Read,Write}` 由后端覆盖，基类在决定 ADD/MOD/DEL 时**上回调** `isRegistered(fd, !is_read)`。Rust trait 对象派发做不了上回调，故压平为 `EventPoller` 结构体（双 handler map + 派发 + spare-time 记账）+ `PollerBackend` trait 对象（OS 臂），上回调改成 `opposite_registered: bool` 入参。handler 按 `Rc<dyn InputNotificationHandler>` 共享，与 `server-runtime/src/timers.rs` 既有风格一致。
  - 证据：注册/注销/唤醒语义——`register_for_{read,write}` **先问后端 hook、失败则不入表**（`register_maps_track_backend_result` 钉死：失败后 `is_registered` 仍 false）；`deregister_for_{read,write}` 先删表再问后端。`EPOLL_CTL` 选择逐分支对齐参考：对向已注册 → `CTL_MOD`（注册取 `EPOLLIN|EPOLLOUT`；注销读留 `EPOLLOUT`；注销写留 `EPOLLIN`，即保留对向），否则 `CTL_ADD`/`CTL_DEL` + 单方向事件。`both_directions_and_selective_removal` 在同一 fd 双向注册后分别注销读、注销写，两个 MOD 臂都在真实管道上验证事件仍可达（读兴趣移除后写入不再上报、重新注册后写入可读）。
  - 证据：失败分级——参考 `doRegister` 按 `errno == ENOENT` 分 `WARNING_MSG`（本就该没有，不算错）与 `ERROR_MSG`（event_poller.cpp:1096）。两个 errno 需**不同夹具**才分得开，这是本轮踩出的覆盖缺口：`dead_fd()`（`descriptor_limit()`，内核永不发放的号）只给 `EBADF` → ERROR 臂；WARNING 臂要一个**打开着但从未注册**的 pipe 端才回 `ENOENT`。
  - 证据：唤醒与 CRITICAL——`EventPoller` 自己实现 `InputNotificationHandler`：`handle_input_notification` 跑一趟零超时 poll（父 dispatcher 把本 poller 的 fd 注册进它自己的轮询即成唤醒通道），`is_ready_for_shutdown` 转发到同一 conjunction 方法（固有方法与 trait override 两条路径分别有测试）。参考 `EPoller` 构造失败用 `CRITICAL_MSG`（`CriticalMsg::writeAndAssert` 会 abort，`cstdmf/log_msg.cpp:1036`），与仓库既有约定一致移植为 panic（同 `server-runtime/src/tick.rs:580` 的 CRITICAL 站点），测试 `#[should_panic(expected = "EPoller::EPoller: epoll_create failed")]`。
  - 证据：验收（空闲轮询可被正常停止）——`process_pending_events(0.0)` 无任何注册时**立刻**回 0（不阻塞、不忙等）；0.05s 有界等待把超时**等完**（实测 ≥40ms）并把等待计入 `spare_time`（dispatcher 空闲记账），`timeout_ms = (maxWait*1000).ceil()` 对齐参考取整。
  - 证据：偏离记录——参考 `EPoller(expectedSize = 10)` 调用的是**已废弃**的 `epoll_create(size)`，该 size 在现代内核上只是建议值；移植改用 `epoll_create1(0)`（无 size 形式），`create(10)` 保留形参并 `let _ = expected_size;` 注明。参考 `MAX_EVENTS = 10`（event_poller.cpp:1117）是单轮事件缓冲上限，与那个默认 10 数值相同但来源独立；移植用 `[PollEvent; 10]` 固定缓冲（`PollEvent` 是 Copy，可传值给 `&mut [PollEvent]`）。
  - 证据：覆盖债清理——首版 97.37% 未达 98% 门，缺口逐条钉死：trait 默认实现（补 `BareHandler`，只实现 `handle_input_notification`）、`EPoller` 构造失败臂（抽出 `from_fd` 使失败态可构造，并由此发现该臂应移植为 panic 而非打印）、MOD-保留-`EPOLLIN` 臂（补镜像方向的注销用例）、WARNING/`ENOENT` 臂（补「打开但未注册」的 pipe 端）、`EventPoller` 的 trait override（补经 `dyn` 的调用）。
  - 证据：并行稳定性——本条新增的管道/epoll 夹具在并行下累计 **65 次**全量跑（platform+mercury：40 + 25）**零失败**。中途另有一次 30 次循环中 1 次 run 挂死：孤儿进程阻塞在 `__skb_wait_for_more_packets`（阻塞 socket 收包），同现象 mercury 侧**早于本次改动**即存在（同日早一次 run 即如此），已 kill 孤儿并给每 run 加 `timeout` 兜底；platform 侧 20 个测试逐个单跑（10s 时限）**20/20 通过**，故挂死不在任一单测内，属并发夹具的既有残余风险而非本次引入。
  - 测试与覆盖：新 16 测试（mercury）+ 4（platform）。全 workspace **522 通过 / 0 失败 / 0 警告**；`cargo clippy -p platform -p mercury --all-targets` 对两 crate **0 警告**（余下 2 条属 foundation 既有）。llvm-cov：mercury/poller.rs **100%** 行与函数、platform/poller.rs 98.57%（唯一未覆盖行是 `epoll_create1` 失败臂——已改 panic，无存活分支）、endpoint.rs 99.69%、TOTAL **99.78%** 行，全 ≥98% 门。
- [x] **BW-02-01-03 实现 NetworkInterface 派发**。**验收** 正确接口接收对应事件。
  - 证据：范围界定——每接口自带 `EventPoller` 作子派发器替身（参考里每个 `NetworkInterface` 持有自己的 `EventDispatcher`；完整 dispatcher/timer/task 归 BW-02-01-05），验收「正确接口接收对应事件」即每接口独立 poller 的按接口路由；`registerWithMachined`/`deregisterWithMachined` 无 machined 可谈，不落地；`MF_SERVER` 缓冲配置块 → BW-02-01-04；信道表、死信道记账 → BW-02-03（本轮以 `ChannelRef`/`ChannelFinder` 两个 seam 预留 `findChannel`，`None` 时安静）；限速器 → BW-02-05-04；接口表（消息分派、回复元素）→ BW-02-02；`PacketReceiverStats` → BW-02-01-05。
  - 证据：poller 重构配合接口所有权——接口在 `Rc` 化后仍需 `recreateListeningSocket`/`closeSocket`，故 `EventPoller` 的注册表改 `RefCell<BTreeMap<RawFd, InputHandlerEntry>>`、注册/注销取 `&self`（同 fd 同向重复注册幂等）；派发前把 handler 条目**先 lift 出表再调用**，回调内重注册/注销安全（测试钉死回调内注销不再回弹）。`NetworkInterface` 字段 `RefCell<Endpoint>`/`RefCell<Address>` 配合 setup 路径 `&self` 可调用；`PacketReceiver` 持 `Weak<NetworkInterface>`（事件时 upgrade，无引用环，`an_unattached_receiver_is_quiet` 钉死未挂接时的安静语义）。
  - 证据：参考顺序保真——`recreateListeningSocket`（network_interface.cpp:339-467）逐步对齐：closeSocket → 清地址 → 建 socket（失败 ERROR）→ **先 `registerForRead` 后 `bind`**（cpp:359 的原顺序，失败 bind 留下注册条目，注释注明按原样移植）→ 接口 spec 解析（四臂全覆盖：可解析/找到无地址/不可解析非空/空 spec，后两臂证明从不查询地址）→ bind（失败 ERROR+close+detach+false，`a_failed_bind_leaves_the_interface_not_good` 用占口 socket 驱动）→ `local_address` 回填 → INADDR_ANY 走默认路由解析（抽出 `resolve_default_route_address` 使两个失败臂可用假查询驱动；成功臂用真路由表 `an_any_bind_advertises_the_default_route` 钉死广告地址≠0）→ 非阻塞 + 错误队列开启。
  - 证据：`/proc/net/udp` 解析（NetworkStats，BW-02-01-02 范围界定的落地）——内核列约定钉死：ip 列是**内存字原样 hex**（`0100007F`=127.0.0.1 不动）、port 列是**主机序 hex**（`9C41`=40001，需 `htons` 回网络序入键）；偏移取法按参考：首数据行 `find(": ")`+2 得地址列、三次单空格推进得 tx:rx 列，后续行全用同一偏移；`strtoul` 语义（前导 hex 串、溢出饱和 `u64::MAX`）；GC 规则 `map.len() > 2*last_active_record_count` 先清再读（`the_map_gcs_when_it_outgrows_its_records`）；`Endpoint::getQueueSizes` 返回 0/-1 而 `transmit/receiveQueueSize` **无视查找结果**恒答 0 起（endpoint.ipp:726,740，`a_live_socket_reports_its_queues` 双向钉死）。
  - 证据：时钟根因——测试两处「缓存刷新断言」偶发失败的根因是 `platform::clock::monotonic_now_micros` 为**进程相对**时钟（首调用冻结原点，进程早期 now≈1µs），而参考 `timestamp()` 自开机计数、零值 `lastUpdate_` 恒旧于 400ms；直接照抄 `saturating_sub` 门会漏掉首查刷新（实测 `now=1 last=0 age=1`）。修法：`last_update_micros == 0` 显式携带「从未刷新」态（与参考行为等价、注释写明差异）；测试钩子 `reset_for_test` 改为**当场读一次内核文件**——兄弟测试共享单例，仅置零时间戳会被并行线程抢先刷新留下空表。
  - 证据：内核行为三则（error 路径测试的依据）——**(1)** 挂起 ICMP 端口不可达使下一次 `recvfrom` 以 `ECONNREFUSED` 失败**且**（IP_RECVERR 下）向错误队列塞副本，故 `EPOLLERR|EPOLLIN` 同报，poller 路由到 `handleErrorNotification`：**只标记信道死、不 report、不调回调**（`a_refused_send_marks_the_channel_dead` 钉死 reports 为空）；**(2)** 70k 超大 UDP send 在本内核**同步返回 EMSGSIZE 且不入错误队列**，该臂经直接调 `dispatch_error_notification` 驱动；**(3)** 内核对同目标地址**限速 ICMP 目的不可达**，毫秒级第二次拒收等不到报错（注释注明，callback 侧由直接派发用例覆盖）。读路径拒收（`handleInputNotification` 直呼、绕开 EPOLLERR 路由）经 `a_refused_read_reports_no_such_port` 钉死：`checkSocketErrors` 进入 errno 分支 → 队列读成功 → 标记+`REASON_NO_SUCH_PORT` 报告。
  - 证据：`checkSocketErrors`/`handleErrorNotification` 全分支——len==0 报 GENERAL_NETWORK；安静 `EAGAIN`（非首轮）静默 false；EAGAIN|ECONNREFUSED|EHOSTUNREACH → 读错误队列 → REFUSED 走「标记+回调+NO_SUCH_PORT」、其余走「WARNING+GENERAL_NETWORK」，队列读失败落底部兜底警告；`onConnectionRefusedTo` 只标记**未死的外部可见内部信道**（FakeChannel 以 marks 计数使「已死/外部/无信道」三臂均可观察）；`onMTUExceeded` 只打印不杀信道；`processPacket` 非成功 reason 经 `RefusingSink` 钉死上报臂；预算断流（`max_socket_processing_time`，非正值禁用、1µs 必超）与警告文本对齐参考。
  - 证据：验收——`the_correct_interface_receives_its_event`：两个接口各自绑定、各自发入一包，只轮 A 时 A 的 sink 恰见自己那包而 B 为空，再轮 B 时 B 恰见自己的而 A 仍一包不增；辅以 `an_interface_listens_where_it_is_told`（绑定即注册于自身 poller、仅读向）与 `close_socket_deregisters_and_detaches`/`recreate_replaces_the_socket`（生命周期全程 poller 表跟随真实 fd）。
  - 证据：偏离与测试债——`Reason::isSuccess` 无使用方已删；`PacketReceiver::default` 无使用方已删（`new()` 自建字段）；misc 字符串表取址改直接下标（C 的越界防护防的是「枚举持任意 int」，Rust 枚举不可表示，注释注明）；平台侧 `conf` 加 `conf_buffer_sized` seam 使「首缓冲填满→倍增重试」可驱动（对参考固定 1024 字节单次填充是已注释的偏离），`conf(-1)` 钉死 ioctl 失败臂；`clippy` 4 条清理（`mem::take`、两处 let-chain 合并、`map_or` 改 match）。
  - 证据：残留未覆盖行（均为环境不可驱动，非逻辑债）——mercury/network_interface.rs `121-124`（socket 创建失败需 fd 枯竭，与并行 fd 纪律冲突故不驱动）、`176-178`（INADDR_ANY 且无默认路由的失败臂，本机必有默认路由）；platform/interface.rs `97`（`if_nameindex` 返回 null）、`134`（SIOCGIFADDR 成功但非 AF_INET）；packet_receiver.rs 5 行为花括号区域残影（紧邻已覆盖代码）。
  - 测试与覆盖：全 workspace **569 通过 / 0 失败 / 0 警告**（mercury 76 + platform 27 为本轮交付面）；`cargo build --workspace --all-targets` 0 警告；`cargo clippy -p platform -p mercury --all-targets` 两 crate 0 警告（余 2 条属 foundation 既有）。llvm-cov：misc/packet/mercury-poller **100%** 行、network_stats 98.53%、packet_receiver 99.12%、network_interface 96.79%、platform/interface 98.93%、TOTAL **99.24%** 行 / **99.02%** 函数，全 ≥98% 门。并行稳定：`timeout 120` 兜底下 **40 次**全量并行跑 **0 失败**。
- [x] **BW-02-01-04 应用 socket 缓冲配置**。**验收** 成功和不可满足时的结果吻合。
  - 证据：三件套对齐参考——`getMaxBufferSize`（network_utils.cpp:91）落 `crates/mercury/src/network_utils.rs`：读 `/proc/sys/net/core/rmem_max`/`wmem_max`，读不开 WARNING `Unable to read buffer size from: %s, error:%s` 保默认、`fscanf("%d")` 语义（首个空白分隔 token 解析整数）失败也保默认（BIGWORLD 3.13 迁移注释的行为），默认值即 basictypes.hpp:78-79 的 `MIN_SND_SKT_BUF_SIZE=1048576`/`MIN_RCV_SKT_BUF_SIZE=16777216`；`NetworkInterface::recvBufferSize_`/`sendBufferSize_` 静态初始化以 `OnceLock` 对应（`buffer_sizes()`，首用算一次）；`MF_SERVER` 块（network_interface.cpp:449-462）落 `apply_server_buffer_sizes`（本移植即 server，块无条件；`__APPLE__` SO_NOSIGPIPE 无 Linux 对应不落）——`setBufferSize(SO_RCVBUF)`→失败 WARNING `Operating with a receive buffer of only %d bytes (instead of %d)`（实际 granted 用 `getBufferSize` 回读），SNDBUF 同型，插在 `enableErrorQueue` 之后、machined 注册（无 machined 不落）之前。
  - 证据：验收两侧——**成功侧**：本盒 rmem_max=16777216、wmem_max=1048576，恰好等于两个 MIN 常量，`recreate_applies_the_wanted_buffer_sizes` 端到端钉死 recreate 后接口 socket 的 SO_RCVBUF≥16MB、SO_SNDBUF≥1MB（内核默认 212992，抬升可观察）；`setBufferSize` 语义是「setsockopt（结果无视）后回读 ≥ 请求才真」（endpoint.cpp:604-613），BW-02-01-01 已落 `Endpoint::get/set_buffer_size` 与 platform `get/set_int_option`，本轮不重复。**不可满足侧**：`impossible_buffer_requests_warn_and_fall_back` 直驱 1 GiB 请求——内核封顶 rmem_max，回读 < 请求 → 双双 (false,false) + 两条 WARNING（helper 返回 `(bool,bool)` 使两臂可断言）；endpoint 层 1<<30/i32::MAX 拒收早有测试钉死。
  - 证据：network_utils 测试四则——真 procfs 值等于读文件解析（钉住两路路径）；不存在路径保默认；不可解析文件（"not a number"）保默认、`"  7 junk"` 按 fscanf 读到 7；`buffer_sizes()` 与两次单查相等。network_utils.rs 覆盖 **100%**。
  - 证据：挂死根因三连（BW-02-01-02 遗留残余这次钉死）——stress 中 `udp_round_trip_and_local_address` 两次 60s+ 挂死，加 SO_RCVTIMEO 后**仍挂**，直接跑平台测试二进制抓到铁证：**测试自己的尾巴 `close(fd)` 后立刻 `recv(fd)` 期待 EBADF，但这个号可在 close 与 recv 之间被并行测试线程的 `socket()` 立刻复用**——recv 于是阻塞在**别人的** socket 上（对方无超时 → 永挂；或读到对方数据 → run 76 铁证 `recv` 回 0 而非 -1，panic `left: 0 right: -1`）。修法：尾巴改用 `descriptor_limit()`（内核永不发放的号，与 BW-02-01-01 的 dead_fd 纪律同源），blocking 等待点加 `SO_RCVTIMEO` 5s（`set_timeout_option` timeval 封装 + SO_RCVTIMEO/SO_SNDTIMEO 常量，accept 在 Linux 上同受 SO_RCVTIMEO 约束）使丢包场景以带 errno 的失败暴露而非挂死。追认：`create_pipe` 的内核拒绝臂（pipe 失败需 EMFILE，与 fd 纪律冲突不驱动）为既有已接受缺口。
  - 测试与覆盖：全 workspace **575 通过 / 0 失败 / 0 警告**（mercury 82 + platform 27 为本轮交付面：network_utils 新 4 测试、network_interface 新 2 测试）；`cargo build --workspace --all-targets` 0 警告；`cargo clippy -p platform -p mercury --all-targets` 两 crate 0 警告。llvm-cov：network_utils **100%**、TOTAL **99.26%** 行 / **99.05%** 函数，全 ≥98% 门。并行稳定：挂死修复后 platform 二进制直跑 **100 次** `--nocapture` 全绿，随后全量 `cargo test` **60 次**并行 **0 失败**（连同修复前累计 140 次中 2 次挂死均根因如上并已消除）。
- [x] **BW-02-01-05 对照 dispatcher 单轮顺序**。**验收** 定时器、频繁任务及网络事件排序吻合。
  - 证据：**单轮顺序（验收核心）**——`processOnce` 逐段对齐参考 event_dispatcher.cpp:340-545：重置 break 旗标 → `processFrequentTasks` →（未 break）`processTimers` → `processStats`（**无条件**，break 时也跑）→（未 break）`processNetwork(shouldIdle)`。验收测试 `a_single_round_runs_frequent_tasks_then_timers_then_network` 钉死日志序 `["frequent","timer","network"]` + `numEvents==1`；构造默认值同参考（`maxWait_=0.1`、`networkTimeBudget_=TimeStamp::fromSeconds(0.01)`）。
  - 证据：**break 短路**——`a_break_skips_timers_and_network_but_not_stats`：频繁任务内 break 后定时器与网络均跳过、stats 仍跑；以「epoll wait 次数不变」反证 `processNetwork` 未进入，随后干净一轮得 `["frequent","frequent","timer","network"]`。
  - 证据：**时间戳单位不可混**——mercury `EventDispatcher` 用**微秒**（poller 的 spare_time 以微秒计费，参考把 poller 空闲时间直接从 elapsed 里减掉，混用纳秒会差 1000 倍）；server-runtime `DispatcherTimers` 保持纳秒。真实时钟 `foundation::time::stamp_now()` 对应参考 `timestamp()`（进程内原点冻结的单调纳秒），server-runtime `tick::stamp_now` 改为委托之 → 单一真源。
  - 证据：**分层搬家**——`TimeQueueT` 属 cstdmf（两个 dispatcher 共用），自 `server-runtime/src/timers.rs` `git mv` 至 `foundation/src/time_queue.rs`，server-runtime 以 `pub use foundation::time_queue as timers;` 保留 `crate::timers::…` 全部路径（迁移后 server-runtime 125 测试原样全绿、无行为变化）；mercury 新增 foundation 依赖。随迁的两个跨模块测试移回 `dispatcher.rs`。
  - 证据：**FrequentTasks 容器语义**（event_dispatcher.rs + frequent_tasks.rs）——add/cancel 置 dirty；`process` 在 dirty 后停本轮（`a_modified_container_stops_the_current_pass`，加在队尾故次序为 `["mutator","mutator","follower","late"]`）；嵌套 `process` 在 dirty 时拒绝、在 clean 时从头跑完整一轮（参考 `wasInProcess` 两臂，`a_nested_pass_restarts_clean_and_refuses_dirty`）；`cancel` 按身份移除并回报是否注册；参考的 `pGotDestroyed_` 逃逸旗标在 Rc 下不可表达（容器随最后一个共享句柄存活、pass 调用前克隆任务），已按扁平化记录。
  - 证据：**dispatcher 耦合**——`DispatcherCoupling` 构造即把自己加入父的 frequent tasks，`doTask` → `child.process_once(false)`（参考 dispatcher_coupling.hpp）；`attachTo` 在 `fd != -1` 时把子 poller fd 的 **read+write** 注册到父（真实 epoll 测试 `a_parent_pass_drives_the_child_round_in_order` 断言父 poller 已注册子 fd）；`detachFrom` 显式取消耦合（参考靠析构取消，已扁平化）；`childDispatchers_` 落 `Vec<Weak<EventDispatcher>>`，子被丢弃即从父消失（升级失败 no-op、`calculate_wait` 跳过、`detach` 保留死项），`a_dropped_child_vanishes_from_the_parent` 覆盖全部四臂。
  - 证据：**等待与预算**——`calculateWait = min(maxWait_, 自身下一定时器, 各子 dispatcher 的回答)`（`calculate_wait_caps_at_max_wait_and_next_timer` 钉 50/100ms 两次、`a_child_timer_caps_the_parent_wait`）；`processNetwork` 用**真实时钟** elapsed 减 poller 空闲后与 `maxNetworkTime_`/超预算计数对齐：真 50ms epoll 空闲 → over_budget 0 且 `max_network_time < 10_000`（`an_idle_wait_stays_inside_the_budget`），Busy 15ms → 计入超预算（`an_expensive_tick_counts_as_over_budget`）。stats 按「每秒一次」滚动（acc/old/tot 三值 + `proportionalSpareTime`）。
  - 证据：**停机排水**——`prepareForShutdown` 按**真实时钟** 2s 边界、100ms sleep 直至 `poller.is_ready_for_shutdown()`：`the_drain_runs_until_the_poller_becomes_ready`（ReluctantHandler 两次后才 ready）与 `a_never_ready_poller_is_bounded_by_two_seconds`（≥2s 且 <4s，证明有界不死等）。
  - 证据：**范围说明**——参考的 C++ 异常缓冲（循环后 `reportPendingExceptions`）未移植，Mercury 错误按发生即上报为 Reason；这是唯一有意偏离，不影响单轮顺序。
  - 证据：**抖动根因与修复**（stress 中 server-runtime 1/40 失败）——signals 测试套件自身的**进程级位集共享缺陷**：`sigquit_handler_fires_immediately` `raise(SIGQUIT)` 置位 `1<<3=8` 从不清理，而 `ignore_signal_swallows_raises` 断言整个 `PENDING_SIGNALS == 0`，并行下两者先后成 1/40 的硬币（`left: 8 right: 0` 铁证）；同模块 `handle_signal_marks_the_pending_bit` 早已注释过这一隐患（「The global set is shared」）。修法：改为断言**增量**（raise 前后位集不变，窗口内持 `disposition_guard` 故无干扰）+ 用完即清位，与既有 `store(0)` 惯用法一致。隔离直跑该测试 0/100 证明非逻辑问题；修复后全量 0/100。
  - 测试与覆盖：全 workspace **590 通过 / 0 失败 / 0 警告**（mercury 107 + foundation 127 为本轮交付面，event_dispatcher 新 20 测试）；`cargo clippy` foundation / mercury / platform **0 警告**，server-runtime 自身 **0 警告**（其输出 34 条全部归属 resources，该 crate 本轮未触碰且既有 clippy error `read amount is not handled` 使整批中止，属已知既有面）。llvm-cov（四 crate）：event_dispatcher.rs **100.00%**、time.rs **100.00%**、frequent_tasks.rs 99.33%（1 行花括号残影，不在 Uncovered Lines 列）、time_queue.rs 99.81%、dispatcher.rs 99.64%、tick.rs 99.00%、signals.rs 98.73%、TOTAL **99.47%** 行 / **99.58%** 函数，交付面全 ≥98% 门。并行稳定：`timeout` 兜底下 mercury **0/100**、foundation **0/40**、platform **0/40**、server-runtime 修复后 **0/100**。

#### BW-02-02 接口注册和 RPC

从接口定义及宏展开提取消息 ID、固定或可变长度、handler、请求/应答 ID 与超时。比对内部和外部接口的字节序、包头和请求回调。

- [x] **BW-02-02-01 提取内外部消息 ID 与长度表**。**验收** 同原版接口注册表比对。
  - 证据：**注册表形态（验收核心）**——原版**无生成式中央注册表**，注册表即宏展开的 `InterfaceMinder` add 序列：`BEGIN_MERCURY_INTERFACE(Name)` 建 `InterfaceMinder gMinder("Name")`，`MERCURY_MESSAGE(NAME,STYLE,PARAM,HANDLER)` 展开为 `gMinder.add(#NAME, STYLE, PARAM, NULL_IF_NOT_SERVER(HANDLER))`，消息 ID 是**声明顺序的连续序号**（`add` 里 `id = elements_.size()`），之后 `registerWithInterface` 把每个元素拷入该 socket 的 256 槽 `InterfaceTable`。移植落点：`interface_minder.rs` / `interface_table.rs` / `interface_element.rs`、`misc.rs` 的 `MessageID = u8`、`interfaces.rs` 的 `InputMessageHandler`。
  - 证据：**内外部并非两套 ID 空间**——区分只在**哪张表**：`NETWORK_INTERFACE_INTERNAL` / `EXTERNAL` 各持一张 `InterfaceTable`，同 id 在两张表里可指不同元素。`each_socket_owns_its_own_table` 以 CellAppInterface（internal：id0=shutdown、id1=handover）对 BaseAppExtInterface（external：id0=authenticate、id1 空闲）钉死这一点；`InterfaceType::{Internal,External}` 已在 network_interface 就位。
  - 证据：**长度表与常量黄金比对**——常量值全部钉死 `FIXED=0 / VARIABLE=1 / CALLBACK=2 / INVALID=3`、`DEFAULT_VARIABLE_LENGTH_HEADER_SIZE=1`、`IDENTIFIER_SIZE=1`、`REPLY_MESSAGE_IDENTIFIER=0xFF`、`INVALID_STREAM_SIZE=i32::MIN`。两个真实参考接口逐字段对齐：`lib/connection/login_interface.hpp` 的 `login`(VARIABLE,2) / `probe`(FIXED,0) / `challengeResponse`(VARIABLE,2) 取 ID 0,1,2；`server/reviver/reviver_interface.hpp` 的 `BW_REVIVER_MSGS(COMPONENT)` 五次展开得 10 条 `handle{CellAppMgr,BaseAppMgr,DBAppMgr,DBApp,Login}{Birth,Death}`，全 FIXED、长度 `sizeof(Mercury::Address)`=8（以 `size_of::<Address>()==8` 反证 uint32 ip + uint16 port + uint16 salt）。
  - 证据：**长度语义逐条对照**——`headerSize`（Variable 为 1+lengthParam、Fixed 为 1、其余 -1）、`nominalBodySize`（仅 Fixed 给 param）、`streamSize`（Variable 取负）、`canHandleLength`（4 字节头直通，否则 `length < (1<<(8*size))-1`）、`updateLengthDetails`（非回调直通 true；回调由 handler 裁定：`INVALID_STREAM_SIZE`→false、负→Variable+|answer|、非负→Fixed+answer）、`c_str` 为 `"{name}/{id}"`、`REPLY = ("Reply", 0xFF, VARIABLE, 4)`。
  - 证据：**addRange 公式**——`endID = startID + (254-startID)/rangePortion` 且 `MF_ASSERT(ie.id() == elements_.back().id())`：`a_range_covers_the_first_block_of_the_free_ids` 以 login（3 条已声明）得 `(254-3)/2=125` → end=128、len=129，块内每个 ID 均为基元素副本（name/style/param/handler 俱留）。
  - 证据：**统计表与 1s 计时器**——`InterfaceElementWithStats` 持 `maxBytesReceived_/numBytesReceived_/numMessagesReceived_` 与两个 `AccumulatingEMA<uint>`；`AVERAGE_BIAS = EMA::calculateBiasFromNumSamples(5)` = `exp(ln(1-0.95)/5)`≈0.54928027（`isZero` 是精确 `fabs<=0`，0 样本→bias 0）；`InterfaceTable` 构造 256 槽并 `addTimer(1000000, this, NULL, "InterfaceStats")`。pin-clock 测试 `the_timer_is_registered_with_the_dispatcher` 以 500_000µs 不触发、1_000_000µs 触发，钉死**每秒一次而非每轮一次**。EMA 数学移植为 `foundation::math::ema`（`Ema` / `AccumulatingEma` / `Accumulator`）；本工具链删掉了 `f32: From<u32/i32>`，故补 `Accumulator` trait 承载参考白得的 `float(accum_)` 转换。
  - 证据：**有意偏离（四条）**——(a) machined 注册（`registerWithMachined`）不属本项，name/id 留为普通字段，交由 BW-02-02-05 填充；(b) `get_message_stream_size` 参考为 `MF_ASSERT(0)` 后返回 -1，本移植**去掉 abort 保留 -1**，误声明的回调消息表现为无效 bundle 而非杀进程；其 `(networkInterface, srcAddr)` 两参省略（参考无任何 handler 用它们定尺寸，地址随 BW-02-02-03 投递面到来）；(c) `InterfaceMinder::handler/set_handler` 越界返回 None / 无操作（参考是未做边界检查的 `elements_[index]`，属 UB）；(d) `c_str` 返回 owned String（参考是 static 缓冲，两调共享同一内存）。
  - 测试与覆盖：全 workspace **898 通过 / 0 失败 / 33 ignored**（mercury 135 + foundation 134 为本轮交付面，新增 20 个接口层测试 + 7 个 EMA 测试）。`cargo clippy -p foundation -p mercury --all-targets` **0 警告**（仅余既有 workspace 级 `python-host` 未用依赖警告，非本轮引入）；`rustfmt --check` 五个新文件 0 差异。llvm-cov（四 crate）：ema.rs **100.00%**、interface_element.rs **100.00%**、interface_minder.rs **100.00%**、interface_table.rs **100.00%**、interfaces.rs **100.00%**（行/区域/函数三项皆满），TOTAL **99.54%** 行 / **99.61%** 函数，全 ≥98% 门。并行稳定：`timeout` 兜底下 mercury **0/100**、foundation **0/40**。
- [x] **BW-02-02-02 实现固定及变长消息帧**。**验收** 黄金包头及负载逐字节一致。
  - 证据：**帧布局（验收核心）**——参考的 `compressLength` / `expandLength` / `unexpandLength`（interface_element.cpp:283-596）配合 `UDPBundle::newMessage`（udp_bundle.cpp:754-795）钉死线格式：`[id:1][length:lengthParam][请求另有 replyId:4 + nextRequestOffset:2][body]`。`newMessage` 里 `qreserve(headerLen + extra)`、`*(MessageID*)pHeader = curIE_.id()`、`msgBeg_` 指向 id 字节，故长度字段从偏移 1 起（`compressLength` 里 `char *pLen = ((char*)data) + 1`）。`ReplyID` 在 misc.hpp:110 是 **int32 而非 1 字节**、`Packet::Offset` 是 uint16，故请求头多 6 字节；`body_offset()` 用 `headerSize()`（fixed 为 1、variable 为 1+lengthParam）复现。
  - 证据：**四宽度长度字段逐字节一致**——1 字节 `*pLen = uint8(len)`、2 字节 `BW_HTONS`、3 字节 `BW_PACK3`（低 3 字节小端）、4 字节 `BW_HTONL`。黄金向量 `the_length_lands_just_past_the_id_byte` 钉死 65000→`[232,253]`、1_600_000→`[0,106,24]`、2_000_000_000→`[0,148,53,119]`，并断言 id 字节不被改写；`the_compress_and_expand_are_inverses` 对四宽度做压缩→展开往返。
  - 证据：**门槛比字段容量少 1**——压缩阈值 `len < 0xff / 0xffff / 0xffffff`，`canHandleLength` 为 `length < (1<<(8*size)) - 1`，故 255 / 65535 / 16777215 恰好落入特殊形式；`the_thresholds_match_can_handle_length` 双向钉死（`fits` 走普通形式、`fits+1` 走特殊形式）。4 字节头参考**先返回 true 再算移位**（否则 C++ 里 `1<<32` 是 UB），本移植同序。
  - 证据：**特殊形式（超宽消息）**——长度字段置全 1、真实长度以小端 u32 写在 body 头部、被挤出的前 4 字节追加到尾部（`specialCompressLength` 逐字节 `*tail = *head; *head = (uint8)localLength; localLength >>= 8;`，因 head 可能跨包）。`an_oversize_body_takes_the_special_form` 以 255 字节（1 字节字段的最小超宽值）钉死 head=`[255,0,0,0]`、tail=`[1,2,3,4]`；expand 后 head 变回 `[1,2,3,4]` 而 tail 保留垃圾 4 字节——参考从不回填，留给调用方 shrink。压缩侧参考先 `pBundle->reserve(4)` 再写 body 之后 4 字节，本移植改由调用方提供余量、越界返回 `BufferTooSmall`。
  - 证据：**反向与溢出臂**——`expandLength` 宽 0→0、1/2/3/4 分别 `BW_UNPACK*`，4 字节顶位为 1 时 `(int)len < 0` 报溢出（`a_four_byte_length_can_overflow` 以 `u32::MAX` 钉死）；只有 `!canHandleLength(len)` 才走 `specialExpandLength`。unknown 宽度走参考 default 臂（记 CRITICAL 后 `len` 保持 0 → 返回 0），fixed 直接返回 `lengthParam`；fixed 长度不符时参考记 CRITICAL 仍返回 0，本移植以 `debug_assert_eq` 复现该断言并用 `#[should_panic]` 钉住。
  - 证据：**单包连续缓冲的等价性**——参考借 `BundleDataPos` 跨 `Packet::next()` 链推进，本移植在一块连续缓冲内框；特殊形式只在 body 远大于 4 字节时发生，head 与 tail 永不重叠，整字复制与逐字节复制同解。多包消息留待 BW-02-03 的 channel 层。
  - 测试与覆盖：全 workspace **814 通过 / 0 失败 / 33 ignored**（mercury 151 = 135 + 新增 16 个 message_framing 测试）；`cargo clippy -p foundation -p mercury --all-targets` 仅余既有 workspace 级 `python-host` 未用依赖警告；`rustfmt --check` message_framing.rs 0 差异。llvm-cov（四 crate）：message_framing.rs **100.00%** 行 / 区域 / 函数（331 行全覆盖），ema.rs、interface_element.rs、interface_minder.rs、interface_table.rs、interfaces.rs 仍 **100.00%**，TOTAL **99.55%** 行 / **99.62%** 函数，交付面全 ≥98% 门。并行稳定：`timeout` 兜底下 mercury **0/100**。
- [x] **BW-02-02-03 实现请求关联与应答派发**。**验收** 交错请求各自收到正确响应。
  - 证据：**验收核心（交错请求）**——`interleaved_replies_each_reach_their_own_request` 以 `with_next_reply_id` 种子 10101 发起三个请求（arg=10/20/30，载荷各不同），应答按**三、一、二**的乱序到达，三个 `Recorder` 各自只收到自己的 `(arg, 载荷)`，id 10101/10102/10103 与请求一一对应，无串扰。
  - 证据：**ID 分配与回绕**（request_manager.cpp:60-77 `getNextReplyID`）——`nextReplyID_ > REPLY_ID_MAX(1_000_000)` 时回绕到 1 再自增返回（`the_reply_ids_climb_and_wrap_at_the_ceiling` 从 999_998 爬到回绕）；种子 `(uint32(timestamp()) % 100000) + 10101`（request_manager.cpp 构造）→ `seed_next_reply_id` 钉死 0→10101、1→10102、99_999→110_100、100_000→10101；默认构造用真实时钟落进同一窗口（`a_default_manager_starts_its_ids_in_the_reference_window`）。线格式为**小端 4 字节**（`BW_HTONL` 在 x86-64 恒等，与帧层、`BinaryStream` 一致），`the_id_lands_in_the_request_frame_little_endian` 钉死 0x01020304 → `[4,3,2,1]`。
  - 证据：**派发顺序**（udp_bundle.cpp:281-340 `processPacket` REPLY 臂）——identifier 必须 `REPLY_MESSAGE_IDENTIFIER(0xFF)`，否则原样丢弃（`a_non_reply_message_is_dropped`）；`header.length < sizeof(int)` 记 CRITICAL 不消费不 erase（`a_short_reply_is_corrupted_and_the_request_stays_pending`，请求仍挂起）；读 `inReplyTo` 4 字节（截断读得 0，无请求持有 0）；`length -= 4` 后交 handler；未知 id 记 WARNING 后 `data.finish()` 消费整条再返回（`an_unexpected_reply_is_dropped_and_consumed`）；**外部门检查在 erase 之前**——`isExternal_ && !isValidSource(source)` 失败时记 WARNING 且**不 erase**（请求保持挂起，等真应答再来），`the_source_is_checked_only_on_external_nubs` 对 external true/false 两态循环钉死；内部 nub 不查来源（`a_channelless_request_answers_from_any_address`）。`isValidSource` = `(pChannel_ == NULL) || (source == pChannel_->addr())`。
  - 证据：**pReplyID 指针→偏移**——参考 `ReplyOrder.pReplyID` 是指进 bundle 数据的裸指针，`addReplyOrder` 经它把 `BW_HTONL(replyID)` 写回消息头；本移植为 `reply_id_offset: usize` + 调用方提供的帧缓冲，`add_reply_order` 越界（offset+4 > len）返回 `BufferTooSmall` **不泄漏请求**（`a_short_frame_is_refused_without_leaking_a_request`），成功则分配 id、入 `BTreeMap`（参考 `BW::map<int, Request*>`）、按小端写入 offset 处。
  - 证据：**注册走表**（network_interface.cpp:147）——`interfaceTable().serve( InterfaceElement::REPLY, pRequestManager_ )`，`the_manager_serves_the_reply_element_through_the_table` 钉死表内 `msg_name(0xFF) == "Reply"` 且应答经表的 `Rc<dyn InputMessageHandler>` 到达 manager。
  - 证据：**有意偏离（四条）**——(a) `Request` 不带定时器：参考构造函数里 `!pChannel` 时注册一次性 timeout timer，超时/取消是 BW-02-02-04，本项的 Request 只承载关联（handler/arg/channel 地址）；(b) `Channel *` → `Option<Address>`，channel 对象 BW-02-03 到来，`isValidSource` 只需地址；(c) `void *arg` → `usize`（沿 timer 层同一减形）；(d) `NubException` 独立模块（沿 packet_receiver 的 Reason+address 先例），`UnpackedMessageHeader` 只带 identifier/reply_id/length/element，pChannel/pInterface/pBreakLoop 随 BW-02-03。`InputMessageHandler::handle_message` 变为**必需**方法（参考纯虚），`&mut header` 对应参考注明的非 const 参数；`ReplyMessageHandler::handle_shutting_down` 默认实现打印参考的 "Not handled. Possible memory leak." 警告。
  - 测试与覆盖：全 workspace **837 通过 / 0 失败 / 33 ignored**（mercury 174 = 151 + 新增 23：request_manager 12、unpacked_message_header 3、request 3、nub_exception 2、interfaces 2、reply_order 1）。`cargo clippy -p foundation -p mercury -p python-host --all-targets` **0 错误**（仅余既有 workspace 级 `python-host` 未用依赖警告）；rustfmt 触及文件 0 差异。llvm-cov（四 crate）：request_manager.rs、request.rs、reply_order.rs、unpacked_message_header.rs、nub_exception.rs、interfaces.rs、misc.rs 均 **100.00%** 行 / 区域 / 函数，TOTAL **99.52%** 行 / **99.46%** 函数，交付面全 ≥98% 门。并行稳定：`timeout` 兜底下 mercury **0/100**；顺带根治 python-host output_writer 六个共享全局 writer 状态的测试互踩（进程级 `OnceLock<Mutex<()>>` 门），`--test-threads=8` 下 **0/30**。
- [x] **BW-02-02-04 实现请求超时和取消**。**验收** 回调恰好一次并释放挂起状态。
  - 证据：**验收核心（回调恰好一次）**——`a_channelless_request_times_out_and_reports_exactly_once`：无 channel 请求（超时 5s）在时钟跨过线后 `process_timers` 恰好触发 1 次，handler 收到**恰一次** `(REASON_TIMER_EXPIRED, arg)`，挂起表归零；随后同一 id 的迟到应答走参考的 "Couldn't find handler for reply id ... (maybe it timed out?)" 路径，既不投递也不再失败（`failures` 仍是恰好一条）。反向钉死：`a_replied_request_cancels_its_timeout` 应答先到即取消定时器，时钟再跑 10 倍超时也只有应答无失败——每请求回调数恒为 1。
  - 证据：**定时器装配**（request.cpp:17-32 `Request` 构造）——`!pChannel` 时 `MF_ASSERT( replyOrder.microseconds > 0 )` + `dispatcher.addOnceOffTimer( microseconds, this, pRequestManager )`；本移植断言文案 "a request with no channel needs a positive timeout"（`#[should_panic]` 钉住），带 channel 的请求装配**零定时器**（`a_channel_request_carries_no_timeout_of_its_own`：时钟跑 10 倍 `process_timers` 计数为 0，随后应答照常工作），零超时+有 channel 合法（参考只在装定时器时断言）。超时到期路径等价参考 `handleTimeout → failRequest(*this, REASON_TIMER_EXPIRED)`：闭包持有 `Weak<Inner>`、以 reply id 为 user 参数，命中即 `inner.fail_request(id, TimerExpired)`。
  - 证据：**取消两路**（request_manager.cpp:60-103）——`cancelRequestsFor( pChannel )` → `matches(pChannel)` 的请求以 `REASON_CHANNEL_LOST` 失败（`losing_a_channel_fails_only_its_own_requests`：同 channel 两请求失败、他 channel 与无 channel 的不动，挂起 4→2）；`cancelRequestsFor( pHandler, reason )` → `matches(pHandler)` 以**调用方给的原因**失败（`cancelling_a_handler_fails_only_its_requests`：两失败一存活，存活者随后照常应答）。指针同一性映射：handler 用 `Rc::ptr_eq`（`the_handler_match_is_by_identity`：同内容不同 Rc 不匹配）；channel 用**地址等同**（channel 对象 BW-02-03，Mercury `findChannel(addr)` 本就以远端地址为键）。
  - 证据：**析构清账**（request_manager.cpp:35-54 `~RequestManager`）——非空时打印 "RequestManager::~RequestManager: Num pending reply handlers = N"，逐请求 `handleFailure( REASON_SHUTTING_DOWN )`；`Request::handleFailure`（request.cpp:82-97）按原因路由：`!= SHUTTING_DOWN` 走 `handleException`、`== SHUTTING_DOWN` 走 `handleShuttingDown`（`a_normal_failure_reaches_the_exception_hook` / `a_shutting_down_failure_reaches_the_shutdown_hook` 各钉一钩）；`dropping_the_manager_fails_every_request_with_shutting_down` 钉死两 handler 三请求各收恰一次 `(ShuttingDown, arg)`、`handleException` 零条、被取消的定时器时钟再跑 10 倍 `process_timers` 计 0。失败的 `NubException` 不带地址（参考 `NubException e( reason )` 默认）。
  - 证据：**失效顺序与重入**（request_manager.cpp:109-113 + 两处 "iter needs to be moved early" 注）——`failRequest` 先 `erase` 再 `handleFailure`；本移植 `Inner::fail_request` 用显式作用域结束 `RefCell` 借用后再调回调（`if let` 临时存活会撞 "RefCell already borrowed"，参考的 `map::erase` 天然无此问题）；`a_handler_may_start_its_own_request_from_any_hook` 钉死投递钩与失败钩内**再发请求**均安然存活（cancel 收集 id 先于失效，重入的 312 存活）；`fail_request_reports_once_and_tolerates_a_missing_id` 钉死公共 `failRequest` 对已消失 id 幂等。
  - 证据：**孤儿定时器**——`a_timer_outliving_its_manager_finds_nothing_to_fail`：manager 析构取消其全部定时器，时钟越过线 `process_timers` 计 0、handler 零失败；`a_request_added_while_dropping_hears_nothing`：析构回调内再发的请求随共享状态一起消失，其孤儿定时器随后照常触发但弱句柄 upgrade 失败落空（参考的 `requestMap_.clear()` 同样静默删除，可观察结果一致；弱句柄语义参考用裸指针做不到的部分是安全）。
  - 证据：**期限精度**——`a_short_timeout_fires_at_its_own_deadline`：100µs 请求在 +99µs 不触发、+100µs 恰好触发，`order.microseconds` 即等待时长；id 计数器搬进 `Inner`、`addReplyOrder` 主体提为 `add_reply_order_to(inner, dispatcher, ...)` 自由函数，manager 方法变薄壳——析构期回调仍可经持有的句柄走到同一装配路径。
  - 测试与覆盖：全 workspace **855 通过 / 0 失败 / 33 ignored**（mercury 192 = 174 + 新增 18：request_manager 13、request 5）。`cargo clippy -p foundation -p mercury -p python-host --all-targets` **0 错误**（仅余既有 workspace 级 `python-host` 未用依赖警告）；rustfmt 触及文件 0 差异。llvm-cov（四 crate）：request_manager.rs **100.00%** 行 / 区域 / 函数（1088 行全覆盖）、request.rs **100.00%**（216 行），interfaces.rs、misc.rs、nub_exception.rs、reply_order.rs、unpacked_message_header.rs 仍 **100.00%**，TOTAL **99.53%** 行 / **99.47%** 函数，交付面全 ≥98% 门。并行稳定：`timeout` 兜底下 mercury **0/100**、python-host **0/30**。
- [x] **BW-02-02-05 注册处理器与校验**。**验收** 未知消息与长度错误结果吻合。
  - 证据：**验收核心（未知消息）**（udp_bundle_processor.cpp:71-84）——`an_unhandled_message_id_discards_the_rest_of_the_bundle`：bundle 内未知 id 打头，`ie.pHandler()==NULL` 记 ERROR "Discarding bundle after hitting unhandled message ID %u" 后返回 `REASON_NONEXISTENT_ENTRY` 并 break，其后的合法消息**零投递**；`a_registered_element_without_a_handler_is_unhandled` 钉死判据唯一性——serve 过元素但 handler 为 None 与未注册 id 同判（参考 `pHandler()==NULL` 不看注册表是否含该元素）。
  - 证据：**长度错误六路逐一吻合**（udp_bundle_processor.cpp:413-495 iterator::unpack + dispatchMessages:100-135）——全部 `REASON_CORRUPTED_PACKET` 且随后的消息不投递：(1) 头不完整 "Not enough data on stream at %hu for header (N bytes, needed M)"（`a_truncated_header_is_corrupted`）；(2) `expandLength` 失败——4 字节长度域最高位置 1 的溢出经 `FrameError::Overflow` 映射参考的 -1，记 "Error unpacking header length at %hu"（`an_overflowing_length_field_is_corrupted`）；未知 style 经 `FrameError::UnknownLengthStyle` 走同一错误路（参考 default 臂 "unrecognised length format" → return -1，interface_element.cpp:584-594）；(3) 回调消息 handler 返回 `INVALID_STREAM_SIZE(i32::MIN)` → `updateLengthDetails` false → "failure to update length details for message ID %hu"（`a_callback_message_that_refuses_its_size_is_corrupted`）；(4) 请求前导不足 6 字节 "Not enough data on stream for request ID and NRO"（`a_request_without_room_for_its_preamble_is_corrupted`）；(5) nextRequestOffset 越界 "> bodyEndOffset" → "Next request offset out of bounds (got offset %hu, msg end offset %hu)"（`a_request_link_out_of_bounds_is_corrupted`，出错消息自身也不投递）；(6) 载荷不足 "Not enough data on stream for payload"（`a_short_payload_is_corrupted`）。每路错误后打印参考的 "Got corrupted message header" 尾行（errorNoRevert 汇合点）；(4)(5)(6) 三路先 `unexpand_length` 回滚帧再报错，对应参考 `goto error` 的 `updatedIE_.unexpandLength(...)`。
  - 证据：**回调定尺寸接线**（interface_element.cpp:64-94 `updateLengthDetails`）——`a_callback_message_is_sized_by_its_handler`：负答案 → 变长消息（宽 = |n|，3 字节长度场小端）、正答案 → 定长消息（值 = 体长），均在 `updatedIE_` 副本上解析后正常 unpack 与投递；解析只动副本，表内元素保持 `CALLBACK_LENGTH_MESSAGE` style，`header.pInterfaceElement = &ie` 装 served 元素（`msg_name()` 可用），下一 bundle 重新定尺寸。
  - 证据：**派发主线**（udp_bundle_processor.cpp:47-223）——`a_good_bundle_delivers_every_message_in_order`：定长+变长逐条按线序投递，header 的 identifier/reply_id（普通消息 `REPLY_ID_NONE`）/length/element 逐字段吻合；`a_request_in_a_bundle_carries_its_reply_id`：请求消息以前导 reply id 投递（isRequest 判据 = `nextRequestOffset_ == offset_`，参考 iterator 同式）；`two_requests_chain_and_a_plain_message_sits_between`：NRO 链把 walk 从 R1 引到 R2，中间普通消息不受染、各 reply id 101/202/REPLY_ID_NONE 各就各位；`an_empty_bundle_is_a_success` 空体即参考的 iter==end 直落 Success。handler 吃剩的字节：普通消息 WARNING "Handler for message %s (ID %hu) left %d bytes" + `mis.finish()`、REPLY 只警告不 finish（`a_handler_that_leaves_bytes_does_not_stop_the_walk` 钉死普通消息 finish 后 walk 继续；reply 臂经闭环测试到达）。参考 `iter.data()==NULL` 的 "chain too short"（udp_bundle_processor.cpp:346-353）在单缓冲铺平下不可达——载荷界检先行于取数，同一 `REASON_CORRUPTED_PACKET` 面。
  - 证据：**闭合回路**——`a_reply_in_a_bundle_completes_its_pending_request`：线上 REPLY 消息（0xFF，请求形帧）经表投给 `RequestManager`（其 `impl InputMessageHandler` 即参考 `serve( InterfaceElement::REPLY, pRequestManager_ )` 的 handler 侧，network_interface.cpp:147），deliver_reply 读 inReplyTo 完成挂起的 4242 号请求（Recorder 收 `(arg=99, payload="done")`），pending 归零——**walk→表→manager 三层闭环**，BW-02-02-03 的回复派发面自此有 wire 入口。
  - 证据：**特殊形式贯穿**（interface_element.cpp `specialExpandLength`）——`an_oversize_body_walks_its_special_form`：宽 1 变长 255 字节体压缩为特殊形式（长度场置 0xff + 头部 4 字节真长 + 体尾挪出 4 字节），dispatch 侧 `expand_length` 还原后 handler 看到的流是**声明长度**（参考 `MemoryIStream mis( msgData, header.length )`），`dataLength_ += sizeof(int32)` 的 +4 只用于推进（`!canHandleLength( dataLength_ )` 同判据）——初版把 259 字节全给 handler 的偏差在交付前修正；`delivered_messages_are_counted_against_their_elements`：每投递一条记 `messageReceived( header.length )`（参考 `stopProfile` 之 `messageReceived`，interface_element.hpp:265-268），落在 `InterfaceTable::record_received`。
  - 证据：**有意偏离（三条）**——(a) 单包铺平：参考走 packet 链（iterator::nextPacket/跨包 dataBuffer 拷贝/分片），本移植单连续缓冲，offset 一律 body-relative、参考的 packet-absolute 差 `Packet::HEADER_SIZE` 常数两侧抵消；`firstRequestOffset` 改调用方传入（页脚解析随 BW-02-03-01 的 Packet/Bundle 编解码），`None` = 页脚无请求（参考的 0）；线上 NRO 定义为 body-relative，BW-02-03-01 写侧按同一契约。(b) BW-02-03 defer 面：early 派发（isEarly/shouldProcessEarly）、per-channel `MessageFilter`、`header.pBreakLoop`（channel 死亡打断 walk）、FragmentedBundle、`ProcessSocketStatsHelper`、`onBundleStarted/onBundleFinished`（handler 形参为 `Channel*`）。(c) 统计无 `ENABLE_WATCHERS` 门（参考 `#if ENABLE_WATCHERS` 下记数，server 构建恒开，本移植无条件记）；BW-02-02-01 偏离 (a) 的「machined 注册交由 BW-02-02-05」**修正为交由 BW-05**（todo.md「machined/ # BW-05」）：`registerWithMachined` 的对象是 bwmachined 进程 + MachineGuard 协议，本层无对象可注册，name/id 维持普通字段。
  - 测试与覆盖：全 workspace **872 通过 / 0 失败 / 33 ignored**（mercury 209 = 192 + 新增 17：bundle_dispatch 16、interface_table 内嵌 1 处扩展）。`cargo clippy -p foundation -p mercury -p platform -p server-runtime -p python-host --all-targets` **0 错误**（resources 的 2 处 clippy 错误为 HEAD 既有，不在交付面）；rustfmt 触及文件 0 差异。llvm-cov（四 crate）：bundle_dispatch.rs **100.00%** 行 / 区域 / 函数（771 行全覆盖）、interface_table.rs **100.00%**（205 行），TOTAL **99.56%** 行 / **99.50%** 函数，交付面全 ≥98% 门。并行稳定：`timeout` 兜底下 mercury `--test-threads=1` **0/100**、python-host `--test-threads=8` **0/30**。

#### BW-02-03 可靠 UDP

实现 Packet、Bundle、UDPChannel、ACK/序号窗口、重传、乱序处理、分片重组、Channel 生命周期及统计。双向混跑并覆盖丢包、延迟、重复包和序号回绕。

- [x] **BW-02-03-01 实现 Packet/Bundle 编解码**。**验收** 真实 UDP 帧逐字节往返。
  - 证据：`crates/mercury/src/packet.rs`（全量 Packet 编解码）+ `crates/mercury/src/udp_bundle.rs`（UdpBundle 写侧）+ `misc.rs` 增 SeqNum 环（SEQ_SIZE=0x10000000/SEQ_MASK/SEQ_NULL/seqMask/seqLessThan/SeqNumAllocator）。验收测试 `a_bundle_survives_a_real_udp_round_trip`：bundle 构造（固定消息+请求+回执）→ finalise → prepare_packets（校验和开）→ 真实 loopback UDP `sendto_address`/`recv_from_endpoint` → validate_checksum → strip 请求页脚 → `dispatch_messages` → 三个 handler 全中、RequestManager 收尾（收端 pending 清零、发端仍挂 1）；另有 golden 逐字节夹具：固定 `[0,0,3,A,B,C,D]`、可变长度压缩、REPLY 帧 15 字节 `[0,0,0xFF,8,0,0,0,77,0,0,0,d,o,n,e]`（长度字段=8 覆盖 id+正文）、请求保留 6 字节前导 + `first_request_offset == HEADER_SIZE`、双请求链 patch、分片（2000 字节 → 2 包，页脚线序 `[seq0,seq1,seq0]`）、超长特殊形（0xff 标记 + 位移 4 字节 qreserve 跨包）、ack 页脚线序 `[seq LE][count]`（count 先 pack 落最高地址）、校验和 XOR 含越界末字（与参考同款 overrun）。
  - 合同要点（对齐参考源码逐行核过）：页脚 pack 次序决定线位（后 pack = 低地址；prepare 顺序 checksum→acks→seq→firstRequestOffset→fragInfo，线上升序 [fragBegin,fragEnd,requests,seq,acks,checksum]）；`addRequest` 首请求记 `first_request_offset`、次请求 patch 前一链槽、末槽置零；piggyback 从顶剥离、长度高位=最后一块、`!raw` 还原长度；`stripFooter` 走到 flags 字即拒；`shouldCreateAnonymous` = ON_CHANNEL|CREATE_CHANNEL 且非 INDEXED；`ProcessPiggybackPackets` 剥出的内包置 `is_piggyback`。
  - **派发缝合修正（取代 BW-02-02-05 旧断言）**：线上请求链接是 packet-absolute（参考 iterator offset_ 从 `Packet::HEADER_SIZE` 起数），旧「HEADER_SIZE 两侧抵消」只对单连续缓冲成立、对真实线帧不成立；现契约=walk 用 body-relative、仅在两处比较点翻译（`is_request = next_request_offset == offset + HEADER_SIZE`、越界按 `link > msg_end + HEADER_SIZE` 记线值），`next_request_offset`/`first_request_offset` 存原始线值，`None` = 页脚无请求。`bundle_dispatch` 4 处测试夹具与 `chain()` 助手同步改。
  - 有意偏离：链用 `Vec<Packet>` 顺序持有（参考侵入式 PacketPtr 链）；`msg_beg` 用 `(packet,offset)` Pos 对（qreserve 连续保证头部不跨包）；`addReliableOrder`/重发计时/首溢返回 → BW-02-03-02+，piggyback 发送路径 → BW-02-03-06，channel 版本页脚 → channel 层；`add_reply_orders_to` 的「帧过小」分支经 1500 字节满缓冲不可达，参考的 log-and-continue 收敛为 expect（注释注明）。
  - 验证：mercury lib 237/0；workspace 900 passed/0 failed；clippy 五 crate 对干净 HEAD 零新增（mercury 2 处新告警已修：useless vec + 不可达分支改写）；llvm-cov 行覆盖 packet.rs/udp_bundle.rs 均 100%（llvm-cov show 逐行、lcov DA、Uncovered Lines 页脚三证一致；summary 计数器多报 packet.rs 3 行为 llvm-cov 已知伪影，无对应行）；压力：mercury `--test-threads=1` ×100 循环 0 失败，python-host ×30（`--test-threads=8`）0 失败。
- [x] **BW-02-03-02 实现 Channel 注册与关闭**。**验收** 重复创建和旧 Channel 回收吻合。
  - 新增：`udp_channel.rs`（1228 行，UDPChannel 构造/注册/condemn/destroy + accessor 面 + `InactivityTimeoutChecker` + `ChannelRefHandle`）、`channel_map.rs`（241 行，地址键注册表）、`condemned_channels.rs`（457 行，处决候补集 + 60s 单次清扫）、`channel_listener.rs`（15 行，两默认体）；改 `network_interface.rs`（持有 poller/dispatcher/channels/condemned/request_manager/self_weak，`findChannel`/`delAnonymousChannel`/`onAddressDead`/`destroyOwnedChannels`）、`packet_receiver.rs`（错误路径 3 处 `find_channel`→`find_channel_ref`）、`udp_bundle.rs`（增 `clear()` 复位到构造态并保留 spare，供 `clearBundle` 用）、`lib.rs`（4 个新模块）。
  - 验收测试 `duplicates_refuse_and_reclaimed_slots_serve_fresh_channels`：同址二次注册被拒（`Should not register a channel twice`，原租户保留、`find` 仍答原租户）；旧 Channel `destroy` 释放槽位后新 Channel 入驻同一地址（经 interface 活表断言——destroy 的注销走 interface 的 map，不走路旁 map）。
  - 合同要点（参考逐行核过）：窗口尺寸按 traits+id（`udp_channel.cpp:92-94` External 256 / Internal 无 id 4096 / 有 id 512）；RTT `stampsPerSecond()/10` 内部、`stampsPerSecond()` 外部（`udp_channel.cpp:100`）；`minInactivityResendDelay_ = delay*stampsPerSecond()`；`unreliableInSeqAt_ = SEQ_NULL`；`highestAck_ = seqMask(smallOutSeqAt_-1)`；`setAddress` 变更时 `lastReceivedTime_` 取 interface 时钟并注册（`MF_VERIFY(registerChannel)`）；`ChannelMap::add` 断言 `addr != Address::NONE`、拒重、`remove` 拒外人/未知且租户不丢（channel_map.cpp:19/41）；condemn 二次告警返回（udp_channel.cpp:380）；condemn → `condemnedChannels().add`「可能立刻删掉本 channel」；`!hasUnackedPackets() || hasRemoteFailed()` 当场 destroy；AGE_LIMIT=60（condemned_channels.hpp:38）；indexed 按 id 挂、后继者 destroy 前任并告警 `Already have a channel with id %d`；60s 单次定时器 → `deleteFinishedChannels`；强制删条件 `now-lastReceived>60s && now-lastReliableSend>60s`（告警保留参考的 `CondemnedChannels::handleTimeout:` 标签）；destroy 幂等（IF_NOT_MF_ASSERT_DEV）、取消不活跃计时、`onChannelGone`→`cancelRequestsFor`、fire listener、注销；`setRemoteFailed`：internal→`cancelRequestsFor`、anonymous→`delAnonymousChannel`（无 unacked 故当场 destroy）；`findChannel` 断言 `!createAnonymous || !isExternal()`、`ip==0`→断言并 NULL、命中者断言 `id()==CHANNEL_ID_NULL`、anonymous=新建 INTERNAL+`isAnonymous(true)`；`delAnonymousChannel` 未找到/非匿名均 ERROR；`destroyOwnedChannels` 只收 `isOwnedByInterface()`（!destroyed && (anonymous||condemned)），存留者告警 `Channel to %s is still registered`。
  - 有意偏离：(a) 注销时机——参考在析构里 `clearState`→`setAddress(NONE)` 注销，本移植在 `destroy()` 内注销（Rust 无「末次解引用时刻」可挂，且 condemned 集不注销以免清扫重入活表）。(b) 时间戳单位微秒（参考 `Time` 秒）。(c) condemn 的「刷未发数据」（`hasUnsentData`→`send`）推后到 BW-02-03-06，未发 bundle 随 channel 走（参考 udp_channel.cpp:389-402）。(d) keepalive/regularity（`isLocalRegular(false)`/`isRemoteRegular(false)`）、per-channel 过滤器（`pFilter && id_` 临界）、`recentlyDeadChannels` → BW-02-03-03+ / BW-02-05。(e) `ChannelRefHandle` 薄封装：Rust 的 `Rc<T>→Rc<dyn Trait>` 需 `T: Trait`，参考靠 `UDPChannel*` 天然是 `ChannelRef*` 切片无需桥。
  - 测试与覆盖：mercury **262 通过 / 0 失败**（较上轮 +8：accessors 构造簿记、condemn 二次、无 interface 的 channel、checker 失去 channel、静默 listener、indexed 清扫排空、错误路径穿透活表、`destroyOwnedChannels` 取舍）。workspace **925 passed / 0 failed / 33 ignored**。llvm-cov：channel_listener.rs **100.00%**、channel_map.rs **100.00%**、udp_bundle.rs **100.00%**、condemned_channels.rs **99.64%**（残 1 行=定时器闭包 `if let` 收括号伪影）、network_interface.rs **98.93%**（残 6 行=socket 创建/默认路由失败既有分支）、packet_receiver.rs **98.97%**、udp_channel.rs **98.29%**（残 12 行=3 处收括号伪影 + 测试夹具 `Empty::handle_message` 8 行——回执只在真实请求往返才送达，构造路径不可达，且该测试断言其永不被调用）；新 7 文件合计 **99.18%**，mercury 全 crate **99.50%** 行。clippy 零新增（仅既有 workspace 级 `python-host` 未用依赖告警）；rustfmt 本轮触及文件 0 差异。
  - 压力：mercury `--test-threads=1` **100 轮 26200 通过 / 0 失败**；python-host `--test-threads=8` **30 轮 1140 通过 / 0 失败**；全量输出 `grep -E "FAILED|panicked"` 0 命中。
- [x] **BW-02-03-03 实现 ACK 与序号窗口**。**验收** 序号回绕夹具通过。
  - 新增：`circular_array.rs`（175 行，2 的幂环形数组：掩码寻址 `get`/`get_mut`、`doubleSize` 按绝对偏移搬运、`inflateToAtLeast` 增容丢内容、`swap`、非 2 幂尺寸拒绝）、`unacked_packet.rs`（206 行，`UnackedPacket` 发送簿记 + 可靠回执单）；改 `udp_channel.rs`（1228→2101 行：`unackedPackets_` 环、`unackedCriticalSeq_`、`hasSeenOverflowWarning_`、`lastReliableResendTime_` 四字段 + 序号/ACK 方法面）、`lib.rs`（2 个新模块）。
  - 验收测试 `the_window_and_acks_wrap_through_the_seq_ring`（唯一依赖回绕的夹具）：`forceOutSeqAt(SEQ_MASK-1)` 后三次发送落在 `SEQ_MASK-1`/`SEQ_MASK`/`0`（`smallOutSeqAt_==1`、`oldestUnackedSeq_==SEQ_MASK-1`、`sendWindowUsage()==3`），`isInSentWindow` 在缝两侧分别成立、缝前一格与越头一格均拒绝；乱序 `handleAck(0)` 只推进 `highestAck_`（跨缝回退到 0）而 `oldestUnackedSeq_` 不动、临界槽清零；旧 ACK 幂等；`handleAck(SEQ_NULL)` 以 `seqMask(SEQ_NULL)!=SEQ_NULL` 拒绝；`handleCumulativeAck(1)` 逐格排空至 `oldestUnackedSeq_==SEQ_NULL`、`usage==0`。全程只有掩码算术，无任何回绕特判（`misc.rs` 的 `seqMask`/`seqLessThan` 单测独立守住环）。
  - 合同要点（参考逐行核过）：`isInSentWindow`（udp_channel.ipp:29）窗口 `[oldestUnackedSeq_, smallOutSeqAt_)` 由 `smallOutSeqAt_` 定界而非 `largeOutSeqAt_`；`addResendTimer`（cpp:817）先断言 oldest 槽占位、`oldestUnackedSeq_==SEQ_NULL` 时置 seq、`lastSentAtOutSeq_/lastSentTime_/wasResent_` 入槽、`lastReliableSendTime_=now`、`reliableOrders_.assign`、`seqMask(seq-oldest+1)>size()` 时 `doubleSize(oldestUnackedSeq_)`、槽内断言为空后插入；满窗判据 `seqMask(largeOutSeqAt_-oldestUnackedSeq_)>=windowSize_` → 前驱槽（`seqMask(smallOutSeqAt_-1)`）为空或老于 `minInactivityResendDelay_` 则 `sendUnacked(*unackedPackets_[smallOutSeqAt_])` 并推进、`checkOverflowErrors()`、返回 false，未满则 `smallOutSeqAt_=largeOutSeqAt_` 返回 true。`handleAck`（cpp:964）顺序为 有效性闸门→在窗内否→槽空否→未重发才更新 `roundTripTime_=((roundTripTime_*9)+(now-lastSentTime_))/10`（`RTT_AVERAGE_DENOM=10`）→临界槽命中清零→oldest 命中则自 `seqMask(seq+1)` 扫至 `largeOutSeqAt_` 取首个非空→`seqLessThan(highestAck_,seq)` 推进→删除置空→`while seqMask(smallOutSeqAt_-oldestUnackedSeq_)<windowSize_ && unackedPackets_[smallOutSeqAt_]` 补发并推进。`handleCumulativeAck`（cpp:899）：闸门→`!hasUnackedPackets()` 返 true→`seqLessThan(smallOutSeqAt_,endSeq)` 报「Received ACK for packets not yet sent」返 false→自 oldest 逐格 `handleAck` 至不含 `endSeq`。`checkOverflowErrors`（cpp:421）：`numOverflow=seqMask(largeOutSeqAt_-smallOutSeqAt_)`，`>max/2` 单次告警「Overflow packet list size (%u) exceeding safety threshold (%u)」并置 `hasSeenOverflowWarning_`，回落 `<max/3` 清除；`s_maxOverflowPackets_={1024,8192,4096}`（cpp:34-38 外部/内部/索引）。`sendUnacked`（cpp:1187）刷 `updateChannelVersion` + 发接口包 + `lastSentAtOutSeq_=smallOutSeqAt_`/`wasResent_=true`/`lastSentTime_=lastReliableResendTime_=now`。`CircularArray::doubleSize` 只搬运 `[start, start+size)` 于相同绝对偏移，故槽位跨倍增不动（circular_array.hpp:63）。
  - 有意偏离：(a) `UnackedPacket` 的 `initFromStream`/`addToStream` 随 BW-02-03-06（移植的 `Packet` 尚无流编码）——参考只写 packet 与 `lastSentAtOutSeq_`，`lastSentTime_` 故意不入流（收端以状态到达时刻打戳）、读入时 `wasResent_` 强制 false（unacked_packet.cpp:29-69）。(b) `sendUnacked` 只做簿记：`updateChannelVersion` 与 `pNetworkInterface_->sendPacket(...,isResend=true)` 随发送路径 BW-02-03-06；槽为空时静默跳过（参考会解引用空指针）。(c) `useNextSequenceID`（cpp:1728）暂不挂告警阈值与回调。(d) `checkOverflowErrors` 不挂 `s_assertOnMaxOverflowPackets_` 断言（参考默认关）。(e) `sendWindowUsage`/`numOutstandingPackets` 返回 `i32`（参考 `int`）。
  - 测试与覆盖：mercury **280 通过 / 0 失败**（较上轮 +18：回绕夹具、未来累计 ACK 拒绝、RTT 单样本、重发包 ACK 不采样、临界回执单随包、满窗告警—排空—阈值回落、倍增阈值、内部/索引各自上限）。workspace **942 passed / 0 failed / 33 ignored**。llvm-cov：circular_array.rs **100.00%**、unacked_packet.rs **100.00%**、udp_channel.rs **98.85%**（残 14 行=5 处收括号伪影 + 测试夹具 `Empty::handle_message` 9 行——回执只在真实请求往返才送达，构造路径不可达，该测试断言其永不被调用）；mercury 全 crate **99.52%** 行。clippy 零新增（仅既有 workspace 级 `python-host` 未用依赖告警）；rustfmt 本轮触及文件 0 差异（另有 8 个文件被早前 `cargo fmt -p mercury` 误改，已 `git checkout` 还原；本轮只对自己 4 个文件跑 rustfmt）。
  - 压力：mercury `--test-threads=1` **100 轮 28000 通过 / 0 失败**；python-host `--test-threads=8` **30 轮 1140 通过 / 0 失败**；全量输出 `grep -E "FAILED|panicked"` 0 命中。
- [x] **BW-02-03-04 实现重传计时**。**验收** 单次可控丢包后仅交付一次。
  - 新增：`irregular_channels.rs`（430 行，`IrregularChannels` 全接口共享重发定时器集合，`MonitoredChannels` 基类折入）；改 `udp_channel.rs`（2101→2609 行：regularity 双旗 + `numPacketsResent_` 计数 + `checkResendTimers`/`resend` + regularity setter 面 + condemn/destroy 挂账）、`network_interface.rs`（+51：持集合、`addIfNecessary`/`delIfNecessary`/period 设置/`checkIrregularChannels` 转发）、`lib.rs`（+1 模块）。
  - 验收测试 `a_single_controlled_loss_is_delivered_exactly_once`（对齐 test_reliable.cpp:398-447 `Bundle_loseOnePacketOfMany_IrregularChannel` 的计时面）：External 信道、双 regularity 旗关、period 0.1s、发 1 包即丢；19 tick（1.9s）0 重发、恰 2.0s（age==period，参考严格 `>`）仍 0、2.1s 恰 1 次重发且槽被重打戳（`lastSentTime()==3.1s`）、再走 20 tick 计数不涨；`handleAck` 排空至 `oldestUnackedSeq_==SEQ_NULL` 后下次扫描把信道从集合摘除，`packetsResent==1`。另有共享定时器驱动夹具（同一边界断言走 dispatcher 真计时器）与 NACK 夹具（regular 远端、ack 后包 → 前包按 `hasNewerAck` 重发且扫描止于 `highestAck_`）。
  - 合同要点（参考逐行核过）：**无 per-channel 重发计时器**——重发由「piggyback-on-bundle 前的 `checkResendTimers`」（udp_channel.cpp:1051）+「集合唯一共享定时器」（irregular_channels.cpp，默认 1s 周期逐成员 `channel.send()`）双路驱动。`checkResendTimers`：闸门（`oldestUnackedSeq_==SEQ_NULL` 返回；`hasRemoteFailed_` 告警返回）；`resendPeriod=max(roundTripTime_*2, minInactivityResendDelay_)`（外部 traits RTT 1s → 周期 2s，与参考测试注释「MinRTT is one second, so we resend after two seconds」互证）；`endSeq = isIrregular ? smallOutSeqAt_ : highestAck_`；`MAX_RESENDS=windowSize_/8`；逐槽 `hasNewerAck=seqLessThan(lastSentAtOutSeq_, highestAck_)` **不论 regularity 都算 NACK**，`shouldResend = hasNewerAck || (isIrregular && now-lastSentTime_ > resendPeriod)` 严格大于；判定按槽现状读（前槽重发刷新自身戳）。`resend`（cpp:1151）：`++numPacketsResent_`；piggyback 臂 → BW-02-03-06，本轮恒走 `sendUnacked`。regularity：构造双 true；`isLocalRegular(false)`（cpp:1702）set 后 `irregularChannels().addIfNecessary` 入集合；`isRemoteRegular(false)`（cpp:1717）只置旗；`condemn`（cpp:403-407）**先双旗置 false 再 `isCondemned_=true`**（不再活跃发送必须按 irregular 管账）；`clearState`（cpp:346）`delIfNecessary` 摘账。`addIfNecessary`（cpp:26）只收 `!isLocalRegular() && hasUnackedPackets()`，首成员按 `defaultPeriod()=1s` 起表；`setPeriod`（monitored_channels.cpp:24）取消+重臂（`seconds*1e6`），≤0 即停；`handleTimeout`（cpp:56）：成员 `hasUnacked && !isLocalRegular` 才查重发，`hasRemoteFailed` 者报 `Removing dead channel` 后除名，其余 `delIfNecessary`。
  - 有意偏离：(a) `resend` 不带 `UDPBundle` 参数、piggyback 臂（external+非分片+`smallOutSeqAt_` 有空位则 `bundle.piggyback`+`handleAck`）整体随发送路径归 BW-02-03-06，本轮恒走 `sendUnacked` 且返回 false。(b) `handleTimeout` 逐成员直呼 `check_resend_timers`（参考呼 `channel.send()`，wire send 同归 BW-02-03-06），共享定时器驱动的验收面等价。(c) `MonitoredChannels` 折入本模块，迭代器成员账 → `Rc::ptr_eq` 搜索（与 condemned 集同一取舍）。(d) `stopMonitoring` 落为 `set_period(0)`，方法本体不搬——树内唯一调用点目标是 keepAliveChannels（network_interface.cpp:259，后置任务）。(e) 告警按移植 `interface.is_verbose()` 门（参考 `isDebugVerbose()`）；dead-channel 告警保留参考原文。(f) `numPacketsResent_` 全生命周期不重置（参考析构前亦不重置，统计口径与既有计数一致）。
  - 测试与覆盖：mercury **295 通过 / 0 失败**（较上轮 +15：IrregularChannels 8 + 信道 7）。workspace **1238 passed / 0 failed / 61 ignored**（合 lane 后他 crate 计数已迁移，非本轮范围）。llvm-cov：irregular_channels.rs **99.58%**（残 1 行=定时器闭包 `if let` 收括号伪影，与 condemned_channels.rs:79 同形）、udp_channel.rs **99.06%**（98.85%→99.06%，残 14 行=5 处收括号伪影 + 测试夹具 `Empty::handle_message` 9 行，口径同上轮）、network_interface.rs **98.98%**（残 6 行=socket 创建/默认路由失败既有分支）；mercury 全 crate **99.53%** 行；circular_array.rs/unacked_packet.rs 仍 100%。clippy 零新增（仅既有 workspace 级 `python-host` 未用依赖告警；`IrregularChannels` 补 `#[derive(Default)]` 应对 clippy 建议）；rustfmt 本轮触及 3 个独立文件 0 差异。
  - 压力：mercury `--test-threads=1` **100 轮 29500 通过 / 0 失败**；python-host `--test-threads=8` **30 轮 1140 通过 / 0 失败**；全量输出 `grep -E "FAILED|panicked"` 0 命中。
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

- [x] **BW-03-01-01 初始化并退出 CPython**。**验收** 重复运行独立进程均正常退出。
  - 证据：`crates/python-host/src/{lib.rs,capi.rs,interpreter.rs,pyref.rs}` + `crates/python-host/build.rs`，`Cargo.toml` 取消注释 `python-host` workspace 依赖（BW-03 先例）。**原版结果**：`Script::init`（`lib/pyscript/script.cpp`）先写三个 legacy pre-init 全局量 `Py_FrozenFlag=1`/`Py_NoSiteFlag=1`/`Py_IgnoreEnvironmentFlag=1` 再调裸 `Py_Initialize`（原版**刻意未迁移** `PyConfig`，`python-313-migration.md` 记为未闭合的行为级变更，故本移植同样保留全局量写法），随后 `PySys_SetObject("argv", …)`（仅当 `g_scriptArgc`）、`Script::disablePythonGarbage` 导入 `gc` 并 `callMethod("disable")`、置 `s_isInitalised=true`；已初始化则 `if (s_isInitalised) return true;` 直接返回。`Script::fini` 顺序为 `while (PyGC_Collect() > 0);` 排空引用环 → 抹白 `__main__`（字典内除 `__builtins__` 全部置 `None`，再 `sys.modules["__main__"] = None`，原版内联重实现了 3.x 已删除的 `_PyModule_Clear`）→ `Py_Finalize` → 清标志位。**新版结果**：`Interpreter::init/fini`（interpreter.rs:71-149）逐步同序，`InitError::{InitializeFailed,ArgvFailed,GarbageCollectionUnavailable}` 三臂对齐三处原版失败点（`Py_IsInitialized()==0`、argv 列表构建失败、`gc` 导入失败——原版清错继续跑，本移植改为报错以免半初始化）；`PyRef`（pyref.rs:29-87）为原版手管 `Py_IncRef/Py_DecRef` 的 RAII 等价：`Clone`=`Py_XINCREF`、`Drop`=`Py_XDECREF`、空句柄（`NULL`）不递减、`Script::fini` 之后不递减（`capi::is_finalized()` 门禁，避免对已拆除的分配器做 DECREF）。**验收**：`repeated_independent_processes_exit_cleanly` 连跑 5 遍独立进程 `exit 0`，`every_scenario_exits_cleanly` 逐条 spawn 子进程断言 exit 0；`python_path_is_ignored_when_the_flag_is_set` 以 `PYTHONPATH=/no/such/python/path` 验证 `Py_IgnoreEnvironmentFlag` 生效。证据链接：`crates/python-host/src/interpreter.rs:63-149`（init/fini）、`pyref.rs:29-108`（Clone/Drop）、`capi.rs:95-100`（`PyModule_Check` 宏重实现——C 宏不导出，改用 `Py_TYPE`+`PyType_IsSubtype`+`PyModule_Type`）、`build.rs:73-81`（链接探测）。**平台差异如实登记**：验收口径定为「独立进程」而非同进程重复初始化，因 CPython 3.13+ 拒绝 `Py_Finalize` 后再 `Py_Initialize`（`Fatal Python error: _PyImport_Init: global import state already initialized`），且原版本就从不重初始化（reload 用 `Py_NewInterpreter` 换子解释器，见 BW-03-04-03）；故每个 scenario 各自 `#[ignore]` 单进程跑，父进程驱动。**版本差**：本机构建 CPython 为 3.14.9 而非 3.13.16（参照树 pin 版），`build.rs` 从 `BOUNDLESS_PYTHON`/`python3` 的 `sysconfig` 取 `LIBPL`+`LDVERSION`，3.13 机器自动链接 `libpython3.13`；3.13+ 已删 `sys.flags.frozen`（Android-only），观测面改用 `sys.flags.no_site`+`sys.flags.ignore_environment`，三个 C 全局量由 `capi::pre_init_flags()` 回读（interpreter.rs:352-362）。**覆盖发现的真实缺陷**：`PyRef` 原为派生 `Clone`（不 IncRef），两个句柄会各减一次同一引用 → 提前释放；改为手写 `Clone`，并以 `sys.getrefcount(__handle)` 钉死 clone=+1、drop clone=复原（interpreter.rs:583-609）。测试：python-host 14 passed / 14 ignored（in-process 驱动）、0 警告；workspace 488 passed 0 failed。llvm-cov：pyref.rs 100%、interpreter.rs 95.65%、capi.rs 94.44%、crate 合计 96.07%（余 18 行为 C API 失败臂：`PyList_New`/`PyUnicode_FromString` 返回 null、`import gc` 失败、`PyErr_Clear`、`PyModule_GetDict` null——健康 CPython 上不可触发，按约定不加故障注入脚手架（commit `02a009a`）。
- [x] **BW-03-01-02 配置标准库和资源导入路径**。**验收** 原版脚本可原样导入。
  - 证据：`crates/python-host/src/import_paths.rs` + `interpreter.rs` 接线。**原版结果**：`PyImportPaths`（`lib/pyscript/py_import_paths.cpp`）是有序去重列表——`addResPath` 把资源树内路径按每个 `BWResource` 根展开（`getPath(0..getPathNum())` 逐个拼 `root + "/" + path`）、`addNonResPath` 已存在则不重复入列（保插入序）、`append` 把对方的路径排在本方之后、`pathsAsString` 按分隔符拼接、`pathAsObject` 逐项建 `PyUnicode`（转换失败即跳过、不回填空洞）。`Script::init` 的组装顺序：`sysPaths.append(appPaths)` → `addResPath("scripts/common")`（EntityDef::Constants::commonPath）→ `addResPath("scripts/common/Lib")` → 服务器臂 `addResPath("scripts/server_common/lib-dynload-<buildStr>" + 非混合构建再拼 "_<BW_COMPILE_TIME_CONFIG>")`（Windows 臂为 `scripts/client/DLLs/<platform>`，`#if defined(_WIN64)||defined(_WIN32) ... #else`）→ `PySys_SetObject("path", pSys)` + `Py_DECREF`，失败则 `ERROR_MSG("Unable to assign sys.path")` 且 init 返回 false。**关键语义**：`sys.path` 是**替换**不是追加——纯 Python 标准库模块（如 `json`）在原版配置下同样不可达，只有冻结/内建模块照常可用。**新版结果**：`ImportPaths`（import_paths.rs:33-139）五件套同语义；`role_paths(app, roots, platform, config)`（import_paths.rs:167-190）复刻该组装（`config=None` 即 `BW_CONFIG_HYBRID` 无后缀；`cfg(windows)`/`cfg(not(windows))` 两臂同原版条件编译）；`InitConfig.import_paths` 接入 `Interpreter::init`，赋值点在 argv 之后、`gc.disable()` 之前（与原版顺序一致），失败臂新增 `InitError::PathFailed` 对齐原版 ERROR_MSG+return false。**验收**：`scenario_a_script_under_the_resource_root_imports` 以临时资源根铺 `scripts/common/probe_script.py`，`role_paths` 展开后 `import probe_script` 读回 `VALUE=7`；`scenario_sys_path_is_the_assembled_list` 断言 `sys.path` 恰 4 项且第 4 项为 `/res/scripts/server_common/lib-dynload-linux-x86-64_server`；`scenario_an_unencodable_path_fails_init` 证明含内嵌 NUL 的路径使 init 以 `PathFailed` 失败并完全回卷。**delta 如实登记**：① 分隔符——原版传 `osdefs.h` 的 `DELIM`（Windows/VxWorks `';'`，其余平台无定义落到 `DEFAULT_DELIMITER` 也是 `';'`），本移植取平台缺省（Windows `';'`、其余 `':'` 即 Python `os.pathsep`），模块文档注明；② 资源根接口化——`add_res_path(roots, path)` 的根列表由调用方传入而非读 `BWResource` 全局（本仓尚无全局 BWResource 实例），且 `crates/resources/src/zip.rs:1140` 存在**既有** clippy error（`read amount is not handled`，非本 lane 引入），python-host 若依赖 resources 会被连坐阻塞门禁，故根列表接口化、待角色运行时（BW-03-01-03 消费方）由调用方传 `bwresource::BwResource::get_path` 列表，该 clippy error 提请 resources 属主修复；③ 路径含 NUL 时本移植拒绝整个列表而非跳过该项（原版 `bw_utf8tow` 不会失败、真失败会在列表里留 `NULL` 槽），选拒绝已注明。测试：python-host 23 passed / 17 ignored、0 警告；workspace 497 passed 0 failed。llvm-cov：import_paths.rs 97.18%、pyref.rs 100%、interpreter.rs 96.47%、capi.rs 94.44%、crate 合计 96.74%（余 22 行为 C API 失败臂：`PyList_New`/`PyUnicode_FromString` null、`import gc` 失败、`PyErr_Clear`、`Py_IsInitialized()==0`、`PyModule_GetDict` null——健康 CPython 上不可触发，无故障注入脚手架（commit `c2d0eb1`）。
- [x] **BW-03-01-03 按角色注册原生模块**。**验收** 实际导入结果与角色基线一致。
  - 证据：`crates/python-host/src/{lib.rs,capi.rs}` 新增 + `init_time_job.rs` 新建 + `interpreter.rs` 接线。**原版结果**：`Script::init`（`lib/pyscript/script.cpp`）在 `PySys_SetObject("argv")` 之后依次执行 `runInitTimeJobs()`（script.cpp:584）→ `Script::disablePythonGarbage()` → `ScriptModule::getOrCreate("BigWorld")` + `setAttribute("component", componentName)`（componentName 为 `cell`/`base`/`client`/`database`/`bot`/`editor` 之一，script.cpp:599 注释）→ `s_pOurInitTimeModules = PyDict_Copy(PySys_GetObject("modules"))`（:602，快照供 `Script::createInterpreter` 把 init 期导入过的模块合入新建子解释器，BW-03-04-03 消费）。`runInitTimeJobs` 按 `map<int, vector<job*>>`（`lib/script/init_time_job.cpp:21`）键升序、组内插入序排空；二次调用 `ERROR_MSG`；init 后再构造 `InitTimeJob` 触发 `CRITICAL_MSG`；map 排空后删除；`FiniTimeJob` 默认 rung 1。**新版结果**：`init_time_job.rs` 以 `BTreeMap<i32, Vec<Job>>` + `OnceLock<Mutex<Registry>>` 复刻双注册表——`add_init_time_job(rung)`/`run_init_time_jobs`/`add_fini_time_job[_at]`/`run_fini_time_jobs`；run 前置 `has_run=true` 并在持锁时 `std::mem::take` 排空（「排空后删除」天然成立），故运行中注册的作业返回 `Err(JobError::AlreadyRan)` 而非死锁，二次运行同报 `AlreadyRan`。**delta**：原版 init 后构造作业为 `CRITICAL_MSG`（进程级告警中止语义），本移植是 Result 错误码（测试可断言）；另加 `std::sync::Mutex` 中毒恢复（`unwrap_or_else(into_inner)`，C++ 无锁故无此面）。`Interpreter::init` 顺序同原版：argv → `run_init_time_jobs()`（interpreter.rs:124）→ `disable_garbage()` → `create_big_world_module(component)`（interpreter.rs:270，`PyImport_AddModule`+`PyObject_SetAttrString`，失败按 `ScriptErrorPrint` 静默继续）→ `take_modules_snapshot()`（interpreter.rs:295，`PyDict_Copy`）。**覆盖发现的真实缺陷**：快照 DECREF 原排在 `Py_Finalize` 之后，`_Py_Dealloc` 对已拆除分配器 free → SIGSEGV（gdb 回溯定位 `fini()`→`dec_ref`→`_Py_Dealloc`）；按原版 script.cpp 的 DECREF 位置移到 collect 循环之前修复。**验收**：`scenario_init_runs_the_init_time_jobs` 在 rung 0 注册作业断言其先于 `gc.disable()` 执行（job 内 `gc.isenabled()==True`，init 后为 `False`）；`scenario_init_time_jobs_run_once_in_rung_order` 证 rung 升序+组内插入序+只跑一次；`scenario_fini_time_jobs_run_once_in_rung_order` 同证 fini 侧；`scenario_big_world_module_carries_the_component` 证 `BigWorld.component=='cell'` 且 `'BigWorld' in sys.modules`；`scenario_a_component_with_a_nul_stays_unset` 证 component 含 NUL 时属性留空（`ScriptErrorPrint` 语义）；`scenario_init_time_modules_snapshot_taken_at_init` 证快照=init 末模块表、`Interpreter::init_time_modules()` 访问器、fini 后回 null；单元测试覆盖 rung 序、二次运行、运行中注册、锁中毒恢复（`init_time_job.rs` 行级全覆盖）。**证据链接**：`interpreter.rs:124-133`（init 接线）、`init_time_job.rs`、`capi.rs:206`（`PyObject_SetAttrString`）、`capi.rs:170`（`PyDict_Copy`）、`interpreter.rs:270-293`（BigWorld 模块/快照）、`interpreter.rs:179-199`（fini 顺序+DECREF 修复位）。**平台差异如实登记**：① 角色清单未做枚举校验——`InitConfig.component` 为调用方字符串（本车道无角色运行时，待 BW-03-04 消费）；② `sys.path` 替换使快照只含内建模块+init 期资源根导入的模块（BW-03-01-02 已登记）；③ 本项无平台差（CPython API 跨平台一致）。测试：python-host 26 passed / 23 ignored、0 警告；workspace 500 passed 0 failed。llvm-cov：init_time_job.rs 95.96%（lcov DA:0 行为 0——摘要口径含测试码行）、interpreter.rs 96.88%、capi.rs 98.61%、import_paths.rs 97.18%、pyref.rs 100%、crate 合计 97.17%（余 27 行全部为 CPython 故障臂：`Py_IsInitialized()==0`、`PyList_New`/`PyUnicode_FromString`/`PyObject_SetAttrString` 返回 null、`import gc`/`getattr("disable")`/`call` 失败、`PyImport_GetModuleDict`/`PyModule_GetDict` null——健康解释器不可触发，按约定不加故障注入脚手架（commit `9fa0107`）。
- [x] **BW-03-01-04 接入脚本日志**。**验收** stdout 和异常落入预期日志入口。
  - 证据：`crates/python-host/src/output_writer.rs` 新建 + `interpreter.rs` 接线 + `capi.rs` 扩展（`PyCFunction_New`/`PyArg_ParseTuple`/`PyTuple_GetItem`/`PyObject_Repr`/`PyErr_Display`/`PySys_WriteStderr`/`METH_VARARGS`/`PyMethodDef`/`PyTraceBack_Type`/`py_traceback_check`）+ `lib.rs` 导出。**原版结果**：`Script::init`（script.cpp:531）`MF_VERIFY( new ScriptOutputWriter )` 把 `sys.stdout`/`sys.stderr` 换成 `PyOutputStream`（`lib/script/py_script_output_writer.cpp`，C heap type，成员 `isStderr_`），其 `write` 以 `PyArg_ParseTuple("s#")` 取参：含内嵌 NUL 改记 `PyObject_Repr` 的结果，否则原文交给 `handleWrite`——按流缓冲、收到结尾 `\n` 才冲刷（剥一个尾换行、按 `\n` 切行、每行一条 LogMsg：stderr→`MESSAGE_PRIORITY_ERROR`、stdout→INFO、source 均为 SCRIPT），冲刷后 hook 链收**原始分块**；hook 可在回调内自删（`pCurrentHook_`/`shouldDeleteCurrentHook_` 配对，删除延到回调返回后生效，`delHook` 删别的 hook 立即生效、同一 hook 加两次须删两次），未重定向时 `getCurrentStdio()==NULL` 拒绝挂 hook。异常面：`lib/pyscript/py_traceback.cpp:533` 的 `printTraceBack` excepthook——`PySys_WriteStderr("Traceback (most recent call last):\n")`、逐帧 `  File "f", line N, in name` + 源码行（原版经 Mercury EventDispatcher **异步**读源文件，outputFrame :436）、沿 `tb_next` 走完、`PyErr_Display(type, value, NULL)` 收尾；`initExceptionHook`（:575）`PyCFunction_New` + `PySys_SetObject("excepthook")`，由各进程 main 在 `Script::init` 后显式调（cellapp.cpp:3622/baseapp.cpp:1304/dbapp.cpp:324/bots main_app.cpp:273）。init 尾部（:617）`import bw_site`，失败「Script::init: Unable to import bw_site」返回 false。**新版结果**：`output_writer.rs` 同序复刻——`install()`（:414，guard 对齐原版 MF_VERIFY → `InitError::StdioFailed`）以 Python bootstrap 类 `PyOutputStream(write)` 绑定两个 `PyCFunction_New(PY_WRITE_DEF)` 内建 `write`（`Py_True`/`Py_False` 作 isStderr 标志）替换 stdio；`py_write`（:221）解析 `s#:write`、NUL 走 `PyObject_Repr` 臂、坏参 TypeError 上抛；`handle_write`（:168）双流缓冲 + 尾 `\n` 冲刷进 `LogSink`（`set_log_sink` 可换、缺省 `console_sink` 走 stderr，INFO/ERROR 以 `is_stderr` 布尔交付）再跑 hook 原始分块；`remove_hook`（:131）回调内经 `thread_local CURRENT_HOOK/DELETE_CURRENT`（:90，原版 `pCurrentHook_`/`shouldDeleteCurrentHook_` 的等价物）延迟自删；`print_trace_back`（:260）解析 `OOO:printTraceBack`、`py_traceback_check` 不过则 `PyErr_Display` 兜底、逐帧 `write_frame`（:309）`tb_lineno/tb_frame/f_code/co_filename/co_name` + `read_source_line`（:349）同步读源码行、尾 `PyErr_Display`；`install_exception_hook`（:475）设 `sys.excepthook`。`Interpreter::init` 全序同 script.cpp（install → argv → sys.path → init jobs → gc.disable → BigWorld → 快照 → `import_bw_site`（interpreter.rs:348，失败 `InitError::BwSiteImportFailed` + 完整回卷）→ excepthook），`fini` 经 `reset()`（:532）丢缓冲与 hook、保留宿主 sink。**验收**：`scenario_stdout_lands_in_the_log_sink`/`scenario_stderr_lands_in_the_log_sink` 证 print 落 sink 且软空格 `softspace` 属性在位；`scenario_partial_writes_buffer_and_hooks_see_chunks` 证半行缓冲、hook 收原始分块、fini 后挂 hook 被 `NoStdioWriter` 拒；`scenario_a_hook_may_remove_itself_mid_callback` 证回调内删他者被拒、自删延迟生效、后续 hook 不丢块、回调外按 id 即删+重复删报 `NoSuchHook`；`scenario_an_embedded_nul_logs_the_repr` 证 NUL 走 repr 臂且 repr 无尾换行故随下一换行写合并冲出（与原版 handleWrite 缓冲语义一致）；`scenario_the_excepthook_formats_the_traceback` 证头行+`  File "…/boom.py", line 2, in boom`+源码行+`ValueError: boom message` 全走 stderr；`scenario_the_excepthook_handles_a_missing_traceback` 证无 traceback 落 `PyErr_Display`、参数不足三静默返回 None；`scenario_bad_write_arguments_raise` 证 `write()` 坏参 TypeError 上抛；`scenario_bw_site_is_imported_at_the_tail_of_init`/`scenario_missing_bw_site_fails_init` 证 bw_site 导入与失败回卷；`hooks_refuse_without_the_redirect` 证未重定向拒绝挂 hook。**覆盖发现并修复的两个真实缺陷**：① `install()` 原整块重置 `WriterState` 把宿主 init 前预设的 sink 抹掉 → 拆出保 sink 的 `clear_writer()`；② `remove_hook` 自删死锁——`handle_write` 持非重入 `std::Mutex` 跨回调、回调再锁同线程卡死（测试挂 600s 定位），以 thread_local 重入通道修复。**delta 如实登记**：① `PyOutputStream` 用 Python bootstrap 类而非 C heap type（省 `PyType_FromSpec` slot 机制，可观测行为同：write/softspace）；② isStderr 以 `Py_True`/`Py_False` 标志对象挂内建 self，原版为 C++ 成员；③ 日志入口接口化——`LogSink` 回调替代 cstdmf LogMsg（本 crate 不依赖 cstdmf，优先级映射留给消费方）；④ traceback 源码行**同步**读，原版经 Mercury dispatcher 异步读（无 dispatcher 层，见 `install_exception_hook` 文档）；⑤ excepthook 折叠进 `Interpreter::init`，原版由各进程 main 显式调；⑥ 非 UTF-8 分块 `from_utf8_lossy` 防御性降级（`"s#"` 实参本已强制 str→UTF-8，正常不可达）；⑦ 回调内 `remove_hook` 仅允许删当前 hook（原版无锁可删任意 hook），std Mutex 不可重入的补偿；⑧ 退出期 `no attribute 'flush'` 噪音忠实原版（PyOutputStream 无 flush 属性）；⑨ `bw_build_platform()`/`platform_mismatch()`（import_paths.rs:165-190，对齐 script.cpp:486 平台不匹配 WARNING）已备、随角色运行时接线（本项未接）。**平台差异**：本项无平台差（stdio 换绑与 `PySys_WriteStderr` 跨平台一致）。测试：python-host 38 passed / 33 ignored、0 警告（唯一 warning 为根 Cargo.toml 既有 `unused workspace dependency`，非本车道）；workspace 512 passed 0 failed。llvm-cov：output_writer.rs 96.77%、interpreter.rs 97.51%、capi.rs 98.84%、import_paths.rs 95.21%、init_time_job.rs 95.96%、pyref.rs 100%、crate 合计 97.18%（余 46 行为 CPython 故障臂——`PyCFunction_New`/`PyObject_SetAttrString`/`PyObject_Repr`/`PyObject_Str` 返回 null、静态字面量 `CString::new`、源码行含 NUL 的 `CString::new`、traceback 帧缺 `tb_frame`/`f_code`——健康解释器不可触发，按约定不加故障注入脚手架（commit `7708ec3`）。
- [x] **BW-03-01-05 核查 Python 关服顺序**。**验收** 脚本清理回调满足宿主释放时序。
  - 证据：`interpreter.rs` `fini()`（interpreter.rs:217）重排 + 新 scenario；对照 `lib/pyscript/script.cpp:659-741`。**核查发现两处序错并修复**：① `runFiniTimeJobs()`（script.cpp:689，「Personality::onFini called as last item here」）在旧 `fini()` 里只有注释没有调用——fini 作业从未随关服跑过（此前 `scenario_fini_time_jobs_run_once_in_rung_order` 只手工驱动注册表）；② `output_writer::reset()` 原排在作业之前，会把宿主 hook 先于 `Personality::onFini` 等回调拆掉，而原版 writer 到 `Py_Finalize` 的 sys 清理才死。**原版结果**：`Script::fini(shouldFinalise)` 顺序为——`!s_isInitalised` 直接返回 →（ENABLE_STACK_TRACKER/PROFILER 时 `PyEval_SetProfile(NULL,NULL)`）→ `FileIOTaskManager::stopAll()` + `BgTaskManager::stopAll()`（各以 `pInstance() != NULL` 护住，注释明言「Stop background tasks before calling Py_Finalize() because they might be loading PyObjects or have script callbacks」）→ `BWConcurrency::setMainThreadIdleFunctions(&NoOp,&NoOp)` → `Pickler::finalise()` → `runFiniTimeJobs()` → `Py_DECREF(s_pOurInitTimeModules)` 置 NULL →（ENABLE_WATCHERS 时 `Watcher::fini()`）→ `shouldFinalise` 臂：`while (PyGC_Collect() > 0);` 排空引用环 → `__main__` 抹白（字典属性除 `__builtins__` 全置 `None`——3.13 迁移内联复刻已删的 `_PyModule_Clear`——再 `sys.modules["__main__"]=None`）→ `Py_Finalize()` → `s_isFinalised=true`。注意原版 collect 循环在 `__main__` 抹白**之前**。**新版结果**：`fini()`（interpreter.rs:217）现为同序——`run_fini_time_jobs()`（interpreter.rs:224，二次驱动 `AlreadyRan` 按原版 ERROR_MSG-and-continue 语义忽略）→ 快照 swap+DECREF（位置介于 jobs 与 collect 循环之间，BW-03-01-03 的 SIGSEGV 修复位）→ `output_writer::reset()` 移到作业之后（回调的 `print` 仍达 hook 与 sink，同原版）→ `while PyGC_Collect()>0` → `clear_main_module()`（除 `__builtins__` 全置 None + `sys.modules["__main__"]=None`，同序在 collect 之后）→ `Py_Finalize` → 清标志。**验收**：`scenario_fini_runs_the_fini_time_jobs`（interpreter.rs:1136）——rung 0 作业内 `run_simple_probe("print('from fini job'); __probe = True")` 证回调期解释器仍活、脚本调用仍可达 CPython、输出仍落 sink，`Interpreter::is_initialized()==true` 钉死「作业先于终结」；rung 2 作业证 rung 序（onFini 挂末 rung）；scenario 结束后断言事件序 `[sink:from fini job, job:alive=true, job:onFini-rung]`、其后仅剩 BW-03-01-04 已登记的退出期 flush 噪音、`is_finalized()==true`。**delta 如实登记**：① 后台任务停止点移交进程层——原版在 `Script::fini` 内经管理器单例 `pInstance()!=NULL` 护着调 `stopAll`，本移植 python-host 与 server-runtime 解耦（依赖方向倒置），停任务属宿主进程关服路径（server-runtime 已有 `TaskManager::stop_all`/`drain`，commit 4dfca51），时序约束写入 `fini()` 文档（宿主须先停后台任务再调 fini）；② `Pickler::finalise()` 缺席——Pickler 未移植（本车道无 pickler 模块，待后续项）；③ `setMainThreadIdleFunctions`/`Watcher::fini()`/`PyEval_SetProfile` 缺席——并发/观察者/profiler 层未移植；④ `shouldFinalise` 参数塌缩——本移植恒终结（既有决定，文档注明），原版 client-module 构建可跳 `Py_Finalize` 仍置 `s_isFinalised`；⑤ 退出期 flush 噪音随 `Py_Finalize` 落 sink、在作业之后——忠实原版（PyOutputStream 无 flush 属性），scenario 显式断言之。**平台差异**：本项无平台差（关服序列平台无关）。测试：python-host 38 passed / 34 ignored、0 警告（唯一 warning 为根 Cargo.toml 既有 `unused workspace dependency`）；workspace 512 passed 0 failed。llvm-cov：interpreter.rs 97.62%、output_writer.rs 96.77%、capi.rs 98.84%、import_paths.rs 95.21%、init_time_job.rs 95.96%、pyref.rs 100%、crate 合计 97.25%（余 46 行同前为 CPython 故障臂与健康解释器不可触发路径，按约定不加故障注入脚手架（commit `c085671`）。

#### BW-03-02 Python 对象桥接

覆盖原生对象包装、属性与方法绑定、容器代理、引用保活、GC、GIL 和跨线程回调约束。比较对象身份、异常类型、回调析构与循环引用。

- [x] **BW-03-02-01 实现原生包装对象的身份缓存**。**验收** 同一底层对象取回同一实例。
  - 证据：`crates/python-host/src/wrapper.rs` 新建 + `capi.rs` 扩展（`PyCapsule_New`/`PyCapsule_GetPointer`/`PyCapsule_IsValid`/`PyCapsuleDestructor`/`refcount`）+ `interpreter.rs` fini 接线 + `lib.rs` 导出。**原版结果**：包装即嵌入——`PyObjectPlus`（`lib/pyscript/pyobject_plus.hpp:1087`）把 `PyObject` 头嵌进 C++ 对象自身，「同一底层对象取回同一实例」由构造保证：原生对象就是那个 Python 对象；查表层从不二次包装——`PyEntities::subscript`（`cellapp/py_entities.cpp:117-130`）`CellApp::findEntity(id)` 找到原生实体后 `Py_XINCREF` 原样奉还，原生 map（CellApp 实体表）全程持有对象所有权，弱引用面由 `PyObjectPlusWithWeakReference`/`PY_WEAK_REFERENCABLE`（pyobject_plus.hpp:1237、`_py_pWeakRefList` + `PyObject_ClearWeakRefs`）提供；计数的自检面是 `PyObjectPlus::refCount()`（pyobject_plus.hpp:1100）与 cstdmf `hasZeroReferenceCount` 直读 `ob_refcnt`（pyobject_pointer.hpp:40）。**新版结果**：Rust 对象无法嵌入 CPython 头，故换成「原生侧注册表持有唯一包装」的同所有权形状——`wrapper.rs` 以 `BTreeMap<u64, usize>`（地址存 `usize` 保持 `Send`；`PyRef` 本身刻意 `!Send`，对齐原版 `Script::acquireLock` 括号）缓存每 id 一个 `PyCapsule`（capsule 携带原生指针，`PyCapsule_IsValid(名)` 即类型判别；capsule 是嵌入头的端口替身，属性/方法绑定的堆类型归 BW-03-02-02）；`unsafe wrap(native, id)`——已注册则 `inc_ref` 后 `PyRef::borrowed` 过户（`PyEntities::subscript` 的 `Py_XINCREF`-并-奉还），首见则 `PyCapsule_New` 注册（注册表自持一引用）；`unsafe native(&PyRef)` 回读原生地址（非本模块 capsule 返回 None，外来对象不冒充）；`release(id)` 宿主弃对象时忘缓存条目并释放注册表那份引用；`reset()` 在 `Interpreter::fini` 的 fini 作业之后、快照 DECREF 之前清空（原生注册表先于脚本系统终结——`CellApp::shutDown` 删实体先于 `Script::fini`），全部 DECREF 落在分配器存活期内；`capi::refcount()` 直读 `ob_refcnt` 供测试自检。**验收**：`scenario_the_same_native_id_wraps_to_one_object`（两次 `wrap` 指针相等 + Python 侧 `one is two` == True + `native()` 回读同地址）；`scenario_a_forgotten_id_wraps_to_a_new_object`（Python 先持住旧 wrapper 再 `release`——首跑发现未持住时 pymalloc 复用了已释放 capsule 的地址造成假等，属测试缺陷非实现缺陷——重包后 `kept is fresh` == False）；`scenario_a_foreign_object_carries_no_native_id`（普通 `str` 判 None）；`scenario_different_ids_are_distinct_wrappers`（同地址两个 id → 两个不同对象，注册表按 id 键不按指针）；`scenario_the_cache_holds_one_reference_per_identity`（wrap 后 `ob_refcnt`==2＝注册表+调用方，再 wrap ==3，句柄 drop 回 2——钉死每 id 恰一份缓存引用）。**TDD 抓出的真实缺陷**：① `wrap` 命中路径初版写 `PyRef::borrowed(ptr).clone()`——clone 的 INCREF 与临时句柄 Drop 的 DECREF 对消，返回的是不拥有引用的幻影句柄（refcount 场景断言 3 得 2 暴露），改为显式 `inc_ref` + `borrowed` 过户；② 注册表 insert 初版未自增引用（注册表与调用方共享一条引用，首个 `release` 会释放仍被持有的 wrapper），改为 forget-clone 自持。**delta 如实登记**：① capsule 代替嵌入头——无 `__dict__`/弱引用槽，Python 侧观察面仅身份与指针（属性/方法/弱引用待 BW-03-02-02/04 堆类型）；② 注册表以 id 为键——同地址不同 id 得两个 wrapper（原版嵌入头下同一原生对象不可能有两个 PyObject，端口按 id 键与原版「每原生对象一包装」在 id↔对象一一对应时等价，登记之）；③ `refcount` 读 `ob_refcnt` 依赖 3.13 前向兼容的稳定头两字段布局（capi.rs 已有同构宏重实现先例）；④ `reset()` 挂在 `fini` 而非进程层显式调用（原版原生 map 死于进程层关服，端口无进程层，BW-03-04 消费时移走）。**平台差异**：本项无平台差。测试：python-host 42 passed / 39 ignored、0 警告；workspace 516 passed 0 failed。llvm-cov：wrapper.rs 96.05%（余 3 行：`PyCapsule_New` null 失败臂——健康 CPython 不可触发；`release_reference` 的 finalize 后防御性跳过——reset 先于终结排空故不可达）、interpreter.rs 97.78%、capi.rs 98.95%、crate 合计 97.31%。（commit `cec4274%（commit `cec4274`）。
- [x] **BW-03-02-02 实现属性和方法绑定**。**验收** 错误参数抛出约定异常。
  - 证据：`crates/python-host/src/binding.rs` 新建 + `capi.rs` 扩展（`PyType_FromSpec`/`PyType_GenericAlloc`/`PyType_GetName`/`PyObject_GenericGetAttr`/`PyObject_GenericSetAttr`/`PyErr_SetObject`/`PyErr_SetString`/`PyUnicode_FromFormat`/`PyExc_TypeError`/`PyGetSetDef`/`PyTypeSlot`/`PyTypeSpec`/`Py_tp_*` 槽常量 + 描述符结构尺寸测试）+ `interpreter.rs` fini 接线与 5 个新 scenario + `lib.rs` 导出。**原版结果**：每个暴露类由 `PY_TYPEOBJECT` 宏声明一个进程级 `static PyTypeObject s_type_`（`lib/pyscript/pyobject_plus.hpp:116`）——一个类名即一个类型对象；协议槽取通用实现：`pyGetAttribute`=`PyObject_GenericGetAttr`、`pySetAttribute`=`PyObject_GenericSetAttr`（`NULL` 值即删除，`pyobject_plus.cpp:89-113`）、`pyRepr`=`"%s at %p"`（`:101`）、`_pyNew` 拒绝直接构造并抛 `"%s() Cannot directly construct objects of this type"`（`:150`）；方法表为 `METH_VARARGS|METH_KEYWORDS` 处理器，用 `PyArg_ParseTuple` 解析（pyobject_plus.hpp:889-908），解析失败由 API 抛约定 `TypeError`。**新版结果**：`binding.rs` 以 `TypeSpec` 构造器（`.attribute(name, get, set)` / `.method(name, handler)`）+ `create_type`（按名注册表幂等：命中即 `inc_ref` 过户，等价 `s_type_` 的进程级唯一）+ `instantiate`（`PyType_GenericAlloc`，引擎构造路径；脚本侧走 `tp_new` 被拒）+ `unsafe qualified_name`（注册表反查，外来类型走 `PyType_GetName`）+ `reset()`（`Interpreter::fini` 在 fini 作业之后、快照 DECREF 之前释放注册表引用，interpreter.rs:226-234）；槽位填 `tp_getattro`/`tp_setattro`=Generic*、`tp_new`=拒绝、`tp_repr`=地址格式、`tp_methods`/`tp_getset` 指向泄漏的描述符数组（静态存储等价）。**验收**：`scenario_a_bound_attribute_round_trips`（读 `4` → 写 `7` → 读回 `7`，经通用协议往返）；`scenario_a_bound_type_refuses_the_wrong_access`（只读属性赋值 → `AttributeError`、未知属性 → `AttributeError`、`del` 绑定属性 → `TypeError`）；`scenario_a_bound_method_parses_its_arguments`（`twist(5)`==`10`、缺参 → `TypeError`、传字符串 → `TypeError`）；`scenario_a_script_cannot_construct_a_native_type`（`native()` → `TypeError`，消息含 `Cannot directly construct objects of this type` 且以 `boundless.Demo()` 起头）；`scenario_an_instance_renders_with_the_reference_format`（`repr` 以 `boundless.Demo at 0x` 起头）；`scenario_a_type_name_creates_one_type_object`（同名两次 `create_type` 同一指针、两次 `instantiate` 不同实例、外来类型 `bool` 经 `PyType_GetName` 回读）。**delta 如实登记**：① 堆类型 `PyType_FromSpec` 代替静态 `PyTypeObject` 宏（Rust 无 C 指定初始化器），描述符数组与 spec 泄漏＝静态 `s_type_` 永不消亡的等价存储，`basicsize`=`sizeof(PyObject)`、`flags`=0（本机 `Py_TPFLAGS_DEFAULT`==0）；② 方法表只用 `METH_VARARGS`（本移植无 kwargs 参数面），参考另置 `METH_KEYWORDS`；③ 描述符数组按名注册表持有、`reset()` 在 fini 排空（同 wrapper.rs 先例）。**平台差异如实登记**：`tp_repr` 只实现非 Windows 的 `"%s at %p"`（本机 printf 风格带 `0x` 小写十六进制），Windows 的 `"%s at 0x%p"` 未实现；本机 CPython 3.14.9（参照树 pin 3.13.16），`Py_tp_*` 槽常量按本机头核验（`Py_tp_getattro`=58、`Py_tp_setattro`=69、`Py_tp_doc`=56、`Py_tp_methods`=64、`Py_tp_new`=65、`Py_tp_repr`=66、`Py_tp_getset`=73），`capi.rs` 加描述符结构尺寸测试防槽布局漂移。**TDD 抓出的真实缺陷**：① `PyExc_TypeError` 是 `PyObject*` 符号，必须传其**值**——传变量地址多一层间接，`PyErr_SetString` 收到野指针，Python 侧表现为 `SystemError: _PyErr_SetObject: exception OverflowError<class 'KeyboardInterrupt'> is not a BaseException subclass`（随机内存被读成异常类型）；② `tp_new` 失败必须返回 `NULL` 并保留异常，返回非空让 `type_call` 抛 `SystemError: … returned a result with an exception set`；③ 可变参 `PyErr_Format` 不得零参调用（x86-64 上残留垃圾入栈同样污染异常态），拒绝路径改用固定参数 `PyErr_SetObject`；④ `PyUnicode_FromFormat` 的 `%s` 期望 C 字符串，传 `PyObject*` 打出 `\x01`，改用 `%U`。**附带修复（commit `efda5a1`）**：`output_writer` 的单元测试与解释器内进程测试共享进程级 writer/sink 状态，`cargo llvm-cov` 构建下并行互串（`stderr_lines_flag_the_error_level` 读到兄弟测试写入的 `"one"`/`"two"`），已为该模块 9 个测试加模块级串行锁。测试：`cargo fmt --check` 通过、`cargo clippy --all-targets` 0 警告（余根 Cargo.toml 既有 `unused workspace dependency`）、python-host 48 passed / 45 ignored、workspace 522 passed 0 failed。llvm-cov：binding.rs 99.10%（余 2 行为 `PyType_FromSpec` null 失败臂——健康 CPython 不可触发，按约定不加故障注入脚手架）、interpreter.rs 97.49%、output_writer.rs 96.63%、capi.rs 99.06%、crate 合计 97.36%。证据链接：`crates/python-host/src/binding.rs`（类型构造/注册/拒绝/repr/引擎构造）、`capi.rs`（堆类型 FFI 面 + `Py_tp_*`）、`interpreter.rs:218-240`（fini 接线）、`interpreter.rs:1330-1560`（demo 类型与 5 个 scenario）。
- [x] **BW-03-02-03 实现容器代理**。**验收** 修改代理可观察到原生状态同步。
  - 证据：`crates/python-host/src/sequence.rs` 新建（原生状态块 `SequenceState{values,writable}` + `SequenceBody` 实例体 + `spec()` 三处理器 + `unsafe wrap(state)` 建视图）与 `binding.rs` 扩展（`SequenceHandlers`/`TypeSpec::sequence()`/`TypeSpec::instance_size()`/`basicsize` 含实例体/注册表加 `sequences` 表/`reset()` 清理/序列协议槽与两个映射桥 `seq_subscript`/`seq_ass_subscript`/`seq_index_key` 负下标归一/`miss_bridge_item`/`miss_bridge_ass_item`/`raise_type_error`）与 `capi.rs` 扩展（`PyMappingMethods` 三指针布局断言 + `PyIndex_Check`/`PyNumber_AsSsize_t`/`PyExc_IndexError`/`Py_mp_*`/`Py_sq_*` 槽常量/`unicode_from_bytes`）。**原版结果**：容器是 `PySTLSequence`（`lib/pyscript/stl_to_py.cpp:163-227`），持一个 `HOLDER&`（原生 `STLSequence` 的引用，`holder_`），脚本下标经 `pySeq_item`/`pySeq_ass_item`（stl_to_py.cpp:163/189）落到 `holder_[i]` 或 `holder_.erase/insertRange/insert` + `holder_.commit()`（stl_to_py.cpp:214-226），`commit()` 把暂存变更写回原生容器——`del seq[i]` 与替换同走 `pySeq_ass_item`（`pItem==NULL` 即删除）；越界 `PyErr_SetString(PyExc_IndexError,"PySTLSequence index out of range"/"... assignment index out of range")`（stl_to_py.cpp:171/201），只读 `PyErr_Format(PyExc_TypeError,"Cannot assign an item in a read-only PySTLSequence")`（stl_to_py.cpp:193）；下标归一在 `seqIndexKey`（`pyobject_plus.cpp:203-240`），非整数 `TypeError`（`pyobject_plus.cpp:216`），映射协议经 `seqMappingMethods`（`pyobject_plus.cpp:294`）桥回 `sq_item`/`sq_ass_item`。**新版结果**：`sequence::SequenceState` 即原生 `Vec<i64>`+writable（无独立暂存层，视图直接读写，故「脚本写→原生可见、原生写→脚本可见」是同一块内存的必然结果）；`spec()` 填 `sq_length`/`sq_item`/`sq_ass_item` 三处理器并挂 `Py_mp_subscript`/`Py_mp_ass_subscript` 两个桥（`create_type` 仅在声明了 sequence 时填映射槽，无序列的类型无映射槽）；`seq_index_key` 端口 `seqIndexKey`（`PyIndex_Check`→`PyNumber_AsSsize_t(key,PyExc_IndexError)`，`index==-1 && PyErr_Occurred` 才判失败、`index<0` 加 `length`）；`unsafe wrap(state)` 经 `create_type(&spec())`+`binding::instantiate` 建视图并写入实例体的状态指针；`SequenceState` 的生命周期由宿主持有，视图不拥有它。**验收**：`scenario_a_sequence_proxy_round_trips_native_state`（原生 `[10,20,30]`，`len(seq)`==3、`seq[1]`==20、`seq[1]=25` 后原生 `[10,25,30]`、原生 `values_mut()[0]=5` 后脚本读 `seq[0]`==5——双向同步同验）；`scenario_two_sequence_views_share_native_state`（一状态两视图，左写右读同值，`is` False）；`scenario_a_sequence_proxy_normalises_negative_indices`（`seq[-1]`==末位，`del seq[-1]` 删末位）；`scenario_a_sequence_proxy_bounds_are_checked`（读/写越界均抛 `IndexError: boundless.Sequence index out of range`/`... assignment index out of range`）；`scenario_a_read_only_sequence_refuses_assignment`（只读态 `TypeError: Cannot assign an item in a read-only boundless.Sequence`）；`scenario_a_non_integer_subscript_refuses`（`seq['one']` 抛 `TypeError: boundless.Sequence indices must be integers, not str`，且 `'text'[0]=1` 的 CPython 原生 `TypeError: 'str' object does not support item assignment` 与端口 miss 桥文案一致——独立校对）。**TDD 抓出的真实缺陷**：① `PyUnicode_FromString(rendered.as_ptr())` 把无终止符的 Rust `String` 直接交 C，越界读相邻堆字节（`str` 后面跟了 `b`/`strobe_script.py` 等垃圾字符）——改 `capi::unicode_from_bytes` 走 `PyUnicode_FromStringAndSize`（带长度）；② `seq_index_key` 初版把 `-1` 同时当哨兵与「转换失败」，判反导致 `seq[-1]` 被当越界（`-1 && 无错误` 才返回，`-1 && 有错误` 反而继续）——改为「仅 `PyErr_Occurred` 非空时判失败」；③ 桥体在解释器未起来时直接调 `PyErr_Occurred`/`Py_TYPE` 会读空/崩（`PyErr_Occurred` 需线程态）——`seq_subscript`/`seq_ass_subscript`/miss 桥/`raise_type_error` 全部先查 `Py_IsInitialized` 早退；④ edition-2024 `let Some(x) = unsafe {…} else` 需括号。**delta 如实登记**：① 端口只填 `sq_length`/`sq_item`/`sq_ass_item` 三处理器 + 两映射桥——`PySTLSequence` 另有的 `sq_concat`/`sq_repeat`/`sq_contains`/inplace 等未填（`PyType_Spec` 可声明，当前无消费方，留待后续按需扩展）；② 无切片（`sq_slice`/`sq_ass_slice` 3.13 已移除，stl_to_py.cpp:31 注释亦说明）；③ 状态指针存实例体（`SequenceBody.state`）——原版嵌 `HOLDER&` 到 `PyObjectPlus` 实例，端口因 `PyType_FromSpec` 的 `basicsize` 须显式含实例体，故 `instance_size()` 显式声明（`basicsize = sizeof(PyObject) + instance_size`）；④ 映射桥用 `pub` 暴露（`seq_subscript`/`seq_ass_subscript`）——测试与宿主可直接派发；⑤ `PyType_GetName` 对静态类型（如 `str`）回 `tp_name`，对 `boundless.Sequence` 走注册表（`qualified_name` 优先查注册表）；⑥ `PyUnicode_FromString` 的 Rust 字符串调用一律改 `unicode_from_bytes`。**平台差异**：本项无平台差。测试：python-host 59 passed / 51 ignored、0 警告（fmt/clippy 0 警告）；workspace 533 passed 0 failed。llvm-cov：binding.rs 95.90%（余 17 行＝解释器启动后的桥体/注册表查找/槽装配，由 6 个 scenario 子进程验证）、sequence.rs 94.51%（余 9 行＝解释器启动后的 `PyLong_FromLong`/`PyLong_AsLong` 写回臂）、capi.rs 99.11%、crate 合计 96.93%。（commit `0d355bb`）。
- [x] **BW-03-02-04 实现引用保活与循环 GC**。**验收** 对象释放只发生一次。
  - 证据：`crates/python-host/src/keepalive.rs` 新建（原生保活注册表：`unsafe pin(id, &PyRef)`/`unpin(id)->bool`/`is_pinned`/`pinned_count`/`take_out(id)->Option<PyRef>`/`reset()`/`unsafe zero_reference_count`）+ `interpreter.rs` fini 接线（`keepalive::reset()` 紧随 `binding::reset()`，interpreter.rs:234-239）与 5 个新 scenario + `lib.rs` 导出。**原版结果**：保活是原版的普适约定而非单点机制——每个原生侧持有脚本对象的注册表各持一份强引用：`PySTLSequence` 构造 `Py_XINCREF(pOwner_)`、析构 `Py_XDECREF`（`lib/pyscript/stl_to_py.cpp:57-75`）；`Script::gTimers` 的 `TimerRecord{function,arguments}` 由 `addTimer` 自持、`clearTimers` 释放（`py_callback.cpp:62-88/272/323-324`）；`ScriptEventList::Element::pFunc_` 每事件一份（`script_events.hpp:47`）；`PyAccessorWatcher::getFunction_/setFunction_`（`pywatcher.cpp:826-843`）；`Pickler::s_pPickleMethod/s_pUnpickleMethod`（`pickler.cpp:264-270`）；`Script::s_pOurInitTimeModules`（`script.cpp:602/693`）。全部遵循 `Script::setData`（`script.cpp:1967-1981`）的「取新引用、释放旧引用」替换语义。原生侧**从不在 Python 仍持名时释放**：`_tp_dealloc`→`pyDel()`→`delete`（`pyobject_plus.cpp:64-71`），而 `~PyObjectPlus()` 只断言 `MF_ASSERT_DEV(ob_refcnt == 0)`（`pyobject_plus.cpp:65-75`）不做 DECREF/清弱引用，析构是 `protected`——多释放会被那条断言当场抓住。计数自检面为 `PyObjectPlus::refCount()`（`pyobject_plus.hpp:1094-1099`）与 cstdmf `hasZeroReferenceCount` 直读 `ob_refcnt`（`pyobject_pointer.hpp:40`）。GC 策略为「平时关闭、关服排空」：`gc.disable()`（`script.cpp:586-587`、`633-653`，`runInitTimeJobs` 之后），关服 `Script::fini`（`script.cpp:659-740`）在 `__main__` 抹白（3.13 内联复刻已删的 `_PyModule_Clear`，:707-734）之后跑 `while (PyGC_Collect() > 0);` 排空引用环，再 `Py_Finalize`。**新版结果**：`keepalive::pin` 即端口版 `Py_XINCREF(pOwner_)`——克隆后 `std::mem::forget` 自持（与 `wrapper::wrap` 同一形状），地址存 `usize` 保持 `Send`；重复 `pin` 同 id 按 `setData` 语义替换：`insert` 返回旧条目即当场 DECREF 恰一次，不泄漏、不重复；`unpin` 命中释放一次并返回 `true`、未命中返回 `false`（宿主双重关服因此无害）；`take_out` 把条目的那份引用过户给调用方（`PyRef::borrowed` 不增不减），调用方 drop 才是那次释放；`reset()` 在 `fini` 的 `binding::reset()` 之后排空全部条目——`Pickler::finalise()` 在原版 `Script::fini` 内的位置（`script.cpp:686`）；`zero_reference_count` 直读 `ob_refcnt` 即端口版 `MF_ASSERT_DEV(ob_refcnt == 0)`；`pin` 在 `Py_IsInitialized()==0` 时报 `PinError::NoInterpreter`（`Py_XINCREF` 是 C API 调用），空句柄报 `NoObject`，`release_reference` 在 `is_finalized()` 后跳过（对齐 `PyRef::drop` 不向已拆分配器减引用）。**验收**：`scenario_a_keep_alive_entry_holds_one_reference`（`sys.getrefcount` 基线 → `pin` 恰 +1 → `unpin` 回基线——一条条目恰一份强引用，不多不少）；`scenario_repinning_adopts_and_replaces_once`（首条目 `ob_refcnt`==3＝`__main__`+句柄+注册表；同 id 换绑后 `pinned_count()==1`、首对象计数回落至 2——旧引用恰释放一次，双重释放会把它在持名下释放掉、漏一次则停在 3）；`scenario_unpinning_twice_releases_once`（首 `unpin` 计数 -1、次 `unpin` 返 `false` 计数不动）；`scenario_a_pinned_callback_survives_the_script_dropping_it`（绑定方法作定时器形状回调，`pin` 后 `del h`/`del __callback` + `gc.collect()` 释放不了任何东西——`ob_refcnt` 仍 2＝句柄+条目；`take_out` 过户后 `drop` 句柄得 1，经条目引用 `PyObject_CallNoArgs` 仍可调得通，`zero_reference_count` 此刻为 false 而由调用方最终 drop 走到 0）；`scenario_a_script_side_cycle_is_freed_exactly_once`（`gc.isenabled()`==False 钉死「平时关闭」；`a<->b` 两对象环，持名时显式 pass 得 0；`del a;del b` 后 `gc.collect()` 恰返回 2；再 pass 得 0——环恰释放一次，不多放不少放，进程干净退出即无双重释放）。**TDD 抓出的真实缺陷**：① 环 scenario 初版断言持名时 `gc.collect()==0`，实测 21（init 遗留的不可达对象，`gc.disable` 只关自动回收、显式 pass 仍收）——改为先 pass 预热再断言 0，避免把无关数量当契约；② 回调 scenario 用 `probe` 跑不赋 `__probe` 的类定义源，`PyDict_GetItemString` 返回 null 送进 `PyObject_Str`——改 `run_simple_string`；③ `gc.collect()` 单行跑时 `gc` 未导入致 `NameError`，脚本跑失败——补 `import gc;`。**delta 如实登记**：① 保活注册表以 `u64` 原生 id 为键，原版各注册表以对象/事件为键——端口把「原生身份」统一成 id，`pin/unpin` 由宿主（`CellApp` 实体表、定时器表、watcher）按 id 驱动；② `take_out` 供宿主「过户而非释放」，原版为逐注册表手写的 `Py_XINCREF`+`forget`；③ 弱引用面未移植（`PyObjectPlusWithWeakReference`/`PyWeakRefListManager`/`WeakPyPtr` 哨兵 `(PyObject*)1`，`pyobject_plus.hpp:1188-1250`、`script.hpp:1363-1572`）——本车道待后续项；④ `zero_reference_count` 仅读不断言（端口无 `MF_ASSERT_DEV` 宏，由测试断言承担）；⑤ `Pickler::finalise` 等具体注册表随各模块移植到位时接入本注册表（本项只交付机制）。**平台差异**：本项无平台差。测试：`cargo fmt --check` 通过、`cargo clippy --all-targets` 0 警告（余根 Cargo.toml 既有 `unused workspace dependency`）、python-host 64 passed / 56 ignored、workspace 538 passed 0 failed。llvm-cov：keepalive.rs 98.84%（余 1 行＝`release_reference` 的 finalize 后防御性跳过——`reset()` 先于 `Py_Finalize` 排空故不可达）、interpreter.rs 97.87%、binding.rs 95.90%、sequence.rs 94.51%、capi.rs 99.11%、crate 合计 97.09%。（commit `8ec91d2`）。
- [x] **BW-03-02-05 约束 GIL 与跨线程回调**。**验收** 线程退出及异常后无悬挂回调。
  - 证据：`crates/python-host/src/gil.rs` 新建（GIL 括号 + 跨线程回调队列）+ `capi.rs` FFI 面调整（撤未用的 `PyGILState_Ensure`/`Release`，换 `PyEval_RestoreThread`/`PyEval_SaveThread`/`PyThreadState_Swap`/`PyRun_SimpleString`，另加 `PyGILState_Check`）+ `interpreter.rs` init/fini 接线与 4 个新 scenario + `lib.rs` 导出。**原版结果**：锁就是两个全局，别无其他——`s_pMainThreadState`（`lib/pyscript/script.cpp:88`）在 init 里 `PyThreadState_Get()` 捕获（`:603-604`，且 `s_defaultContext = s_pMainThreadState`），`s_defaultContext` 是 `THREADLOCAL( PyThreadState * )`（`:89`，每线程一份）。`Script::acquireLock`（`:932-942`）＝`if (s_defaultContext == NULL) return; PyEval_RestoreThread( s_defaultContext )`；`Script::releaseLock`（`:949-958`）＝同样 NULL 早退 + `PyEval_SaveThread()` 后校验返回值 == `s_defaultContext`，不符即 `MF_EXIT("releaseLock: default context is incorrect")`（`:955-956`）。**原版全树无 `PyGILState_*`、无 `Py_BEGIN/END_ALLOW_THREADS`**；GIL 只在主线程阻塞时放下：`setMainThreadIdleFunctions( &releaseLock, &acquireLock )`（`:612-613`）由 `event_poller.cpp:482,500` 的 `startMainThreadIdling`/`endMainThreadIdling` 包住阻塞 `select`（`:496`），`Script::fini` 在 `:685` 复位为 `NoOp`。脚本调用**不带**锁：`Script::call`（`lib/pyscript/script.ipp:22-28`）直接转 `Script::ask`，靠调用方已持 GIL。跨线程面是 `TaskManager::addMainThreadTask`（`lib/cstdmf/bgtask_manager.cpp:709-715`，"@note any thread can access this."，`fgTaskListMutex_` 守护）由 `TaskManager::tick`（`:744-749`，"should only be called by the owning thread"）在主线程排空，其实现是**锁内 swap 出整表、锁外逐个执行**——这即是「延时 ≤0 的新定时器也必须至少等一帧」（`py_callback.cpp:161-162`）的实现根据。`Script::fini` **不碰**该队列，清空在显式 `Script::clearTimers()`（`py_callback.cpp:81-91`，"this has to be called at a different time than fini, that's why it's a separate method"）。**关键佐证**：`script.cpp:939-941` 明写 `PyThreadState_Get` 在 thread state 为 NULL 时是致命错误，"there doesn't appear to be a good way to assert this here"。**新版结果**：`gil::init_thread()` 端口 `:603-604` 的捕获（`Py_IsInitialized()==0` → `LockError::NoInterpreter`；已有 context → `AlreadyInstalled`）；`acquire_lock()`/`release_lock()` 逐句端口，`release_lock` 校验保存态 == 安装态，不符返 `ReleaseError::WrongContext`（端口报告而非 `MF_EXIT`）；`fini_thread()` 归还 GIL 并清 context（对齐 `:685` 的 NoOp 复位）；`post`/`process`/`pending`/`clear` 端口 `addMainThreadTask`/`tick`/`clearTimers`，`process` 先 `mem::take` 出快照再跑——锁已释放、回调内 `post` 留待下一轮（与「至少等一帧」同形），且逐个 `catch_unwind`，一个回调 panic 不阻断其后的回调、不中毒队列锁；`Interpreter::init` 在 `Py_IsInitialized()` 检查后接 `gil::init_thread()`，`fini` 在 `run_fini_time_jobs()` 之后接 `gil::clear()`。**验收**：`scenario_the_gil_context_is_installed_at_init`（init 后 `has_context()` 与 `PyThreadState_Get()` 非空；`release_lock()` 返 `Ok`——保存态 == 安装态——且 `PyGILState_Check()==0`；`acquire_lock()` 后 `PyGILState_Check()!=0` 且当前态 == init 捕获值；随后脚本仍读到自己的全局 `seen`）；`scenario_a_callback_posted_from_a_dying_thread_runs_on_the_script_thread`（**验收项本体**：子线程 `post` 后退出、`join` 成功，`pending()==1`，主线程 `process()` 返 1，脚本 `counter[0]`==1，`pending()` 归 0——线程已退，回调仍在且由脚本线程执行）；`scenario_an_exception_in_a_callback_does_not_leave_it_queued`（首个回调抛 `ValueError`，其后的回调照常跑完：`process()` 返 2、`pending()`==0、`counter[0]`==1）；`scenario_finishing_the_script_system_leaves_no_callback_behind`（`post` 后直接 `fini()`：`pending()`==0、回调未被执行、`is_finalized()`）。**TDD 抓出的真实缺陷**：① `PyThreadState_Get()` 在 GIL 放下后调用是**致命错误**（`Fatal Python error: PyThreadState_Get: … but the GIL is released (the current Python thread state is NULL)`，本机 3.14 实测 SIGABRT）——初版 scenario 用它断言「锁已归还」，改用不需持 GIL 的 `PyGILState_Check`；这正是原版 `:939-941` 记录的同一死胡同（原版因此干脆不加断言，端口用 `PyGILState_Check` 补上这个可观察面）；② 初版断言 `release_lock` 后 `has_context()==false`，语义搞错——`releaseLock` 只归还 GIL、**不清** context（那是 `finiThread` 的职责）；断言 panic 后又带着 GIL 未取回的状态 unwind，`fini` 的快照 DECREF 落进 `_Py_Dealloc` SIGSEGV（`interpreter.rs:268`）；③ `clear()` 初版只返回长度而**未清空**；④ `process()` 初版对同一把锁取两次且 `mem::take` 之后再读 `len()` 编译不过，合并为一次取锁；⑤ `thread_local!` 初始值须 `const` 化才消掉 clippy 警告；⑥ 本 crate 无 `log` 依赖，回调中止的日志改走 `output_writer::handle_write`（同 crate 的脚本日志面，标 stderr）。**delta 如实登记**：① `init_thread` 端口的是 `:603-604` 的捕获，**不是** `Script::initThread`（`script.cpp:829-882`）——后者是线程 bootstrap（`PyEval_RestoreThread` → `Py_NewInterpreter()`/`PyThreadState_New` → `PyEval_SaveThread` → `s_defaultContext = newTState` → `acquireLock()`，`:880-881`），在参照树中无 `Script` 之外的调用方，端口无对应物（单线程持有解释器，子解释器切换属 server reload 路径）；② `initThread` 的 `MF_EXIT("trying to initialise scripting when already initialised")`（`:831-834`）在端口成为 `AlreadyInstalled`（报告而非退出）；③ `WrongContext` 同样由 `MF_EXIT` 变为可返回的 `ReleaseError`；④ `process` 的 `catch_unwind` 是端口侧加固——原版 C++ 处理器不 unwind（`MF_EXIT` 直接终止），可观察顺序不变；⑤ 队列以 `FnOnce() + Send` 闭包为载体、**不携带 `PyRef`**（`PyRef` 是 `!Send`；原版队列持 `PyObject*` 并 `Py_XINCREF`），故回调在 GIL 线程内自行取对象——与「Python 对象不得跨线程共享」一致；⑥ `clear()` 放在 `run_fini_time_jobs()` **之后**，而原版 `clearTimers` 在 `Script::fini` **之前**（app 层调）——若作业 `post` 会遗留悬挂回调，故这是**对原版缺陷的有意偏离**，使验收在关服路径上成立。**平台差异如实登记**：本机 CPython 3.14.9（参照树 pin 3.13.16）；`PyThreadState_Get` 在 GIL 未持时致命错误为本机 3.14 实测行为，据此 scenario 改以 `PyGILState_Check` 观察锁状态；`capi.rs` 新增该 FFI，四个符号（`PyEval_RestoreThread`/`PyEval_SaveThread`/`PyThreadState_Swap`/`PyGILState_Check`）均经 `nm -D libpython3.14.so.1.0` 核验存在；`tick` 的「锁内 swap、锁外执行」逐句对齐 `bgtask_manager.cpp:744-749`，排空语义无需平台适配。测试：`cargo fmt --check` 通过、`cargo clippy --all-targets` 0 警告（余根 Cargo.toml 既有 `unused workspace dependency`）、python-host 73 passed / 60 ignored、workspace 547 passed 0 failed。llvm-cov：gil.rs 96.22%（余 7 行＝`init_thread` 的 `AlreadyInstalled` 与 state-null 防御臂、`fini_thread` 实路径、`release_lock` 的 `WrongContext` 臂、一处 if 收尾大括号——均需解释器在线或状态错配才可达，由 4 个 scenario 子进程覆盖但不进父进程计数）、capi.rs 99.11%、crate 合计 96.92%。（commit `55b9fbe`）。

#### BW-03-03 公共脚本模块

移植脚本数学与 Provider、时间和定时器、资源访问、网络/URL 请求（`lib/urlrequest/`）及共享模块。按进程盘点模块名、参数、返回值和异常，通过原样调用的 Python 测试。

- [x] **BW-03-03-01 维护公共 Python 导出清单**。**验收** 模块、签名与进程范围可核对。
  - 证据：`crates/python-host/src/manifest.rs` 新建（24 个模块行 × 5 列签名/返回/异常/引用/scope 的导出清单 + `check()` 核对）+ `capi.rs` 补 `PyObject_HasAttrString` FFI + `interpreter.rs` 活解释器 scenario + `lib.rs` 注册。**原版结果**：模块对象的创建几乎全部走 init-time job 内 `PyImport_AddModule(name)` + `PyModule_AddObject/AddFunctions`——`PyModuleMethodLink::init`/`PyModuleAttrLink::init`/`PyModuleResultLink::init`（`lib/pyscript/script.cpp:1409/1453/1496`）与 `PyFactoryMethodLink::init`（`lib/pyscript/py_factory_method_link.cpp:40-41`，顺带把 `tp_name` 改写成 `Math.<名>`）；`ScriptModule::create` 全树不存在，`ScriptModule::getOrCreate` 只钉 `BigWorld`（`script.cpp:589`）与 `__main__`（`:1302`）；`PyImport_AppendInittab` 全树唯一调用点是 WorldEditor 工具构建（`tools/worldeditor/scripting/world_editor_script.cpp:2165`）。**进程范围由源文件级门而非库边界决定**：pyscript 库 client/server 共链（`lib/pyscript/CMakeLists.txt:41-55` 只按 `MF_SERVER` 拆 `py_callback.cpp`/`py_traceback.cpp`/`script_events.cpp`），`MF_SERVER`/`EDITOR_ENABLED` 分别由 `build/cmake/BWConfiguration_server.cmake:1`、`BWConfiguration_tools.cmake:1` 定义。盘点出**两处反直觉的 per-item scope**：`BigWorld.callback`/`cancelCallback` 仅客户端（`#ifndef MF_SERVER` 包裹，`py_callback.cpp:9`…`:337`，"Not available on server yet, pending refactoring."），而 `addEventListener`/`removeEventListener` 仅服务器（`script_events.cpp` 在 CMakeLists 的 `ELSE()` 分支）——同一 `BigWorld` 模块内逐函数分域。逐面盘点：`component` 属性（`script.cpp:598-600`；server 侧再补 `serverMode`，`lib/server/script_app.cpp:219,225`）；`fetchURL`（`"sO|OfsO"` 关键字 url/callback/headers/timeout/method/postdata，`common/py_urlrequest.cpp:318`）与 `URLResponse` 类（`:26-59`）双端；watcher 六函数双端（`pywatcher.cpp:405/466/539/1551/1635/1658`，无 watcher 构建另有 no-op 形 `:2036-2062`）；服务器计时器 `addTimer`/`delTimer`（`lib/server/app_script_timers.cpp:193/235`）、`addFrequentTask`（`script_frequent_tasks.cpp:142`）、`saveAllocationsToFile`（`script_app.cpp:354`）；日志五函数（`py_logging.cpp:91-106`）与 `defaultLoggers`（`py_debug_message_file_logger.cpp:460`）；py_network 六函数仅服务器（`common/py_network.cpp:301/325/361/385/414/490`）；py_chunk 两函数双端（`:106/:134`）；`getHeightAtPos` 仅服务器（`py_terrain.cpp:74`）、`getMaterialKinds` 双端（`py_physics2.cpp:63`）。`Math` 模块 17 面（`getRegister` `script_math.cpp:5051` + 16 类，经 `TempPyVectorInit` `:3060` 附着）；`ResMgr` 14 面（`res_mgr_script.cpp:49-355`，其中 `remove`/`rename`/`copy` 在 `#ifdef EDITOR_ENABLED` 内 `:166-260` 故仅工具；`DataSection` `py_data_section.cpp:360`；`ResourceTable` `resource_table.cpp:666`）；`BWConfig` 六函数仅服务器（`common/py_server_lib.cpp:45-137`，@components base,cell,db,bots）。其余 12 个工具/渲染栈模块（GUI/Pixie/_FMOD/_Scaleform/_PostProcessing/_AssetProcessor/BWReplay/WorldEditor/ModelEditor/ParticleEditor/Locator/Functor/View/Tool/OpenAutomateWrapper）与 5 个项目供给模块（Keys/Personality/BWTwoWay/FixedDictWrappers/UserTypes，引擎只 import 不创建，`client/script_bigworld.cpp:1053`、`personality.cpp:65`、`method_description.cpp:243`、entitydef 单测 fixtures）登记为名字+scope 行。**任务书勘误**：任务句写 `lib/urlrequest/`，参照树中该目录不存在，URL 请求实际源在 `common/py_urlrequest.cpp`；BW-03-03-04 任务句写 `lib/crell/`/`lib/crecell/` 同样不存在，资源访问实际源为 `lib/pyscript/res_mgr_script.cpp`/`py_data_section.cpp`/`resource_table.cpp` + `lib/resmgr/`——均已写进清单 note。**新版结果**：`Scope{Both,Client,Server,Tool,Supplied}`、`Kind{Module,Function,Attribute,Class}`、`Status{Ported,Covered,Pending}` 三枚举 + `Item{name,kind,scope,status,signature,returns,error,reference}` 逐行面 + `Entry{module,scope,status,reference,note,items}` 逐模块行；per-item scope 独立于模块 scope（`callback`=Client 挂在 Both 的 `BigWorld` 下）；`check()` 把盘点走查抽成 `audit(entries, imports, carries)` 闭包缝——活解释器上 `PyImport_ImportModule`+`PyObject_HasAttrString` 只对 `Status::Ported` 行断言，失败即 `Mismatch{ModuleMissing,ItemMissing}`（失败 import 先 `clear_error`，清单是巡检不是脚本运行）；Pending 行不声明、交付一项翻一行 Ported 即被 `check()` 当场核验——这就是「维护」机制；`Covered` 标注已由前项交付但端口无自有模块对象的名字（`__main__` 抹白、`bw_site` 导入）。**验收**：单元测试 `every_row_cites_its_source_and_signs_its_functions`（每行必有 reference+returns、Function 行必有 signature、Ported 行的模块必已 Ported）、`the_counter_intuitive_scopes_are_pinned`（callback=Client、addEventListener=Server、模块=Both——BW-03-03-03 的定时器模块依此行分域）、`supplied_modules_are_not_claimed_by_the_engine`、`only_the_delivered_surface_is_marked_ported`（Ported 面=BigWorld+component，不多勾）、`a_walk_reports_both_ways_a_table_can_lie`（audit 的 ModuleMissing/ItemMissing 两臂 + render 成句）、`the_row_constructor_sets_the_fields_in_order`（8 位置参防错位）；活解释器 `scenario_the_manifest_matches_the_interpreter`（`check()`==Ok；`BigWorld.component` 在；`callback` 不在——scope 记录与活模块一致；`Math` 模块整体缺席＝Pending 如实不虚勾）。**TDD 抓出的真实缺陷**：① 初版 `check()` 的失败分支不可测——提取 `audit` 闭包缝后单元测试走「说谎的表」覆盖全部失败臂；② `manifest::check()` 是 unsafe fn，scenario 内直接调用被 edition 2024 拒编译（E0133），补 unsafe 块；③ scenario 内 `b"BigWorld\0".as_ptr()` 手工拼 NUL 串触发 clippy 警告，改 `c"..."` 字面量；④ `audit(&[entry], …)` 传 `&Entry` 进 `&[Entry]` 不匹配（E0308），改 `std::slice::from_ref`；⑤ 闭包 `|_| false` 过 HRTB 检查报 "implementation of FnMut is not general enough"，显式标 `|_: &str| false` 消解。**delta 如实登记**：① 本项只交付清单与核对机制，**不创建任何模块对象**——端口尚无 `PyModuleDef`/`PyModule_AddObject` 面，创建随各模块项交付后逐行翻 Ported；② `Covered` 是端口侧词汇（原版无此分类），标注引擎对名字的行为已交付而模块对象非引擎所建；③ 工具/渲染栈 12 模块只登记名字与注册点（面不在本车道，note 记录注册 `path:line` 供后续车道起步）；④ Math 的文档注释提到 `Math.MatrixProvider`/`Vector4Provider`/`Vector4Basic`/colour 别名，实际**均未导出**（有 `PY_TYPEOBJECT` 无 `PY_FACTORY`）——清单按代码不按注释，note 记录该陷阱（含 `script.hpp:584-585` 的 TypeError 文本）；⑤ doc marker 与注册名不一致三处（`_Scaleform`/`_PostProcessing`/`_AssetProcessor`，注释写无下划线名、代码注册带下划线名），按代码登记；⑥ `py_data_section.cpp:815` 用 args tuple 当异常类型调 `PyErr_Format` 是原版缺陷，note 记录防移植。**平台差异如实登记**：本机 CPython 3.14.9（参照树 pin 3.13.16）；核对只依赖 `PyImport_ImportModule`/`PyObject_GetAttrString`/`PyObject_HasAttrString`，均 3.13+ 稳定 C API，无平台差；client-only/server-only 差异是**构建差异**（`MF_SERVER`/`EDITOR_ENABLED`）而非运行时差异，端口以 scope 枚举承载、宿主进程按角色选面。测试：`cargo fmt --check` 通过、`cargo clippy --all-targets` 0 警告（余根 Cargo.toml 既有 `unused workspace dependency`）、python-host 81 passed / 61 ignored、workspace 555 passed 0 failed。llvm-cov：manifest.rs 97.67%（余 6 行＝`check()` 活体的 import 失败/Err 收尾臂——需活解释器上清单出错才可达，audit 缝已单测覆盖同型臂）、capi.rs 99.11%、interpreter.rs 97.60%、crate 合计 97.20%。（commit `b50a875`）。
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

- [x] **BW-04-01-01 解析实体和别名文件**。**验收** 实体 ID、别名解析及缺项错误吻合。
  - 证据：crates/entitydef 差分测试对参考 process_defs dump（fantasydemo / client_integration 两棵 res 树）逐字节相等，149 单测通过，行覆盖 99.41%；详见 docs/review/bw04-01-01-entitydef-parse.md。commit `0023108`。
- [x] **BW-04-01-02 合并父类和接口**。**验收** 继承冲突与优先级吻合。
  - 证据：父类同名方法签名冲突失败、接口成员先于自身并入、Volatile 缺省继承父值、接口链自深向浅合并；差分 oracle 仍逐字节相等。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-01-02 节。commit `da3b2ff`（前置 clippy 修复 `ca51862`）。
- [x] **BW-04-01-03 处理 Service 定义**。**验收** 公开方法与原版清单一致。
  - 证据：自建 res 夹具（services.xml 两服务 + service_defs + 服务脚本 + 服务接口链）上跑参考 process_defs 生成 oracle `docs/review/reference/service_entitydef.json`（MD5 012FC1AB…），Rust `dump_map` 逐字节相等；服务公开方法清单 = `<Implements>` 接口方法在前 + `<Methods>`（vacuum/sweep/tally），`sweep` exposedIndex=0，cell/client 半边为空，服务自带 `<Properties>` 被忽略。同时修正 `EntityDistribution` 标签名对齐参考 `readBool("Client"/"Base"/"Cell")`。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-01-03 节。commit `683bb5d`。
- [x] **BW-04-01-04 分配属性和方法索引**。**验收** Base、Cell、Client 索引吻合。
  - 证据：新增自建夹具 `docs/review/fixtures/entity_index/res`（父/子实体 + server-only + 三边混合定长/变长方法），参考 process_defs 生成 oracle `docs/review/reference/entity_index_entitydef.json`（MD5 6FAD1D72…），Rust `dump_map` 首轮即逐字节相等；属性 index 按并入序、clientServerFullIndex 只在 client-server 半边重排、base/cell/client 三边 exposed 索引各自排序（子实体 repair=0、damage=1、shout=2，retire=-1）。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-01-04 节。commit `df12d8e`。
- [x] **BW-04-01-05 计算定义摘要**。**验收** 同输入得到与原版相同字节。
  - 证据：四份参考 oracle（fantasydemo / client_integration / service_def / entity_index）的 `constants.digest` 与 Rust `dump_map` 逐字节相等；新测 `the_digest_is_deterministic_and_matches_the_reference_oracles` 钉住同夹具两次解析同字节且等于参考常量（service 8A225AFC…、entity_index 3F009C3E…），既有测试钉住"只计入 client 类型、按加载序"规则。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-01-05 节。commit `43a4902`。

#### BW-04-02 数据类型和属性流

覆盖基本类型、ARRAY/TUPLE、FIXED_DICT、USER_TYPE、MAILBOX、嵌套属性和变化切片。比对 Python 类型转换、默认值、压包、广播和持久化字段。

- [x] **BW-04-02-01 实现标量与默认值转换**。**验收** 数值边界和异常对照通过。
  - 证据：`crates/entitydef::value` 落地 `PropertyValue`/`PropertyError` 与 `check_integer`/`value_from_section`/`default_value`/`add_to_stream`/`create_from_stream`；数值边界钉住 `Script_intOverflow` 四边界（INT32 收 0x7fffffff 与 -0x80000000、拒 0x80000000 与 -0x80000001），四条参考错误模板逐字节锚定（含结尾 `\n`），`pInitialValue` 三槽顺序与 `setDefaultValue` 语义各有一测；171 单测 + 4 差分通过，value.rs 行覆盖 99.09%。无字节级参考 oracle（process_defs 不遍历属性流），本项证据为源码级对齐，不虚勾。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-02-01 节。commit `c7c2e45`。
- [x] **BW-04-02-02 实现 ARRAY/TUPLE**。**验收** 长度、元素及嵌套字节一致。
  - 证据：`crates/entitydef::value` 新增 `PropertyValue::Sequence` 与 `Kind::Sequence` 的五个入口（`check_value`/`value_from_section`/`default_value`/`add_to_stream`/`create_from_stream`）+ `check_sequence_capacity`；定长序列**不写**长度前缀、变长写 packed int 计数，钉住 `TUPLE size 2 of INT8` 的 `[1,2]` 与 `ARRAY of INT8` 的 `[2,1,2]`；嵌套「变长 ARRAY of 定长 TUPLE」字节钉为 `[2,1,0,2,0,3,0,4,0]`，「ARRAY of STRING」钉为 `[2,1,'a',2,'b','c']`；六个参考错误模板逐字节锚定（含结尾 `\n`），参考在读路径误印 `fromSourceToStream` 的复制粘贴瑕疵逐字复刻；默认值按 `size_`（变长空、定长 N 个元素默认值），节读取按 `countChildren()` 且尾部缺失取元素默认值；报错中的类型名是元素自己的 `typeName()`（`INT8`）而非 `ARRAY size 2 of (INT8)`；181 单测 + 4 差分通过，value.rs 行覆盖 99.06%、crate 99.46%，clippy 0 findings。仍无字节级参考 oracle，证据为源码级对齐，不虚勾。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-02-02 节。commit `e4a2997`。
- [x] **BW-04-02-03 实现 FIXED_DICT**。**验收** 字段次序和缺省行为一致。
  - 证据：`crates/entitydef::value` 新增 `PropertyValue::FixedDict` 与 `Kind::FixedDict` 的五个入口 + `PropertyError::{NullSection, MissingNoneIndicator, FieldDefaultFailed}`；钉住无 `<AllowNone>` 时**无**指示字节、字段按类型自身顺序直排（`UINT8 7` + `STRING "x"` = `[7,1,'x']`），`Enter/Leave/End` 三个 StreamElement 均不写字节；`<AllowNone>` 时钉住 `uint8(isNone ? 0 : 1)`（None 写 `[0]` 即止、有值写 `[1]` 再接字段）；缺省行为钉住 `getDefaultValue` 双分支（allowNone→None，否则逐字段默认值按字段序）、`countChildren()==0` 即 None、缺失字段先在本类型 `<Default>` 按字段名找子节、找不到才用字段类型默认值、该子节构建失败报 `ClassDataType::Field::createDefaultValue: Failed to create %s from default section`；三条参考错误模板逐字节锚定（含结尾 `\n`，`Missing None/values indicator` 一条钉住参考源码相邻字面量拼接后 `fromStreamToSink` 后无冒号的形态）；190 单测 + 4 差分通过，value.rs 行覆盖 99.27%、crate 99.47%，clippy 0 findings。仍无字节级参考 oracle，证据为源码级对齐，不虚勾。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-02-03 节。commit `959ec7f`。
- [x] **BW-04-02-04 实现 USER_TYPE 与 MAILBOX**。**验收** 转换及序列化夹具一致。
  - 证据：`crates/entitydef::value` 新增 `PropertyValue::MailBox { id, ip, port, salt }` 与 `PropertyError::{MissingConverter { site, module, instance }, UninitialisedMailBox { id }}`，补齐 `Kind::UserType`/`Kind::MailBox` 五入口；USER_TYPE 无 Python 宿主的五个上报点逐一对齐——`isSameType` 静默假、`addObjectToStream` 先写空字符串（字节 `[0]`）再报错、`createObjectFromStream` 先消费长度前缀字符串再报错（截断时得 `StreamTruncated`，转换器未被询问）、`createFromSection` 报站点串 `UserDataType::<site>: Do not have demo.Thing`；MAILBOX 钉住 12 字节小端 `{id, ip, port, salt}` 线格式并往返一致、ip==0 报 `MailBoxDataType::isSameType: Mailbox for entity %d has not been fully initialised yet (IP address is zero)`、`getDefaultValue` 全零 ref、`createFromSection` 读 `id`/`ip`/`etc` 具名子节且 `etc = port | (salt << 16)`、子节缺失读 0；`data_type.rs` 新增 MAILBOX 拒绝 `<Default>` 子节（报 `MailBoxDataType::setDefaultValue: Default value for mailbox not supported`）但类型照常构建且不落 `default_section`；197 单测 + 4 差分通过，value.rs 行覆盖 99.08%，clippy 0 findings。关键登记：参考 `MailBoxDataType::to(BinaryOStream&)` 写 `stream << mbr` 而全树无 `operator<<(BinaryOStream&, EntityMailBoxRef&)`，流字节为按 `streamSize()==12` 推导而非验证；USER_TYPE 默认值参考报错后写 Python None，本模型落 Int(0) 兜底，差异已登记。仍无字节级参考 oracle，证据为源码级对齐，不虚勾。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-02-04 节。commit `3828957`。
- [x] **BW-04-02-05 编码嵌套属性增量**。**验收** 修改一个 FIXED_DICT 子字段时，更新切片的路径和负载与原版逐字节一致。
  - 证据：新模块 `crates/entitydef::property_change`（`PropertyChange` 基类 + `SinglePropertyChange`/`SlicePropertyChange`）复刻 `property_change.cpp` 内流编码：flags 字节（SLICE=1、NESTED=2）、`uint8` 路径长 + 逐跳 `int32`（叶→根）、extra bits（单值 int32 leafIndex / 切片 uint32 start+end，**仅嵌套路径携带**）、负载走叶子类型自身 `addToStream`；验收钉 `entity.myDict.ammo = 7` 的 `[2, 0, 0,0,0,0, 7]`——实体自身属性索引 6 进 root_index（信封，`ghostedDataUpdate` 先写 id+propertyIndex 再进变更体）**不进路径**（参考 `IntermediatePropertyOwner::getTopLevelOwner`），负载是字段类型自己的 `[7]` 而非字典形态；深层 `arr[4].dict.ammo` 钉 `[2, 1, 4,0,0,0, 0,0,0,0, 7]`；嵌套切片钉 `[3, 1, 1,0,0,0, 2,0,0,0, 5,0,0,0, 9, 8]`；平路单值 `[0, 7]`、平路切片 `[1, 5]` 无 start/end（参考读侧 `MF_ASSERT( !isSlice )` 佐证）；STRING 子字段走 packed 形态、rootIndex 虚派发（平路单值=leafIndex/切片=0/嵌套=信封索引）各有一钉；`isNestedChange(bool)` 忽略实参恒置真、路径长>255 静默截 uint8 两处参考形态逐字保留；`data_type.rs` 增 `field_type`（=`getFieldDataType`）与 `element_type`（=`getElemType`）；206 单测 + 4 差分通过，clippy 0 findings，全 crate 行覆盖 ≥98%（property_change.rs 100%）。外部位压缩形态（BitWriter/bitsRequired）未移植已登记；仍无字节级参考 oracle，证据为源码级对齐，不虚勾。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-02-05 节。commit `7213567`。
- [x] **BW-04-02-06 应用嵌套属性增量**。**验收** 接收原版更新切片后，只有目标字段改变，Python 通知携带正确旧值。
  - 证据：`property_change.rs` 新增应用族 `apply_internal_stream(root_type, root_value, stream)`——flags 分派后平路整属性 `create_from_stream` 替换（空路径）、嵌套走 `uint8` 路径长 + 逐跳 `int32` 下降（`getChildPropertyOwner` 同构：Sequence/FixedDict 可作中间所有者，其余 `OwnerIsNull`）、`getPyIndex` 映射为 `PathElement::{Field(字段名),Index,Slice}`；验收钉 FIXED_DICT 子字段变更 `[2, 0,0,0,0, 0,0,0,0, 7]` 应用后**只有 ammo 变 7、label 原样、old_value=旧子值 `Uint(5)`**（参考 `setOwnedProperty` 返回旧子值而非整属性）、通知路径 `[Field("ammo")]`；切片应用钉 `[2,5]` 替换后数组重排 + 通知路径元素 `Slice(1, 2)`（end 按**有符号**尺寸增量重定基，缩容为负）；固定尺寸校验 `end-start != new.len()` → `WrongArraySize`（参考原话无尾换行）、尺寸 0 豁免；坏路径各有钉：标量根嵌套 → `OwnerIsNull{hop:1}` 值原样、字典坏跳 → `BadChildIndex`（`"Bad index %d. size = %zd\n"` 原话）、叶越界 → `LeafIndexOutOfRange`、非数组切片 → `OldValueIsNull`（`"Old value is NULL\n"`）、截断 → `Payload(StreamTruncated)`、BLOB 越界 → `UnableToCreateSlice`、平路切片 flag → `FlatSliceOnInternalStream`（对齐参考 `MF_ASSERT( !isSlice )`）；FixedDict(None) 视 0 子项；222 单测 + 4 差分通过，clippy 0 findings，触碰文件行覆盖 ≥98%（property_change.rs 99.44%）。登记差异：参考双打印收敛为单错误、叶越界参考是 UB 而 Rust 显式报错、`endIndex<startIndex` 参考只打印不中断未建形态；**路径序更正随本 commit 落账**：`ChangePath` 头注释"叶→根"有误，`getTopLevelOwner` 递归返回后才 `addToPath`，流内实为下降序（BW-04-02-05 文档表述已更正）；仍无字节级参考 oracle，证据为源码级对齐，不虚勾。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-02-06 节。commit `183708a`。
- [x] **BW-04-02-07 提取持久化投影**。**验收** 数据库属性流与原版字节一致。
  - 证据：`data_type.rs` 增 `DataType::num_persistent_fields`（=`FixedDictDataType::getNumPersistentFields`）与 `DataType::field_index_for_persistent_field_index`（=`getFieldIndexForPersistentFieldIndex` 线性投影映射）；`value.rs` 内部流函数增 `persistent_only` 形参按参考透传（Sequence/FixedDict 递归臂透传、标量臂忽略），FixedDict 写臂 `continue` 跳过非 `<Persistent>` 字段、读臂用 `default_value` 补缺（参考 `createInstanceFromStream` 的 `createDefaultValue` 补缺），公开面新增 `add_persistent_to_stream`/`create_persistent_from_stream`（数据库投影形态）；`property_change.rs` 六个负载点显式传 `false`（对齐参考 `/* isPersistentOnly: */ false`）。验收钉三字段字典（ammo 持久/temp 持久关闭/label 持久）投影流 `[7, 1, 'x']`——temp 的 99 不入流，全字段形态 `[7, 99, 1, 'x']`，字节形态差异只在字段过滤（None 指示与 enter/leave/end 元素不变）；读回钉 temp 取类型缺省、ammo/label 从流恢复；投影记账钉 3 持久字段、位次 0/1/2→0/2/3、越界 `None`（参考 `MF_ASSERT( false )`，Rust 统一为可报告结果，登记加严）。225 单测 + 4 差分通过，clippy 0 findings，触碰文件行覆盖 ≥98%（value.rs 99.12%、data_type.rs 98.09%）。登记差异：序列 `dbLen()>0` 持久截断未建模、CLASS 的 `getStreamElement(isPersistentOnly)` 未建模（Rust 流无 CLASS 臂）；仍无字节级参考 oracle，证据为源码级对齐，不虚勾。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-02-07 节。commit `a39d91f`。

#### BW-04-03 可见性与调用约束

覆盖 exposed 方法、属性分发标志、客户端/服务端权限、参数校验、方法应答和 MailBox 引用。比对同一实体定义在 Base、Cell、Client 和 DB 的投影。

- [x] **BW-04-03-01 实现 exposed 方法可见性**。**验收** 客户端只能调用允许方法。
  - 证据：`method.rs` 增 `EntityMethodDescriptions::exposed_method_from_msg_id(msg_id, sub_index, range)`——入站远程调用只经暴露表解析（`exposedIDFromMsgID` → `exposedMethod`）；验收钉三方法表（ping/shout 带 `<Exposed>`、secret 未暴露）：`num_exposed()==2`、每个暴露位次都不是 secret、位次 2 拒绝、`set_exposed_msg_ids` 发出的 msg id 往返还原同方法（exposed_index 一致）、表外 msg id（200）拒绝、赋号前一切拒绝（构造态 `-1`，对齐参考 `maxExposedMethodCount_(-1)`）。**连带修复潜在错误**：原 `#[derive(Default)]` 使未赋号态计数为 0，参考构造器是 `-1`，红测先抓住再修复（TDD red→green）。参考越界 ERROR_MSG+NULL 与未赋号 MF_ASSERT 统一收敛为 `None`（登记）；`component()!=CLIENT` 的断言是调用方使用约束不入解析器（登记）。226 单测 + 4 差分通过，clippy 0 findings，触碰文件行覆盖 ≥98%（method.rs 99.64%）。暴露位矩阵/`exposedMethods_` 收集/固定尺寸前排序/安全检查为 BW-04-01 系列已交付，本项补解析门。无字节级 oracle，证据为源码级对齐，不虚勾。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-03-01 节。commit `af07627`。
- [x] **BW-04-03-02 处理属性分发标志**。**验收** 同一属性的四类接收方与基线一致。
  - 证据：`data_description.rs` 增属性层转发访问器（`is_ghosted_data`/`is_other_client_data`/`is_own_client_data`/`is_base_data`/`is_cell_data`/`is_client_server_data`），全部落到已交付的 `EntityDataFlags` 位谓词（对齐 `data_description.ipp:80-155`：四类接收方是分发位直读，`isCellData()` 是 `!isBaseData()` 否定式，`isClientServerData()` 是 OTHER_CLIENT|OWN_CLIENT|REPLAY 三元并）。验收钉 `ALL_CLIENTS`/`CELL_PUBLIC_AND_OWN`/`BASE_AND_CLIENT`/`BASE`/`CELL_PRIVATE` 五个参考命名表条目的 `(ghost, other, own, base, cell-home)` 全矩阵：(T,T,T,F,T)/(T,F,T,F,T)/(F,F,T,T,F)/(F,F,F,T,F)/(F,F,F,F,T)，并钉 `CELL_PRIVATE` 不进客户端服务器通道。顺带钉 `is_cell_data` 否定式语义。227 单测 + 4 差分通过，clippy 0 findings，触碰文件行覆盖 ≥98%。登记差异：参考解析期 `MF_ASSERT_DEV(!isBaseData() || !ghost && !other)` 未建模（非行为差异）、`CELL_PRIVATE`=0 位是「无接收方」的参考写法（空 `<Flags>` 两侧都是解析失败）；无字节级 oracle，证据为源码级对齐，不虚勾。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-03-02 节。commit `da83a9b`。
- [x] **BW-04-03-03 验证参数与权限失败**。**验收** 返回同类错误且业务回调未执行。
  - 证据：`method.rs` 增 `ArgCountError`（参考 `MethodArgs::checkValid` 原话 Display，含 `expectedArgs == 1 ? "" : "s"` 单复数）与 `MethodDescription::first_ordinary_arg`/`expected_args`/`check_arg_count`（`firstOrdinaryArg = exposedExplicit && isExposed() && component()==CELL`，暴露 CELL 方法带隐式实体参数）。验收钉暴露 CELL 方法 `jump(INT32)`：期望 2（隐式 1 + 声明 1）、给 2 通过、给 1 拒并输出 `"Method jump requires exactly 2 arguments; 1 given"`；基座方法 `spin` 期望 1、多给一个拒且钉单数措辞 `"Method spin requires exactly 1 argument; 2 given"`。计数门在逐参数类型检查之前返回，故错误即终结调用路径、回调不执行（静态 crate 无分派回调，以错误结果等价表达，已登记为口径说明）。228 单测 + 4 差分通过，clippy 0 findings，触碰文件行覆盖 ≥98%。登记差异：`PyErr_Format`/`generateException` 的 Python 异常状态、隐式实体参数的 None/id/obj-with-id 类型检查、逐参数类型不匹配消息（依赖 Python 类型对象）三者均未建模。权限侧（只调暴露方法）见 BW-04-03-01。详见 docs/review/bw04-01-01-entitydef-parse.md 的 BW-04-03-03 节。commit `12c1e31`。
- [x] **BW-04-03-04 实现带返回值的方法应答**。**验收** 响应字段和超时行为一致。
  - 证据：`crates/entitydef/src/method.rs` 新增 `Response::{Values,Failure}` / `ResponseError` 与 `add_return_values_to_stream` / `create_return_values_from_stream` / `create_response_from_stream`，应答帧 `uint8 isError` 起头、错误半帧两字符串、成功半帧逐声明返回值无计数无名字，缺值时以类型缺省照样入流并报错——原话/字节均锚定（`[3,0,0,0,2,'o','k']`、`[1,"TypeError",""]`、`[0,0,0,0]`）。230 单测+4 差分通过，clippy 0 findings，method.rs 行覆盖 99.33%。验收中「超时行为」在参考 entitydef/delegate 层全树检索无对应物（属消息/网络层），如实登记不虚勾（见 docs/review/bw04-01-01-entitydef-parse.md BW-04-03-04 节备注 1）。commit `5ea7e79`。
- [x] **BW-04-03-05 序列化 MailBox 引用**。**验收** 原版可恢复实体位置与类型。
  - 证据：`crates/entitydef/src/value.rs` 新增 `MailBoxComponent`（8 组件按参考数值序）、salt 位域常量、`mail_box_ref`（`init` 打包 `(c<<13)|t`）与 `mailbox_component`/`mailbox_type_id`/`mailbox_component_name`/`mailbox_has_address` 读取器；以参考 fixture（id 2405 @ 0xc0180010:0x4567，BASE_VIA_CELL，typeID 7 → salt 0x6007）做 12 字节流往返，逐项恢复实体位置与类型，另钉全组件名与 ip/port-only `hasAddress` 语义。232 单测+4 差分通过，clippy 0 findings，value.rs 行覆盖 99.19%。无字节 oracle（推导登记）、`<invalid>` 分支不可达登记见 docs/review/bw04-01-01-entitydef-parse.md BW-04-03-05 节备注 1/3。commit `dc7b2d8`。

#### BW-04-04 定义工具和 SDK 契约

复现 `process_defs` 的命令行、诊断及产物。核对 `bwentity` 和客户端集成示例依赖的定义接口与生成结果。

- [x] **BW-04-04-01 兼容 process_defs 参数**。**验收** 帮助、退出码和选项有基准。
  - 证据：`crates/entitydef/src/cli.rs` + `src/bin/process_defs.rs`——usage 文本与 stock `--help` 逐字节（打到 stderr、exit 0）、退出码三基准（help=0/加载失败=1+原话/成功=0）、`getOption`/`hasFlag` 停一位短与盲取值行为逐条建测、`-r` 扫描按 `BWResource::init`；Rust bin 端到端对 stock 实测：`--help` stderr 逐字节相等，service 夹具 dump stdout 与 oracle 逐字节相等，无 `-r` exit 1。238 单测+4 差分通过，clippy 0 findings，cli.rs 100%（bin 不计入 lib 覆盖，由端到端背书；多 `-r` 与未知 flag 差异登记，见 docs/review/bw04-01-01-entitydef-parse.md BW-04-04-01 节备注 4/5）。commit `f56abea`。
- [x] **BW-04-04-02 兼容定义诊断**。**验收** 坏 .def 显示对应文件和字段。
  - 证据：坏定义 fixture（Bad.def 字段 hp 用 NOSUCHTYPE）在 stock 二进制七行诊断链逐字捕获（docs/review/reference/bad_def_diagnostics.txt），Rust 经惰性别名加载（data_type.cpp s_aliasesDone 等价）后差分测试断言六行 [DataDescription] 链顺序与文本逐行一致，含 `Unable to find data type 'NOSUCHTYPE' for Bad.hp` 文件.字段行；238 单元 + 1 诊断差分 + 4 差分通过，clippy 0 findings，覆盖率 data_type.rs 943/962=98.02% 全部 ≥98%（commit 7af5fd6）。
- [x] **BW-04-04-03 复现生成产物**。**验收** 对照 ID、摘要及引用关系。
  - 证据：新建 full_def fixture（3 实体、alias/ARRAY/FIXED_DICT、九个 Flags 族、identifier 链、base/cell/client 脚本三向），stock 产物 docs/review/reference/full_entitydef.json（11,763 字节，digest 6FA9B4C32162519E4A50B9C98136791B），Rust bin stdout 与之逐字节相等（diff 空，exit 0）；ID（实体/属性/clientServerFull index、65535 哨兵）、摘要（全 flag 组合、streamSize、方法 args/returnValues）、引用关系（别名解析、类型组合、脚本存在性、全树 digest）三轴一致，差分测试 full_dump_matches_the_process_defs_oracle 无条件锚定；238 单元 + 6 差分通过，clippy 0，覆盖率全部 ≥98%（commit 4912826）。
- [x] **BW-04-04-04 验证 bwentity 与集成示例**。**验收** 用产物成功加载脚本定义。
  - 证据：bwentity 等价工厂路径（tests/bwentity_load.rs，镜像 EntityDescriptionMapFactory::create：开 scripts/entities.xml → parse → nameToIndex/entityDescription 导航）在 full_def（无条件）、client_integration 示例与 fantasydemo（BW04_CI_RES/BW04_FD_RES 本环境实跑）三棵树加载成功，digest 与 BW-04-04-03 产物逐字节核对一致；锚定测试先红后依参考语义改正（EDITOR_ONLY 属性不进运行时属性集，secret 不占 propertyCount）；238 单元 + 9 差分/工厂测试通过，clippy 0，覆盖率全部 ≥98%（commit 5539da6）。

### BW-05 集群控制、进程发现和恢复

源码 `server/tools/bwmachined/`、`lib/network/machine_guard.cpp`、`server/reviver/`、`server/{baseappmgr,cellappmgr,dbappmgr}/`。

#### BW-05-01 Machined 与服务发现

复现 MachineGuard 查询、进程注册/注销、发现、出生/死亡通知、主机和进程统计，以及原版工具所用的受控进程管理入口。用原版组件和新版守护进程做双向互测。

- [x] **BW-05-01-01 实现 MachineGuard 查询**。**验收** 相同请求返回兼容负载。（044dcf8）
- [x] **BW-05-01-02 登记与注销进程**。**验收** 发现结果即时反映变化。（7430b80）
- [x] **BW-05-01-03 发送出生及死亡通知**。**验收** 订阅者收到同顺序通知。（1505ad8）
- [x] **BW-05-01-04 采集主机和进程统计**。**验收** 公共指标字段满足原版查询。（d535bdd）
- [x] **BW-05-01-05 对齐受控进程管理入口**。**验收** 合法生命周期操作对应原版退出状态。（38be764）
- [x] **BW-05-01-06 原版守护进程与新版角色双向联调**。**验收** 两种替换方向均可发现。（d9bbf9e）

#### BW-05-02 Reviver 外部恢复

覆盖组件探活、优先级、异常重启、重新注册和关服抑制。验收为进程退出和失联后恢复流程与原版等价，且正常停机不会触发误重启。

- [x] **BW-05-02-01 采集探活与过期**。**验收** 延迟、失联及恢复三种状态可复现。（dc28306）
- [x] **BW-05-02-02 实现重启优先级**。**验收** 多个目标失败时按基线顺序处理。（dc28306）
- [x] **BW-05-02-03 异常退出后重启并重新登记**。**验收** 角色恢复可服务状态。（dc28306）
- [x] **BW-05-02-04 识别正常关服**。**验收** 关闭期间不会误重启。（dc28306）

#### BW-05-03 BaseAppMgr 调度

覆盖 BaseApp 注册/移除、负载报告、创建目标选择、登录条件、全局状态转发和关闭协调。比较多实例下的调度和失败回退。

- [x] **BW-05-03-01 登记与移除 BaseApp**。**验收** 调度列表状态一致。（de2e8c9）
- [x] **BW-05-03-02 接收负载报告并挑选创建目标**。**验收** 多实例夹具选择吻合。（de2e8c9）
- [x] **BW-05-03-03 处理登录条件失败与回退**。**验收** 返回预期拒绝状态。（de2e8c9）
- [x] **BW-05-03-04 转发全局状态**。**验收** BaseApp 接收相同更新。（de2e8c9）
- [x] **BW-05-03-05 协调关服**。**验收** 工作进程按阶段退出。（de2e8c9）

#### BW-05-04 CellAppMgr 分区和负载均衡

覆盖 Space/Cell 创建与销毁、BSP 切分、边界报告、负载采样、拆并 Cell、跨主机分配和 viewer 读接口。用原版轨迹对照 split/merge 和重分配。

- [x] **BW-05-04-01 创建和销毁 Space/Cell**。**验收** 空空间与活跃空间状态吻合。（3f62cf6）
- [x] **BW-05-04-02 实现 BSP 切分和边界报告**。**验收** 固定负载序列边界一致。（3f62cf6）
- [x] **BW-05-04-03 实现 Cell 合并**。**验收** 低负载时实体归属与基线一致。（3f62cf6）
- [x] **BW-05-04-04 实现跨主机分配**。**验收** 候选主机与失败回退匹配。（3f62cf6）
- [x] **BW-05-04-05 提供 viewer 读接口**。**验收** 展示拓扑与实际分配一致。（3f62cf6）

#### BW-05-05 DBAppMgr 发现和主从角色

覆盖 DBApp 注册、主节点选取、负载/状态转发，以及死亡后的拓扑和服务通知。比对已登录实体和未完成请求的恢复结果。

- [x] **BW-05-05-01 登记 DBApp 并选取主节点**。**验收** 加入顺序固定时选择吻合。（52c1417）
- [x] **BW-05-05-02 转发 DB 负载与状态**。**验收** 对端可读到最新值。（52c1417）
- [x] **BW-05-05-03 处理节点死亡和角色切换**。**验收** 拓扑与服务通知吻合。（52c1417）
- [x] **BW-05-05-04 核查未完成请求和在线实体**。**验收** 主节点更换后结果可对照。（52c1417）

#### BW-05-06 集群阶段与故障矩阵

对齐启动就绪条件、时间同步、受控关服及经理/工作进程失联通知。逐角色验证重启、网络隔离、进程恢复及重复通知，记录哪些故障原版支持自动恢复。

- [x] **BW-05-06-01 实现启动就绪条件**。**验收** 缺少必要角色时保持未就绪。（c7481a8）
- [x] **BW-05-06-02 实现进程时间同步**。**验收** 固定样本的游戏时间漂移在允许范围。（54e209a）
- [x] **BW-05-06-03 实现关服阶段广播**。**验收** 各角色报告及退出次序吻合。（1236a7f）
- [x] **BW-05-06-04 验证各角色异常退出与重启**。**验收** 结果记入故障矩阵。（51ed0db）
- [x] **BW-05-06-05 验证隔离、重连和重复通知**。**验收** 实体/会话终态可复现。（0d82259）

### BW-06 LoginApp、BaseApp 会话与客户端连接

源码 `server/loginapp/`、`server/baseapp/{proxy.cpp,login_handler.cpp,pending_logins.cpp}`、`server/dbapp/*login*`、`lib/{connection,connection_model}/`。

#### BW-06-01 LoginApp 公共登录入口

复现登录参数和版本检查、挑战的启用和应答、请求缓存、DB 转发、过载及失败状态。比对正常和失败登录的响应状态及线格式。

- [x] **BW-06-01-01 解析登录请求参数和版本**。**验收** 原版样本得到相同接受或拒绝。（8a80a66）
- [x] **BW-06-01-02 实现登录挑战协商**。**验收** 启用与禁用路径均能互通。（3244af7）
- [x] **BW-06-01-03 缓存重复请求**。**验收** 重发得到一致结果。（570ead7）
- [x] **BW-06-01-04 转发 DB 登录并处理超时**。**验收** 响应状态与原版匹配。（b29d94d）
- [x] **BW-06-01-05 返回过载和其他失败原因**。**验收** 错误码及负载一致。（61743fe）

#### BW-06-02 账号认证与重复登录

串通 LoginApp、DBApp billing、BaseAppMgr 和目标 BaseApp。覆盖认证拒绝、登录记录、重复登录/重登和账号实体选择。

- [x] **BW-06-02-01 接通 Billing 账号认证**。**验收** 成功和拒绝均对照。(afd7ca2)
- [x] **BW-06-02-02 选择账号实体并派发目标 BaseApp**。**验收** 正确账号绑定正确实体。(87bc504)
- [x] **BW-06-02-03 维护在线登录记录**。**验收** 断开或拒绝不会遗留记录。(b214a49)
- [x] **BW-06-02-04 处理重复登录与重登**。**验收** 既有会话和新会话终态吻合。(96bb2fa)

#### BW-06-03 BaseApp 接续与 Proxy

复现登录地址切换、pending login/session key、Channel 绑定、Proxy 与玩家实体绑定、断线/踢出/重新连接和会话清理。比较回调和状态顺序。

- [x] **BW-06-03-01 发出 BaseApp 接续地址**。**验收** 客户端按原版顺序切换。(8315c4a)
- [x] **BW-06-03-02 绑定 pending login 与 session key**。**验收** 无效或过期票据被拒绝。(04d72a3)
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

- [x] **BW-16-01-01 建立应用与角色 crate 依赖单向关系**。**验收** 依赖图无环。
  - 证据：原版 C++ 无分层约束（依赖靠 Makefile 隐式），新版在 crates/test-support/src/layering.rs 建立分层注册表（foundation=0、platform=1、contracts/entitydef/mercury/python-host/resources=2、server-runtime=3、boundless-server/test-support=4）+ manifest 行级扫描 + Tarjan 环检测 + cargo metadata 成员交叉校验，`workspace_rules_hold` 守护全成员，cargo test -p test-support 7 passed。
- [x] **BW-16-01-02 抽取进程间消息契约**。**验收** 调用方无需依赖对端服务 crate。
  - 证据：原版 `lib/network/machine_guard.hpp` 的 MachineGuardMessage 体系原先分散在 machined 与各角色实现里，新版抽到 crates/contracts/src/machined.rs（消息 id、标志、param/type/category 枚举、Header+ProcessMessage+ListenerMessage+SignalMessage 及 matches/describe）。`process_message_matches_reference_bytes` 用按原版字段序手写的 38 字节向量逐位断言，另测旧发送方无 extra 块的兼容读取与未来字段跳过；cargo test -p contracts 8 passed。
- [x] **BW-16-01-03 验证独立构建**。**验收** 指定服务能单独编译。
  - 证据：原版 GNU Make 无按服务单独编译入口，新版 crates/test-support/src/standalone.rs 从根 manifest 枚举 `apps/` 下应用包并逐个 `cargo build -p <package>`，`application_crates_build_independently` 常驻守护；实测 `cargo build -p boundless-server` 生成 target/debug/boundless-server 且 role 参数校验正常。

#### BW-16-02 平台与外部依赖边界

OS 调用进 `platform`，图形后端进 `rendering`，数据库驱动进 `storage`，CPython FFI 进 `python-host`。应用入口装配具体实现，通过 workspace 依赖图检查循环及跨层引用。

- [ ] **BW-16-02-01 收敛系统调用至 platform**。**验收** 依赖扫描确认调用位置。（依赖 crates/platform 改动冻结，主席位 flake 调查在途，解禁后解锁）
- [ ] **BW-16-02-02 隔离图形后端**。**验收** 服务端构建不加载图形后端。（依赖 rendering crate 未建，随 BW-11 立项解锁）
- [ ] **BW-16-02-03 隔离数据库驱动**。**验收** 可单独启用后端。（依赖 storage crate 未建，随 BW-09 立项解锁）
- [ ] **BW-16-02-04 隔离 CPython FFI**。**验收** 绑定与解释器所有权有测试。（依赖 python-host crate 未建，现为 4 行空壳，随 BW-03 立项解锁）
- [x] **BW-16-02-05 接入依赖检查**。**验收** 跨层违规产生可定位报告。
  - 证据：原版 GNU Make/CMake 无依赖方向检查（仅编译顺序）；新版 `cargo run -p test-support --bin check-deps` 输出 `manifest:line: kind: detail`，同规则由 cargo test 常驻。实测合成 workspace 得 `crates/above/Cargo.toml:7: unregistered crate` 并退出码 2。

#### BW-16-03 Python 布局与资源交付

实现各角色的模块注册和部署搜索路径，保留 `BigWorld` 等公开导入名称及原版资源定位。用 Demo 原样导入测试检验构建目录和安装目录。

- [ ] **BW-16-03-01 布置按角色加载的 Python 模块**。**验收** 角色之间公开 API 正确隔离。（依赖 python/ 模块目录与 python-host crate 未建，随 BW-03 立项解锁）
- [ ] **BW-16-03-02 实现原版导入名称和路径**。**验收** Demo 无需改导入。（依赖 python/ 模块目录与 python-host crate 未建，随 BW-03 立项解锁）
- [ ] **BW-16-03-03 复测构建目录及安装目录**。**验收** 两种启动均找到同一资源。（依赖 python/ 模块目录与安装脚本未建，随 BW-03 立项解锁）

#### BW-16-04 源码映射与扩展接口

登记原版构建目标、功能编号、新 crate/模块及测试位置。编辑器和工具复用世界/资源/转换库。明确 Rust 原生扩展与 Python 扩展各自入口，验证加载和错误传播。

- [ ] **BW-16-04-01 登记原版目标到新版目录**。**验收** 每个目标有模块与测试路径。（依赖各角色 crate 与目标树未建，随 BW-03/BW-09/BW-11/BW-13 逐项解锁）
- [ ] **BW-16-04-02 统一编辑器和工具的资源入口**。**验收** 两类应用可加载同一产物。（依赖 editor/tools 应用未建，apps/ 现仅 server，随 BW-13 立项解锁）
- [ ] **BW-16-04-03 定义 Rust 扩展入口**。**验收** 成功及错误能正确传播。（依赖 editor/tools 应用与扩展宿主未建，随 BW-13 立项解锁）
- [ ] **BW-16-04-04 定义 Python 扩展入口**。**验收** 导入失败可定位且状态可清理。（依赖 python-host crate 与 python/ 模块未建，随 BW-03 立项解锁）

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

- [x] **BW-18-01-01 提供资源根和临时目录夹具**。**验收** 并行测试目录隔离。
  - 证据：原版 `base_resmgr_unit_test_harness.cpp` 从 `BW_ROOT`/可执行文件目录合成 `--res` 路径，测试耦合检出布局；新版 crates/test-support/src/workspace.rs 提供 TempWorkspace（unique 目录、drop 即清理、失败可 keep 现场）与 ResourceRoot，`workspaces_are_isolated`/`sequential_fixtures_get_distinct_directories`/`dropped_workspaces_are_removed` 常驻验证并行隔离。
- [x] **BW-18-01-02 提供固定随机种子**。**验收** 重复调用给出同一序列。
  - 证据：原版 `lib/network/unit_test/network_app.hpp:57` 用 `srand((int)timestamp())` 播种，重复运行轨迹不同、无可复现种子；新版 crates/test-support/src/rng.rs 提供 SeededRng（SplitMix64，平台无关），`the_same_seed_repeats_the_same_sequence` 钉死同种子同序列、`the_zero_seed_is_usable` 覆盖零种子、`bounded_values_stay_in_range` 覆盖取界。
- [x] **BW-18-01-03 提供配置和数据库夹具**。**验收** 测试可从已知状态启动。
  - 证据：原版测试各写各的 bw.xml 与临时 DB 路径，无统一已知状态入口；新版 crates/test-support/src/config_fixture.rs 提供 ConfigFixture（写入 server/bw.xml 链后走真实 BwConfig::init_from_sources 解析，`the_chain_resolves_child_first`/`a_missing_personalised_file_falls_back_to_base` 验证链序与回退）与 DatabaseFixture（全新 db/、journal/ 目录布局，实体库本体待 BW-09），`a_database_backed_config_starts_from_a_known_state` 串起两者证明测试从已知状态启动；为可断言链序给 BwConfig 加了 `section_names()` 访问器（crates/server-runtime/src/config.rs）。
- [x] **BW-18-01-04 映射原版单测**。**验收** Rust 和 pytest 用例可通过原始编号追溯。
  - 证据：crates/test-support/src/bin/scan-cases.rs 扫描参考检出 `lib/**/unit_test*/**.cpp` 的 `TEST()` 注册，生成 cases/original_inventory.txt（908 条，模块/文件/行号/用例名，committed）；crates/test-support/src/case_registry.rs 解析该清单并提供 `case_named`/`targets_for`/`originals_covered_by` 正反查，`PORTED` 映射表以 machined 契约 7 条已迁用例作种子（如 MGMMSG_listenerRoundTrip → contracts::machined::tests::listener_message_round_trips），`every_mapping_names_a_real_original_case`/`no_target_is_claimed_by_two_originals` 常驻防错；target 兼容 Rust 测试路径与 pytest node id，各 lane 随迁移追加行。

#### BW-18-02 多进程测试框架

对应原版 `multi_proc_test_case`，提供按拓扑启动、按条件等待就绪、独立端口和数据库、日志采集、失败现场保留及进程清理。验证并行执行时隔离和三端清理。

- [x] **BW-18-02-01 按拓扑启动进程**。**验收** 依赖顺序可观察。
  - 证据：原版 `multi_proc_test_case.cpp` 只有 fork/waitpid 匿名子进程，无角色与拓扑概念；新版 crates/test-support/src/multiproc.rs 提供 Topology（ProcessSpec 带 depends_on 边）+ Kahn 拓扑排序（独立角色按插入序保证可复现）+ ProcessGroup::start 按序真实 spawn，`dependents_start_after_their_dependencies`/`independent_roles_keep_insertion_order` 钉死顺序可观察，`cycles_list_their_members`/`unknown_dependencies_report_both_roles`/`duplicate_role_names_are_rejected` 报错带角色名，`a_started_group_spawns_roles_in_dependency_order` 验证真实进程按序启动且 pid 独立，`a_failed_spawn_names_the_role_and_cleans_up` 验证半启动不泄漏。
- [x] **BW-18-02-02 按就绪条件等待**。**验收** 超时能明确定位角色。
  - 证据：原版测试用 `sleep( 0.5 )` 硬等后直接连接，超时无从定位；新版 multiproc.rs 提供 Readiness 三种条件（PortOpen 探测 127.0.0.1 监听、FileExists 标记文件、LogContains 日志就绪行）+ `wait_ready` 轮询等待，`a_timed_out_wait_names_the_role_and_condition` 钉死超时错误带角色名+条件+等待毫秒，`an_exited_role_fails_fast_with_its_status` 验证角色先死立即报错不空等，`wait_ready_all_reports_the_first_failing_role` 验证多角色等待点名第一个未就绪者，`unknown_role_in_a_wait_is_rejected` 拒绝拼错的角色名。
- [x] **BW-18-02-03 分配独立端口与数据库**。**验收** 两个测试并跑互不干扰。
  - 证据：原版测试硬编码端口段并共用检出内数据目录，并跑必撞；新版 crates/test-support/src/allocation.rs 提供 PortAllocator（bind 127.0.0.1:0 由内核发号、PortLease 持有监听期间无人可拿同号）与 RunRoot（logs/db/artifacts 布局）+ database_slot（同标签也各得独立空 db/+journal/ 槽位），`port_leases_are_distinct_while_held`/`bulk_leases_come_back_all_distinct`/`two_parallel_runs_never_overlap`（双 run 根+双分配器 6 端口 2 槽位全不相交）常驻验证并跑互不干扰。
- [x] **BW-18-02-04 保留日志与失败目录**。**验收** 失败报告可打开原始产物。
  - 证据：原版 MultiProcTestCase 子进程输出直接进测试 stdout，失败后无现场可查；新版 multiproc.rs 提供 `start_logged`（每角色 stdout+stderr 汇入 RunRoot logs/&lt;role&gt;.log，保持角色实际输出顺序）与 `preserve_failure`（artifacts/failure-report.txt 记录原因、启动顺序、各角色退出状态与日志路径，并 keep 整个 RunRoot），`role_output_is_captured_in_the_run_log` 用日志就绪探针证明输出进的是运行日志，`a_failure_scene_keeps_the_original_logs_and_a_report` 验证报告可打开且原始日志可读、RunRoot 在 drop 后仍存活；另补 `kill_all` 让测试不再空等长驻角色。
- [x] **BW-18-02-05 清理整个进程树**。**验收** 三端测试退出无残留进程。
  - 证据：原版 `killChildren` 只 SIGKILL 直接子进程，角色再 fork 的孙进程泄漏；新版 multiproc.rs spawn 时给每角色 `process_group(0)`（自成为进程组长），`kill_tree` 用 `kill(-pid)` 打全组（libc），Drop 兜底未显式清理的组，`kill_tree_takes_out_grandchildren` 验证角色 fork 的孙进程一起死，`three_roles_leave_nothing_behind` 三端各带 helper 全清无残留（ps 按 marker 扫描），`parallel_groups_clean_up_independently` 验证并跑两组互不误伤，`a_dropped_group_tears_itself_down` 验证测试 panic 掉组也不泄漏。

#### BW-18-03 可控时间与消息故障

对应原版 `lib/testing/` 消息过滤能力，提供虚拟时钟、手动推进 tick、丢包、延迟、乱序与进程故障注入。验证定时器、重传、迁移和恢复行为可重复。

- [x] **BW-18-03-01 实现手动时钟**。**验收** 同一序列推进得到相同 Tick。
  - 证据：原版 lib/network/unit_test/manual_clock.hpp 从墙钟起步、advanceMs 走浮点秒换算，重复运行 Tick 不同；新版 crates/test-support/src/clock.rs 提供 ManualClock（stamp 零起步、纳秒刻度与 stamp_now()/DispatcherTimers ClockFn 完全同单位，advance_ms/rewind_ms 定点整数推进、rewind 过零环绕保留原版无符号减法语义），`the_same_advance_sequence_yields_the_same_ticks` 钉死同序列同 Tick，`time_stands_still_until_advanced`/`milliseconds_convert_to_nanosecond_stamps`/`the_injected_clock_fn_tracks_advances` 验证静止性、刻度与 ClockFn 注入可观察推进。
- [x] **BW-18-03-02 实现确定性丢包**。**验收** 指定包触发预期重传。
  - 证据：原版 `test_mangle.cpp` 用 `g_mangleProb=0.05` 墙钟概率丢包，无法指名丢哪个包、重传行为不可复现；新版 crates/test-support/src/channel.rs 提供 ScriptedChannel（按发送序号 script 每包 Fate，Drop 包进 dropped() 记录），`designated_packets_drop_and_the_rest_deliver` 验证指名丢包其余按序达，`a_designated_drop_triggers_the_expected_retransmission` 走通 丢1→缺口→重发→新副本必达 的完整重传环，`script_by_chance` 用 SeededRng 复刻概率模式且 `seeded_chance_scripting_is_reproducible` 钉死同种子同丢包集；mercury 包类型 BW-02+ 接入同通道。
- [x] **BW-18-03-03 实现延迟及乱序**。**验收** 按指定顺序交付。
  - 证据：原版测试靠真实网络抖动或睡固定毫秒，延迟乱序无法指名；新版 channel.rs 增加 `Fate::Delayed{until_nanos}`（吃 BW-18-03-01 虚拟时钟戳）与 `Fate::After(anchor)`（锚定交付，脚本化换序），pump 内解约束到不动点，`delayed_packets_deliver_when_the_clock_advances` 验证差 1ms 都不交付、到点即达，`anchored_packets_deliver_after_their_anchor`/`an_anchor_chain_delivers_in_the_scripted_order` 钉死按指定顺序交付（含链式反序 2,1,0），`delay_and_reorder_stay_deterministic_together` 验证延迟+乱序同脚本同时钟必同序，`an_anchor_behind_a_dropped_packet_stays_back` 验证锚在被丢包上时不误交付。
- [x] **BW-18-03-04 提供进程故障夹具**。**验收** 可复现迁移或恢复。
  - 证据：原版测试崩溃恢复靠人工 kill 或不可控退出，场景无法重放；新版 multiproc.rs 在 ProcessGroup 上提供 `crash`（整树 SIGKILL 并收割、腾出槽位）与 `restart`（按原 spec + 原日志捕获重生新 pid），`a_crashed_role_restarts_with_a_fresh_pid` 验证 kill-pid 对上、新 pid 不同，`a_migration_scenario_replays_identically` 在两份独立副本上跑 启动→crash→重启→恢复 同一状态机序列完全一致（恢复标记只有重启副本能写，排除误判），`a_restarted_role_keeps_its_log` 验证重启后日志续写原文件，`crashing/restarting_an_unknown_role_is_rejected` 拒绝拼错角色。
- [x] **BW-18-03-05 接通可控时钟取消测试**。**验收** 定时器重复用例稳定。
  - 证据：原版定时器跑真实墙钟，重复用例每次触发次数都不同、取消测试与截止时刻赛跑；新版 timer_scenarios.rs 把 `ManualClock::clock_fn()` 注入 `DispatcherTimers::with_clock`（对应 `EventDispatcher::clock_`），时间只在脚本推进时流动；`repeat_timers_deliver_the_same_sequence_every_run` 验证 12 次 50ms 推进的交付序列逐位重放一致，`cancelling_a_repeat_timer_stops_exactly_on_the_script` 验证 200ms 处 cancel 恰好停在 @200 触发之后、另一 70ms 定时器照常走完，`a_cancelled_once_off_never_fires` 验证 once-off 取消后时钟跑过 500ms 也不发，`mixed_deadlines_interleave_in_stamp_order` 验证混合截止按戳序交错稳定；交付日志记录的是回调运行所在 pass 的时钟戳（与处理器读钟所见一致），4 用例 0 失败。

#### BW-18-04 本地与 CI 共用入口

提供按模块执行、全量测试、兼容差分、覆盖率和结果归档入口。文档记录调用、依赖、夹具来源和失败复现方式。

- [x] **BW-18-04-01 提供单模块和全量入口**。**验收** 同一模块可单独执行。
  - 证据：本地与 CI 走同一入口 `cargo run -p test-support --bin run-tests -- ...`；harness.rs 的 `workspace_modules` 从根 manifest `members` 解析全部 10 个 crate 名、`Plan::for_module` 校验模块名（拼错时报错并列出全部已知 crate）、`cargo_args` 给出映射的 cargo 调用（`-p <crate> [filter]` / `--workspace`）；实测 `run-tests -- test-support timer_scenarios` 单独执行该模块（4 用例通过、其余 90 过滤）、`run-tests -- all` 跑全 workspace、`run-tests -- clock` 拒绝未知模块，7 项单测覆盖映射规则。
- [x] **BW-18-04-02 提供兼容差分入口**。**验收** 输出可定位差异。
  - 证据：harness.rs 增加共享归档口径 `PASS/FAIL/SKIP <用例名>` 行格式（`parse_outcomes` 跳过头/注释/空行，同一文件本地与 CI 产物通吃）与 `diff_runs`（按名匹配、重名取最坏结果、输出 REGRESSED/healed/MISSING/ADDED 四类），入口 `run-tests diff <baseline> <current>` 每行点名差异用例，实测对样例归档输出 `REGRESSED case_b`+`ADDED case_new` 并以 exit 2 拒绝不兼容；未重跑他人兼容差分（各 lane 自行喂归档），14 项单测覆盖解析与四类分类。
- [x] **BW-18-04-03 提供覆盖率入口**。**验收** 报告能映射源码。
  - 证据：harness.rs 的 `coverage_report_dir`（target/test-results/cov-<module>，本地与 CI 同路径）与 `coverage_args`（cargo-llvm-cov 0.9.1，`--lcov --output-path`；0.9.1 不允许 --html 与 --lcov 同用故取 lcov 单报告），入口 `run-tests cov [module]` 校验模块名后生成报告并打印映射的源文件数；实测 `run-tests cov foundation` 产出 63KB lcov.info，SF: 记录映射 8 个真实源文件（ids.rs..time.rs），`DA:87,2` 与 `FN:87 is_null_entity_id` 逐行对上——报告映射到源码文件+行号；`lcov_source_files` 解析 SF: 记录有单测，17 项 harness 单测全绿。
- [x] **BW-18-04-04 提供结果归档**。**验收** CI 和本地采用同一目录及格式。
  - 证据：harness.rs 的 `RunArchive`（label/run 命令/commit/when 头 + `PASS/FAIL/SKIP <名>` 行 + summary，渲染确定且 `parse_outcomes` 可原样读回）与 `parse_cargo_test_output`（解析 libtest 默认 `test <名> ... ok/FAILED/ignored[, reason]` 行），入口把 cargo 测试 stdout 边流式回显边收结果，统一写入 `target/test-results/<label>/run.txt`（本地与 CI 同一目录同一文件格式）；实测 `run-tests -- test-support timer_scenarios` 产出含 `commit: 7b8793f` 的 run.txt（4 PASS+summary），该产物直接喂 `run-tests diff` 判 no differences、exit 0；全 crate 111 项单测全绿。
- [x] **BW-18-04-05 编写失败重现说明**。**验收** 从产物可复跑相应用例。
  - 证据：新增 `crates/test-support/RUNS.md`，记录入口调用（all/单模块/单测试模块/diff/cov 一张表）、依赖（cargo、cargo-llvm-cov 0.9.1、git、date、multiproc 所需 /bin/sh+sleep+true 与临时端口）、夹具来源（ConfigFixture 走生产 init_from_sources、multiproc 拓扑+进程组+crash/restart、ManualClock+timer_scenarios、ScriptedChannel+SeededRng、case_registry 908 原始用例清单）与失败复现五法（归档 `run:` 行原样复跑、diff 两归档点名差异、可控时钟逐位重放、进程泄漏 marker 扫描、lcov DA 行核对）；harness.rs 增加 `rerun_command` 从归档程序化提取 `run:` 行（2 项单测），实测从真实 run.txt 提取 `cargo test -p test-support timer_scenarios` 复跑得同 4 用例全过，26 项 harness 单测全绿，llvm-cov 实测 harness.rs 行覆盖 99.0%（14 源文件全部映射）。

### BW-19 VitePress 文档站

参考原版 `docs/.vitepress/config.mjs`、`docs/zh/` 和仓库根 `package.json`。Boundless 文档站位于 `docs/`。

#### BW-19-01 站点骨架

配置导航、侧栏、搜索、静态资源和构建入口。在 CI 校验链接与生产构建，并生成可预览的站点产物。

- [x] **BW-19-01-01 构建 VitePress 配置及导航**。**验收** 关键页面可访问。
  - 证据：原版 BigWorld `docs/.vitepress/config.mjs` 用 `locales{root:en, zh}` 双语、nav 5 项、`ignoreDeadLinks: false`；新版为 `docs/.vitepress/config.mjs` 单语 `en-US`、`ignoreDeadLinks: false`、`cleanUrls: false`、`base: "/"`，`themeConfig.nav` 挂 Overview/Guide，`sidebar` 键 `/guide/`，`search` 与其余栏目随后续条目增量生长。收口时站点已增量生长到 **nav 5 项**（Overview/Guide/Architecture/Reference/Operations）与 **4 个 `sidebar` 组**（`/guide/`、`/architecture/`、`/reference/`、`/operations/`，后三组各带 `index.md` 汇总页），19 个页面。依赖由 `docs/package.json`（pnpm 管，仓内装）解析 vitepress 1.6.4（125 包，102 复用离线 store）。`pnpm --dir docs run build` 绿；`/` 与 `/guide/` 关键页面在 dev 与 preview 两入口均 HTTP 200。
- [x] **BW-19-01-02 配置搜索与静态资源**。**验收** 搜索命中且资源路径可用。
  - 证据：原版 `docs/.vitepress/config.mjs` 在两个 locale 的 `themeConfig` 内各写 `search: { provider: "local" }`，静态资源放 `docs/public/`；新版为 `themeConfig.search = { provider: "local" }`（纯客户端本地索引，不引外部搜索服务），并接 `docs/public/`。产物含本地搜索索引 `.vitepress/dist/assets/chunks/@localSearchIndexroot.*.js`（91755 字节，实测命中 FantasyDemo ×6、ServerAppConfig、beeline、capacity）；收口时该索引随页面增长到 **107813 字节**，静态资源 `docs/public/examples/bw.standalone.xml` 与 `docs/public/robots.txt` 均打包进 dist 并可由 preview 以 HTTP 200 取到（字节数与 dist 内一致）。`bw.standalone.xml` 非填充物：XML 可解析，8 个取值与 `crates/server-runtime/src/server_app.rs` 默认值逐一对齐。`pnpm --dir docs run build` 绿（8.77s）。
- [x] **BW-19-01-03 接入链接检查**。**验收** 坏内部链接使检查失败。
  - 证据：原版 `scripts/check-docs-links.py`（88 行，仓根，接受 docs 参数、逐 .md 扫描内联/图片/引用定义、绝对链接以 docs 根为根、跳过 `.vitepress`、退出码 0/1）；新版为 `docs/scripts/check-docs-links.py` 并以 `pnpm run check:links` 接入（留在本席独占的 `docs/` 内，不改他人目录）。相对原版四处修正：①跳过 `node_modules/`、`dist/`、`.git/`（原版无此过滤，实测在 `docs/` 内置 node_modules 时扫进 **72 条 node_modules 误报**）；②静态资源改按"非页面后缀一律查 `public/`"的通用规则（原版硬编码扩展名表，实测漏掉 `.xml`，导致 `bw.standalone.xml` 报 1 dead）；③额外扫描 `.vitepress/config.mjs` 的 `link:`（原版不查导航/侧栏），且先剥离 JS 注释——注释里记载的 `link: '/zh/'` 会被误判；④站点根 `/` 解析为 `index.md`。坏链接使检查失败已真造 prove-red 后删除：checker 由 21 链接 0 dead（exit 0）→ 22 链接 **1 dead（exit 1）** → 删除后回 21/0（exit 0）；同一坏链接使 VitePress 构建门禁（`ignoreDeadLinks: false`）失败，`pnpm run build` 报 `1 dead link(s) found`、exit 1；恢复后构建绿（7.76s）。
- [x] **BW-19-01-04 生成预览与生产产物**。**验收** 两种入口都能加载站点。
  - 证据：原版 `package.json`（仓根）三个入口 `docs:dev|docs:build|docs:preview` = `vitepress dev|build|preview docs`，以 srcDir 传参；新版为 `docs/package.json` 的 `dev`/`build`/`preview`（不传 srcDir，以 `docs/` 自身为 srcDir，故 CI 可 `pnpm --dir docs run build`）。两种入口均实测可加载站点：`pnpm run build` 绿、**5.58s**、exit 0，产物 `.vitepress/dist/` 共 **17 个 HTML**（16 页 + 404）并含静态资源（收口时随页面增长到 **20 个 HTML**：19 页 + 404） `examples/bw.standalone.xml`、`robots.txt`；`preview --port 4597` 下 `/`、`/guide/`、`/architecture/process-topology`、`/reference/api` 全部 **HTTP 200**；`dev --port 4596` 下 `/`、`/guide/` **HTTP 200**。产物目录由 `docs/.gitignore` 忽略；私有仓只建产物、不发布 GitHub Pages。

#### BW-19-02 用户与开发文档

编写三端安装、Demo、独立服务启动、Python 实体开发、客户端 SDK 和引擎接入指南。每条快速开始使用干净环境验证。

- [x] **BW-19-02-01 编写三端安装指南**。**验收** 各端干净环境可复现。
  - 证据：原版无对应的三端安装页（`docs/getting-started/` 仅 build-from-source / project-structure / api-reference，面向 C++ 源码构建；其 README 记录服务端基准 Linux x86-64、C++23、嵌入 CPython 3.13.16）；新版为 `docs/guide/install.md`，按 BW-15 首批三端 Linux x86-64 / Windows x86-64 / macOS arm64 分表，含 `rust-toolchain.toml` 钉定 1.100.0、`cargo build -p boundless-server`、release profile（`opt-level=3`+`debug=true`）与干净安装 4 步清单。Linux 标注 Verified 并实测：`cargo build -p boundless-server` 绿（首编 3.17s，复检命中缓存 0.04s）；`target/debug/boundless-server loginapp` 打印 `[loginapp] starting (boundless minimal entry)` exit 0，`bogus` exit 1，无参 exit 1——与页面所述逐条吻合。Windows 与 macOS 如实标注「未在本树验证」并指向 BW-15-03/04（`crates/platform/` 现仅 `clock.rs`；收口时已长到 **4 个模块** `clock`/`interface`/`socket`(unix)/`poller`(linux)，页面相应改为「只有 `clock` 与 `interface` 处处可编译」），不代餐。`pnpm --dir docs run build` 绿 5.68s，checker 24 链接 0 dead。
- [x] **BW-19-02-02 编写 FantasyDemo 快速开始**。**验收** 按步骤可登录进入世界。
  - 证据：原版 FantasyDemo 位于 `BigWorld/programming/fantasydemo/`（108 文件），`res/scripts/entities.xml` 声明 5 类实体 Account/Avatar/Guard/Space/ClientOnly，脚本分 base/cell/client 三层，全部由 `scripts/generate-fantasydemo-assets.py` 生成（末行 `-- do not edit.`）、Tab 缩进；新版为 `docs/guide/fantasy-demo.md`，含五类实体表、两份真实脚本全文（`base/Account.py` 15 行、`cell/Avatar.py` 18 行）、目录树、六步验收对照表（BW-14-01-01..06），并如实标注所引路径是**原版路径**、本树尚无 `fantasydemo/`（规划目标 `examples/fantasydemo/`）。12 个引用路径实测 **11 个 EXISTS**，唯一例外 `res/scripts/alias.xml` 不存在——实为 `entity_defs/alias.xml`（该目录恰为 5 个 `.def` + `alias.xml`），文档已改正；`entities.xml` 的生成横幅注释含 `--`，使严格 XML 解析器拒绝（`xml.etree` 报 not well-formed，line 2 col 59），已以 tip 块如实登记，BW 自身解析器接受。验收「按步骤可登录进入世界」依赖 BW-14：本树无登录协议与 Python host，页面明示该六步为 BW-14-01 的验收目标而非可跑步骤，不代餐。`pnpm --dir docs run build` 绿 5.47s，checker 25 链接 0 dead。
- [x] **BW-19-02-03 编写独立服务指南**。**验收** 命令能启动并停止单角色。
  - 证据：原版基准为 `game/bin/server/el7/server/` 下 13 个已构建角色二进制（baseapp、baseappmgr、cellapp、cellapp-extensions、cellappmgr、commands、dbapp、dbapp-extensions、dbappmgr、loginapp、reviver、serviceapp、bots），由 bwmachined2 统一拉起整组；`serverMode` 默认 `standalone`，另接受 `center`/`periphery`；新版为 `docs/guide/standalone-server.md`，如实区分「已实现」与「目标」：单角色启动/校验已实现，角色间消息未实现（`crates/contracts/src/lib.rs` 实测 3 行、**0 个 `pub` 项**，仍是占位；收口时它已长出 `machined` 模块——`lib.rs` 7 行 1 个 `pub mod`、`machined.rs` 581 行含 `Header`/`ProcessMessage`/`ListenerMessage`/`SignalMessage` 及各自 `write`/`read`，但根 `Cargo.toml` 的 `[workspace.dependencies]` 里 `contracts` **仍被注释掉**、无任何 crate 消费，故页面结论不变），停机机制已实现但未接入入口（`server-runtime/src/app.rs` 模块注释写明完整 init 链随 **BW-01-04** 落地），故页面警示框明确写「角色进程自行退出，SIGINT/SIGTERM 尚无注册处理器」，不谎称可长跑。验收实测：`cargo build -p boundless-server` 绿（复检 0.03s）；`target/debug/boundless-server loginapp` 打印 `[loginapp] starting (boundless minimal entry)` **exit 0**；`bogus` 打印 `unknown server role 'bogus' (expected one of: baseapp, baseappmgr, cellapp, cellappmgr, dbapp, dbappmgr, loginapp, reviver)` **exit 1**；无参走同一路径（空角色）**exit 1**；八个角色逐一实测全部 **exit 0**。页面另附 5 条命令冒烟清单、`ShutDownStage` 六级取值表（None=0…Trigger=5，与 `shutdown.rs` 逐项一致）、与原版二进制布局的差异说明（原版多 `serviceapp`/`commands`/`*-extensions`，`bots` 是压测器而非服务角色）。`pnpm --dir docs run build` 绿 **5.61s**，checker **27 链接 0 dead**。
- [x] **BW-19-02-04 编写实体开发指南**。**验收** 示例脚本有可观测回调。
  - 证据：原版基准为 `programming/bigworld/scripts/generate-fantasydemo-assets.py` 生成的 base/cell/client 三层脚本，base 部分 `Account`/`Avatar` 派生 `BigWorld.Proxy`、`Guard`/`Space` 派生 `BigWorld.Base`，cell 与 client 部分一律 `BigWorld.Entity`；回调以 `/*~ callback` 注释分散在 `server/cellapp/entity.cpp`、`server/baseapp/base.cpp`、`server/baseapp/proxy.cpp`、`client/py_entity.cpp`、`client/entity.cpp`；`.def` 属性标志表在 `lib/entitydef/data_description.cpp`；新版为 `docs/guide/entity-authoring.md`，含逐回调出处表（`Entity.__init__`/`onDestroy`/`onLoseControlledBy`/`onRestore`、`Base.onLoseCell`/`onGetCell`/`onTimer`、`Proxy.onClientDeath`/`onEntitiesEnabled`/`onClientGetCell`、client 侧 `onBecomePlayer`/`onBecomeNonPlayer`/`onEnterWorld`/`onLeaveWorld`）、九值 `<Flags>` 映射表（FantasyDemo 实测用五值：`ALL_CLIENTS`×4、`BASE_AND_CLIENT`×2、`OTHER_CLIENTS`×1、`BASE`×1、`OWN_CLIENT`×1）、三端 `BigWorld.createEntity` 签名表（cell 5 参、client 6 参多 `vehicleID` 且返回 entity **id**、base 为 `createBaseLocally` 别名）。写页过程据源码更正三处易错点并如实入文：①生成脚本 `cell/Avatar.py` 的 `onLoseControl` 是**死代码**——框架真正钩子名为 `Entity.onLoseControlledBy`，全树 grep `"onLoseControl"` 仅命中生成脚本自身，故以 warning 块登记；②`createEntity` 三端签名不可混用（原文初稿误写为单一致签名）；③`<Flags>` 补全至九值。可观测回调即 `cell/Avatar.py` 的 `onTimer` → `self.allClients.fromCell("Hello from the cell")` + `cellTick(1)`，引用块与生成器 370-387 行**逐字节一致**（Tab 缩进保留）。`crates/python-host/src/lib.rs` 实测 **4 行**、仓内无任何 `.py`（唯一为 `docs/scripts/check-docs-links.py`），页面明示脚本尚不能跑在本树（BW-03/BW-14）。`pnpm --dir docs run build` 绿 **7.00s**，checker **30 链接 0 dead**。
- [x] **BW-19-02-05 编写 SDK 与引擎接入指南**。**验收** 示例包含第一次 RPC 验证。
  - 证据：原版基准为 `examples/client_integration/python/simple/`（C++ 宿主内嵌 Python：`main.cpp`/`py_server.cpp`/`py_entity.cpp` + `res/scripts/`），README 45-47 行给出登录命令（`./simple_python_client --res … -server <addr>:<port> -user MyAccount -password a`），与 `client-sdk.md` 引用块逐字一致；第一次 RPC 双向均有原版出处：client→Base 是 `client/ClientAvatar.py` 的 `onFinish` → `self.base.logOff()`，Cell→Client 是 `cell/Avatar.py` 的 `onTimer` → `self.allClients.fromCell("Hello from the cell")`，客户端回显行 `ClientAvatar.fromCell: <id>: <msg>` 与源码 print 语句逐字一致，且两者都由 `.def` 声明（`<BaseMethods><logOff>` / `<ClientMethods><fromCell>`）而非脚本自造；Android/iOS 位于 `examples/client_integration/c_plus_plus/{android,ios}/`。新版为 `docs/guide/client-sdk.md`（能力清单对应 BW-21-01-01..06、ABI 清单对应 BW-21-02-01..05、五步第一次 RPC 表、移动端互通、今日可跑命令与 Mercury 常量表——`PACKET_MAX_SIZE=1472`/`HEADER_SIZE=2`/`RESERVED_FOOTER_SIZE=27`/`MAX_CAPACITY=1472-2-27=1443` 与 `crates/mercury/src/packet.rs:10-21` 逐一相符）与 `docs/guide/engine-integration.md`（Unity/Unreal/Godot 三表与 todo.md **BW-21-03-01..04 / BW-21-04-01..05 / BW-21-05-01..05** 验收逐条对应，并点出「销毁后不再派发」被单列为 BW-21-04-04/BW-21-05-04 这一信号）。诚实登记：仓内实测无 `sdk/` 目录，两页均以 warning 块声明 BW-21 未交付，`sdk/{core,ffi,python,engines/{unity,unreal,godot}}` 布局引自 todo.md 目标目录树一节（行号随合并漂移，收口时位于 1154-1159 行）而非现状。`pnpm --dir docs run build` 绿 **10.21s**，checker **36 链接 0 dead**。

#### BW-19-03 架构及运维参考

维护进程拓扑、协议兼容矩阵、配置和 API、数据库迁移、Bots 压测、内部测试框架和发布指南，链接对应任务及可运行样例。

- [x] **BW-19-03-01 维护进程拓扑和兼容矩阵**。**验收** 可追溯目标及版本。
  - 证据：原版基准为 BigWorld 14.4.1（commit `4657a17cf10d55e2fbebf258f96da96a3dbe0c22`，C++23 + 内嵌 CPython 3.13.16）：`programming/bigworld/server/` 下 8 个角色源码目录 baseapp/baseappmgr/cellapp/cellappmgr/dbapp/dbappmgr/loginapp/reviver（另有 `*_extensions`、`tools` 与 Makefile 构建脚手架），二进制装在 `game/bin/server/el7/server/`，由 bwmachined2 整组拉起、角色间走 Mercury 报文。新版为 `docs/architecture/process-topology.md` + `docs/architecture/compatibility-matrix.md`。**进程拓扑**：单二进制实测（`apps/server/Cargo.toml` `[[bin]] name = "boundless-server"`）；`KNOWN_ROLES` 8 角色与源码目录一一对应（`crates/server-runtime/src/app.rs:9-18`），并有测试 `role_list_covers_the_reference_server_dirs` 把它钉到原版 `server/*/`；未知角色实测报 `unknown server role 'cellap' (expected one of: baseapp, baseappmgr, cellapp, cellappmgr, dbapp, dbappmgr, loginapp, reviver)`；`serverMode` 三值 `standalone`（默认，`DEFAULT_SERVER_MODE` 于 `server_app.rs:35`）/`center`/`periphery`，白名单校验于 `:176`；退出契约逐字引自 `server_app.rs:17-18`（`fini()` → `interface_.prepareForShutdown()` → `"%s has shut down."`），引用特例 `doBWMainT` 失败返回 `-1` 登记于 `:5`；实测退出码 0/1。**兼容矩阵**：基线 commit、版本、`rust-toolchain.toml` 1.100.0、`Cargo.toml` edition 2024、release profile `opt-level = 3` + `debug = true` 全部实测；平台表 7 项平台接口（**BW-15-01-01..07**）中仅 **BW-15-01-02** 单调时钟已实现（`crates/platform/src/clock.rs`），Linux/Windows/macOS 三端对应 **BW-15-02/03/04**；后端表 MySQL/PostgreSQL/XML/SQLite 四后端均未实现，id 词汇实测在 `crates/foundation/src/ids.rs:41,44`（`DatabaseId = i64`、`PENDING_DATABASE_ID = -1`）；客户端与引擎表对应 **BW-21-02-05 / BW-21-03 / BW-21-04 / BW-21-05 / BW-14-02**；跨版本三规则（协议/脚本 API/资源路径）各引源；工作区边界表实测列各 crate 公开模块（`resources` 实为 10 个：`base64`、`binary`、`bwresource`、`data_section_cache`、`datasection`、`file_system`、`multi_file_system`、`packed`、`special_chars`、`zip`；`contracts` 3 行、`python-host` 4 行均为占位——收口时 `contracts` 长出 `machined`（581 行）、`python-host` 长出六模块（约 3211 行），均不再是占位；`foundation/math` 含 angle/boundbox/matrix/quaternion/vector；`server-runtime` 9 个模块）；根 `Cargo.toml` 工作区依赖实测启用 `foundation`/`resources`/`server-runtime`/`libc`，其余 5 个 crate 注释待首消费者——收口时启用集扩到 `foundation`/`platform`/`resources`/`python-host`/`server-runtime`/`libc`，仍注释的只剩 `mercury`/`contracts`/`entitydef`/`test-support`。写页过程据源码更正两处：①工作区边界表原记 `resources` 仅 2 模块，实为 10 个公开模块；②拓扑页原称错误消息「含最接近的正确拼写」，实测消息只列出全集、无最接近拼写算法（测试 `msg.contains("cellapp")` 仅因 cellapp 出现于列表），已更正为「回显拼错的名字并列出全集」。本次提交同时恢复 BW-19-02-03 的前向链接（`standalone-server` → `/architecture/process-topology`）并新增 `architecture/index.md` 与导航/侧栏/首页行；BW-19-02-05 的 `engine-integration` → `/architecture/compatibility-matrix` 链接于同批恢复。诚实登记：`contracts` 为空，故拓扑图是「已部署未接线」——角色间报文、服务发现、Mercury 连接层均未实现。收口时 `contracts::machined` 已给出 bwmachined 线格式（`Header`+`ProcessMessage`/`ListenerMessage`/`SignalMessage` 及 `write`/`read`），但根 `[workspace.dependencies]` 仍注释 `contracts`、**无任何 crate 消费**，故拓扑页结论保持「已部署未接线」，仅把该行改为「类型已到位、无消费者」。`pnpm --dir docs run build` 绿 **5.87s**，checker **45 链接 0 dead**。
- [x] **BW-19-03-02 维护配置、CLI 和 API 参考**。**验收** 条目链接到可执行示例。
  - 证据：原版基准为 `lib/server/bwconfig.cpp`（`bw_<username>.xml` 经 `getpwuid`、`parentFile` 链、`chainTail_.front()` 取值）与 `lib/server/bwservice.hpp:153` 的 `BIGWORLD_MAIN` 宏（`BWResource::init`/`BWConfig::init` 返回值被丢弃——缺配置仍按默认值跑；唯一把 `BWConfig::isBad()` 当早退的是 `server/tools/consolidate_dbs/main.cpp:48-51`，且返回的是 `EXIT_SUCCESS`）；新版为 `docs/reference/configuration.md` + `cli.md` + `api.md` 三页。**配置页**：链语义 5 条与源码逐条对上；`+key value` 写入链首、尾随无值 `+key` 被忽略（`config.rs` 注释明记 `i < argc - 1`）、空链报 `No configuration section.`（实测 `config.rs:155`）；`ServerAppConfig` 10 项选项表与 `server_app.rs:88-101` 初值逐项一致（gameUpdateHertz=10、personality=BWPersonality、serverMode=standalone、production=true、timeSyncPeriod=60、useDefaultSpace=false、maxOpenFileDescriptors=-1、maxSharedDataValueSize=10240、maxMgrRegisterStagger=0.0、numStartupRetries=60）+ 派生项 `time_sync_period_in_ticks`（测试断言 600）；资源路径三级优先与 `BW_RES_PATH_SEPARATOR=':'`、`BW_CACHE_SIZE` 实测于 `bwresource.rs:29,195,397`；样例即本站静态资源 `/examples/bw.standalone.xml`（明示仓内无 `config/` 目录）。**CLI 页**：单二进制、8 角色、`args[1]` 语义、退出码表（含 `doBWMainT` 的 `-1` 是配置级信号、进程码报 1 的辨析）、`--res`/`-r`/`-uid` 与 `compressArgs` 压缩尾实测于 `bwresource.rs:192-229`，规划入口 `apps/{client,editors,tools,bots}` 引自 todo.md 目标目录树一节（行号随合并漂移，收口时位于 1142-1149 行）并明示尚不存在。**API 页**：逐 crate 公开项列表，每 crate 附 `cargo test -p <crate>` 可执行示例，页尾 `cargo test --workspace` 实测 **17 个测试二进制 474 通过 0 失败**（收口时 22 个测试二进制 **738 通过 0 失败 33 ignored**，exit 0）；条目与源码逐项核对，其中两处据源码更正：①`Stamp` 实为 `foundation::time::Stamp`（platform 仅是消费者，非自有类型）；②原稿误写 `shutdown::TaskManager`——`TaskManager` 在 `task_manager.rs`（`shutdown` 模块对应项是 `ShutdownState`），已拆行更正；mercury 九个 flag 位（`HAS_REQUESTS…HAS_CUMULATIVE_ACK`）、`REPLY_MESSAGE_ID=0xFF`、小端序、`entitydef` 注册表 API、`EXIT_SUCCESS=0`/`EXIT_FAILURE=1`（`server_app.rs:40-42`）均逐一实测。验收「条目链接到可执行示例」：API 页每节一个 `cargo test` 块，配置/CLI 页命令均在本树实跑通过。本次提交新增 `reference/index.md` 与 Reference 导航/侧栏/首页行。诚实登记：`contracts` 与 `python-host` 无公开项（占位 crate），API 页以 info 块如实声明而非留白。收口时两 crate 均已长公开面（`contracts::machined` 581 行、`python-host` 六模块约 3211 行），API 页相应换成 `## contracts` / `## python-host` 两张公开项表，info 块改为「每个 crate 的 `pub` 项与源码逐一对齐」，并登记 `python-host` 自带 `capi` 手写 CPython ABI、**不引 pyo3 等原生引擎依赖**。`pnpm --dir docs run build` 绿 **7.22s**，checker 提交树口径 **53 链接 0 dead**（工作树当时含未提交的 operations 三页为 56；`api.md` 页尾对 `/operations/testing-and-release` 的前向链接在本条降级为普通文字、随 BW-19-03-05 恢复，以逐提交归档实测全绿：`git archive` 抽出本提交 docs 后 checker+build 双绿，5.80s）。
- [x] **BW-19-03-03 编写数据库迁移和恢复说明**。**验收** 样本恢复步骤可执行。
  - 证据：原版基准为 `server/tools/` 下六个存储运维工具 `sync_db`、`consolidate_dbs`、`transfer_db`（含 snapshot/consolidate 两子模块）、`snapshot_helper`、`clear_auto_load`、`remove_db`（实测目录全在），`sync_db/main.cpp` 第 3 行即 `#include "mysql_synchronise.hpp"`——MySQL 是迁移源端；新版为 `docs/operations/database.md`，每个步骤一行一个 `todo.md` id：schema 三步（**BW-09-07-01** 空库初始化 / **-02** dry-run「数据库不变且报告拟执行语句」/ **-03** 旧样本升级后可读）、数据三步（**BW-09-07-05** 导入对照 / **-06** DBID 校准 / **-07** 失败重跑不重复导入）、恢复一步（**BW-09-07-04** 空目标库恢复相同记录，验收「样本恢复步骤可执行」以此为准并回指 DBID 序列必须随备份携带 **BW-09-06-04**），写正确性五条（**BW-09-06-06** 提交可从另一连接读回 / **-07** 回滚读旧值 / **-08** 并发冲突契约 / **-09** 断链恢复 / **-10** 重登与 MySQL 基准相符），并回指 **BW-09-06-01**（启动报告后端）；工具名逐个列出并标注其 Boundless 等价物归 **BW-09-05-01..06**。四后端表 XML/SQLite=**BW-09-03**、MySQL=**BW-09-02**、PostgreSQL=**BW-09-06/07**，与 todo.md BW-09 节逐条对上（行号随合并漂移，收口时 648-730 行）。诚实登记以 danger 块开头：「本页今天没有任何一步可跑」——仓内实测无 SQL 依赖、无 `.sql`、无迁移目录，唯一持久化原语是 `DatabaseId = i64` 与 `PENDING_DATABASE_ID = -1`（`crates/foundation/src/ids.rs:41,44` 实测），页尾给出今日唯一可用的 id 约定代码块并警示 `-1` 不得作为真实 id 写库。本次提交新增 `operations/index.md`、Operations 导航/侧栏/首页行，并恢复 BW-19-03-01 预留的 `compatibility-matrix` → `/operations/database` 链接。`pnpm --dir docs run build` 绿 **8.93s**，checker 提交树口径 **61 链接 0 dead**（归档实测 5.63s）。
- [x] **BW-19-03-04 编写 Bots 压测说明**。**验收** 示例能产出报告。
  - 证据：原版基准为 `server/tools/bots/`，关键源文件实测在位：`bot_entity.cpp`（BOT 实体）、`beeline_controller.cpp`（固定路线）、`add_bots_callable_watcher.cpp`（Watcher 可调用控制）、`bots_config.cpp`（配置），BOT 实体注释并明记属性可见性语义（`BASE_AND_CLIENT` 随 base 初始化下发、`OWN_CLIENT/OTHER_CLIENT/ALL_CLIENT` 随 cell 下发）；新版为 `docs/operations/load-testing.md`，三段逐条挂 `todo.md` id：运行时六项（**BW-17-01-01..06**：客户端连接层/批量增减与标签过滤/Python 行为入口/beeline/巡逻路径/Watcher 控制）、场景八项（**BW-17-02-01..08**：固定种子+账号池+并发的输入定义、爬升/预热/时长执行器、登录洪峰/稳态移动/密集 AOI/跨 Cell/断线重连/长稳六场景）、报告四项（**BW-17-03-01..04**：版本+拓扑+种子可追溯、登录成功率+在线数、p95/p99 延迟与超时、带宽与资源使用）及回归分工（短场景进 PR smoke=**BW-17-03-05**，容量/长稳进定时或手动=**BW-17-03-06**，阈值取自已记录基线）——与 todo.md BW-17 节逐条对上（行号随合并漂移，收口时 1235-1273 行）。目标布局 `apps/bots/` 与 `tests/load/scenarios/` 引自 todo.md 目标目录树与 BW-17-02 节（行号随合并漂移，收口时 1149、1250 行）。验收「示例能产出报告」如实登记为**目标**而非现状：以 warning 块开头声明仓内无 `apps/bots/`、无 `tests/load/`、无任何 bot 代码，「今天能跑的」一节只有 `cargo test -p server-runtime` 与 `update_hertz` 默认 10（`DEFAULT_GAME_UPDATE_HERTZ`，`server_app.rs:30`）这一条可复现事实；报告表即未来示例产出物的字段契约（版本/拓扑/种子、成功率/在线数、p95/p99）。本次提交另把 `__pycache__/` 加入 `docs/.gitignore`（checker 运行残留），并新增侧栏 Load 组与 operations 首页条目。`pnpm --dir docs run build` 绿 **5.67s**，checker 提交树口径 **64 链接 0 dead**（归档实测 6.56s）。
- [x] **BW-19-03-05 编写测试和发布说明**。**验收** 本地可复跑对应 CI 步骤。
  - 证据：原版基准为参考仓 `.github/workflows/`、`scripts/package-daily*`、`scripts/check-docs-links.py` 与 `TESTING.md`（实测在 `BigWorld/TESTING.md`）；新版为 `docs/operations/testing-and-release.md`，分四段。「本地今天可复跑」：工具链（`cat rust-toolchain.toml` → `channel = "1.100.0"`，`rustup show active-toolchain` 实测输出 `1.100.0-x86_64-unknown-linux-gnu (overridden by …/rust-toolchain.toml)`，对应 **BW-00-03-02**）；构建与测试（`cargo build -p boundless-server`、`cargo test --workspace` 实测 **17 个测试二进制 474 通过 0 失败**（收口时 22 个测试二进制 **738 通过 0 失败 33 ignored**，exit 0）、并实测仓内无 `tests/` 目录与集成夹具，内部测试框架=**BW-18**）；文档门禁（`pnpm run check:links` + `pnpm run build` 双门，`ignoreDeadLinks: false`，CI 入口 `pnpm --dir docs run build`，checker 语义一段完整描述——绝对链接以 docs 根为根、页面目标 `.md`/`index.md`/`.html`、资源目标须在 `docs/public/`、退出码 0/1）。规划 CI（**BW-22**）四组表：构建与静态检查 **BW-22-01-01..05**（钉工具链与锁文件/Rust format+lint+build/Python/TypeScript/变更路径条件）、测试矩阵 **BW-22-02-01..06**（三端 runner/MySQL+PostgreSQL/XML+SQLite/兼容差分/SDK 与插件/归档覆盖率日志夹具）、压测回归 **BW-22-03-01..04**（PR 短压测/定时容量长稳/手动故障/对比报告——版本+硬件+场景+DB 齐全）、发布物 **BW-22-04-01..05**（三端安装包/文档与版本化索引/各引擎 SDK/VSIX/元数据与散列防篡改）——与 todo.md 1425-1459 行逐条对上，并点出两条设计性质（路径条件=**BW-22-01-05**、每作业本地可复跑=**BW-22-02-06**）。压测回归段回指 `/operations/load-testing`。**发布段遵守版本红线**：明写「本树没有发布流水线、也没有产出过任何发布；版本号人工指定，本站不 bump 版本、不打 tag、不发布产物」，产物清单标注为 BW-22-04 的目标而非承诺，并写明 BW-14 与 BW-22-04 落地前唯一可交付物是 `cargo build -p boundless-server` 的二进制。验收「本地可复跑对应 CI 步骤」成立：页面所有命令均在本树实跑通过（`.github/`、`ci/`、Makefile、justfile 均实测不存在，页首如实声明）。本次提交恢复三处前向链接（首页 Building this site 段、guide 首页 Verifying 段、api.md 页尾均 → `/operations/testing-and-release`），补全 Operations 侧栏 Release 组、operations 首页条目与首页行。`pnpm --dir docs run build` 绿 **5.55s**，checker 提交树口径 **70 链接 0 dead**（含首页「Reproduce the build gates locally」行，归档实测 6.00s）。

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
- [x] **BW-20-02-06 验证 VS Code 的 Rust 启动调试**。**验收** 能由启动配置运行 CellApp 并命中断点。
  - 证据：原版结果——引用端角色进程是 C++ `bigworld_app <role>`，VS Code 调试需手配 launch.json 指向其二进制。新版结果——`src/workspace/launchConfig.ts` 的 `rustLaunchConfig(role)` 生成启动配置：`type` 按平台取 `lldb`（Linux/macOS）或 `cppvsdbg`（Windows），`program` 指向 `target/debug/boundless-server`（`apps/server` 的产物，`apps/server/Cargo.toml` [[bin]]），`args=[role]`，`env.BW_RES_PATH=<workspaceFolder>/res`，`stopAtEntry:false`；`extension.ts` 的 `boundless.setupRustDebug` 命令经同一个 `mergeLaunchJson` 并入工作区 `launch.json`，且把 `boundless.cluster.binary` 的默认值从错误的 `target/debug/server` 修正为实际产物名。证据链接——`pnpm test` 149 例全绿且 `pnpm run check` 0 错误（含 `rustLaunchConfig` 形状、Windows 调试器选择、覆盖 program/resPath、与附加配置并存的 4 例）；实跑 `sh tools/verify/rustLaunch.sh`：gdb batch 以 launch 配置同一命令行（`target/debug/boundless-server cellapp`、`BW_RES_PATH` 指向示例 res）启动 CellApp，断点命中 `server_runtime::app::run`（`crates/server-runtime/src/app.rs:46`），回溯顶两帧为 `app::run ← boundless_server::main`，脚本判 PASS。
- [x] **BW-20-02-07 提供文档 MCP 接入配置**。**验收** 查询返回当前工作区版本文档。
  - 证据：原版结果——引用端的文档是仓库里的 markdown（VitePress 渲染），编辑器侧没有任何检索入口，开发者只能翻目录或开网页版；引用端不存在 MCP 面。新版结果——`extensions/vscode/src/workspace/docsMcp.ts` 生成 `.vscode/mcp.json` 条目：`type: stdio`、`command` 指向 `boundless-docs-mcp`、`args` 依次为 `--root ${workspaceFolder}`、`--prefix docs`、`--version HEAD`、`--langs zh`，配置了站点地址才追加 `--docs-url`（`DOCS_MCP_DEFAULTS` 与 `boundless-docs-mcp` 自己的默认值一致）；`mergeMcpJson` 与 launch.json 同一"只增不改"面——只替换自己写的那一项、用户自己的 server 原样保留，解析不出来的文件整体重写；`extension.ts` 的 `boundless.setupDocsMcp` 命令按工作区文件夹逐个写入 `.vscode/mcp.json`，新增 6 个设置 `boundless.docs.{server,root,version,prefix,langs,url}`。证据链接——`pnpm test` 160 例全绿（含新 `test/docsMcp.test.ts` 11 例）、`pnpm run check` 0 错误；实跑 `node tools/verify/docsMcp.cjs` 11/11 PASS：harness 用扩展自己的 writer 写出真实 `.vscode/mcp.json`，再按该文件里的 command/args 起真二进制（`${workspaceFolder}` 按编辑器变量语义替换）做 MCP stdio 会话——`tools/list` 只列 `search_docs`/`read_doc`；`search_docs "second edition"` 命中仅 v2 才存在的 `guide/second.md` 且 `version` 为配置给的 `HEAD`；`read_doc guide/second.md` 返回 `version=HEAD`、`commit=2b8be3d` 恰等于工作区当前 HEAD、`source=https://docs.boundless.example/guide/second.html`；同一配置把 `boundless.docs.version` 改成 `v1` 后，v2 页面零命中、`guide/entities.md` 回到 v1 文案、链接保持仓库相对 `docs/guide/entities.html`。如实登记——编辑器内的 MCP 客户端加载（自动发现 `.vscode/mcp.json`）未在真实 VS Code 里验证：本机 `/usr/bin/code` 1.144.1 可装但不跑带 GUI 的 MCP 会话，取证止于"按其写出的配置实跑真二进制并逐条核对查询结果"。


#### BW-20-03 打包与三端验证

提供 VSIX、扩展宿主集成测试及安装说明，在 Linux、Windows、macOS 用 Demo 工作区检查核心流程。源码和发布包在插件目录独立构建。

- [x] **BW-20-03-01 独立构建 VSIX**。**验收** 可在干净编辑器安装。
  - 证据：原版结果——引用端是 C++ 服务器加 Python 脚本，编辑器侧没有 VS Code 扩展包，要用编辑器只能手配 launch.json 并自起进程。新版结果——`extensions/vscode` 独立成包：`pnpm run package` 调 `vsce package --no-dependencies` 产出 `boundless-vscode-0.0.1.vsix`；新增 `.vscodeignore` 把 `src/`、`test/`、`tools/`、`tsconfig*.json`、`vitest.config.ts`、`pnpm-lock.yaml`、`pnpm-workspace.yaml`、`out/**/*.map` 挡在包外（88 文件 168.83 KB → 24 文件 42.4 KB，包内 22 个文件）；`tools/verify/vsix.cjs` 只用扩展目录自带的 `node_modules/.bin/vsce` 打包（不依赖全局工具），解包后逐条核对：manifest 存在、`main` 指向的 `out/extension.js` 在包内、语言/片段/语法齐全、入口加载的 `out/workspace/{docsMcp,launchConfig,cluster}.js` 齐全、不含 src/test/开发清单、不含指向未随包源码的 .map。证据链接——`pnpm run verify` 输出 `vsix: all checks passed`（8 项全 PASS）；干净编辑器实装：`code --user-data-dir <新> --extensions-dir <新> --install-extension boundless-vscode-0.0.1.vsix` 返回 `Extension 'boundless-vscode-0.0.1.vsix' was successfully installed`，`--list-extensions --show-versions` 列出 `boundless.boundless-vscode@0.0.1`，扩展目录内解出 `out/ package.json readme.md snippets syntaxes language-configuration.json`。如实登记——包只在本地构建：`package.json` 仍是 `private: true`、版本仍 0.0.1，未打 tag、未建 GitHub Release、未发 marketplace（发版由用户手动触发）。
- [x] **BW-20-03-02 编写安装和配置说明**。**验收** 能从空工作区启用。
  - 证据：原版结果——引用端编辑器是独立 C++ 程序，说明散在 `programming/bigworld/` 与发行版帮助里，没有"从空工作区启用编辑器支持"的入口。新版结果——`extensions/vscode/README.md`：安装（`pnpm run package` → `code --install-extension`）、构建链（`pnpm install` / `build` / `check` / `test` / `verify`）、"Enable it in an empty workspace"（资源根即持有 `scripts/entities.xml` 的目录；空目录会明确报 `No Boundless resource root found.`，这是"没有可绑定的项目"而非失败）、命令表 10 条、`boundless.cluster.*` 5 项与 `boundless.docs.*` 6 项设置表（含默认值）、文档服务器的一次构建步骤与写出的 `mcp.json` 示例、`pnpm run verify` 的覆盖范围。证据链接——README 里每一步都在本机实跑取证：打包与包内容核对见 `vsix.cjs`（`vsix: all checks passed`）、干净 profile 实装见 BW-20-03-01 一行、`mcp.json` 示例逐字取自 `host.cjs` 写出的文件、命令与设置表逐项对照 `package.json` 的 `contributes.commands` 与 `contributes.configuration.properties` 默认值；README 随包发布，安装后在扩展目录里可见 `readme.md`。如实登记——说明只写了"本地构建 + VSIX 安装"这一条途径，因为包未发 marketplace，没有 marketplace 安装路径可写；示例输出（工作区报告、`mcp.json`）取自实跑结果而非手写。
- [x] **BW-20-03-03 验证三端扩展宿主**。**验收** Demo 工作区核心动作均通过。
  - 证据：原版结果——引用端没有 VS Code 扩展，"三端扩展宿主"对它不适用（其编辑器是独立 C++ 程序）。新版结果——Linux 端跑真实宿主：干净 profile 实装成功且 `--list-extensions` 列出 `boundless.boundless-vscode@0.0.1`；再用 Xvfb 起真实窗口，`window1/exthost/exthost.log` 的 `initData.allExtensions` 含 `boundless.boundless-vscode`，渲染器日志有 `pickRunningLocation for boundless.boundless-vscode … => LocalProcess`，即扩展确实被真实扩展宿主加载进真实编辑器。Demo 工作区核心动作由 `tools/verify/host.cjs` 在包内产物上验证：解包 VSIX 后直接 `require` 包里的 `out/extension.js`，用与编辑器同形的 API 面（设置默认值从 manifest 读，行为与真实编辑器一致）驱动一个真磁盘 Demo 工作区，15 项全过——manifest 贡献的命令全部注册、输出通道建立、`boundless.setupDocsMcp` 写出 `.vscode/mcp.json`（stdio 条目、`--root ${workspaceFolder}`、`--version HEAD`）、Python/Rust 配置命令写出 4 条 `launch.json` 配置且本平台调试器为 `lldb`、工作区报告定位到 res 根、`.def` 诊断命中 `ON_NO_CELLS` 而干净文档零诊断、方法元素跳到真实 `scripts/base/Clean.py` → `host: all checks passed on linux`。win32/macOS 无法在本机起真编辑器，改为在取分支的代码处注入平台：`processRunner(platform = process.platform)`（与既有 `rustDebugger(platform)` 同一惯用法），`test/platformHost.test.ts` 6 例覆盖 Linux/macOS 起进程 `detached: true`、Windows `detached: false`，Windows 停止走 `taskkill /pid <pid> /t /f`，Linux/macOS 停止对进程组发 `-pid SIGTERM` 后 `SIGKILL`。证据链接——`pnpm run verify` 末尾 `host: all checks passed on linux`；`npx vitest run test/platformHost.test.ts` 6/6；`pnpm test` 166 例全绿、`pnpm run check` 0 错误。如实登记——win32 与 macOS 的**真实编辑器宿主**未验证（本机无 Windows/macOS 机器，容器内 VS Code 起窗口需 Xvfb 且命令级 `vscode://` 转发未能送达运行中的实例），这两端只到"平台分支在注入平台下被单元取证 + 打包产物在 Linux 真机端到端通过"；命令级证据来自动态 API 面驱动包内产物，而非在编辑器 UI 里点命令。

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

- [x] **BW-22-01-01 固定 CI 工具链和锁文件**。**验收** 本地解析结果一致。
  - 原版：BigWorld `.github/workflows/{ci,daily-build}.yml` 只钉 vcpkg `builtin-baseline` 与系统 `gcc-14`，无 Rust 侧约束（原版不含 Rust）。
  - 新版：钉位沿用 BW-00-03-02 的 `rust-toolchain.toml` channel=1.100.0（未重做），新增 `ci/toolchain-check.sh` 断言 rustup 依该文件解析出的 `rustc`/`cargo` 等于 pin、`Cargo.lock` 已入库且未被 gitignore、`cargo metadata --locked` 一致，并把解析结果写成 `ci-out/toolchain.json`。
  - 结果：`ci/toolchain-check.sh --offline` 退出码 0，报 pinned=1.100.0 / resolved=1.100.0 / members=9 / offline build resolved；负例 `RUSTUP_TOOLCHAIN=1.99.0` 退出码 1，报 `resolved 1.99.0 but rust-toolchain.toml pins 1.100.0`。
  - 证据：`ci/toolchain-check.sh`、`ci-out/toolchain.json`（pinned、resolved、members、cargo_lock_tracked、offline、checked_at、git_sha=0cd68ff）；CI 作业 `toolchain` 见 `.github/workflows/ci.yml`（run 号见 BW-22 交付汇总）。
- [x] **BW-22-01-02 接入 Rust 格式、lint 与构建**。**验收** 各有可复跑入口。
  - 原版：BigWorld `ci.yml` 只跑 `ctest` + vcpkg baseline，`daily-build.yml` 跑 `make bw-run-all-unit-tests`，无 cargo fmt/clippy 门禁。
  - 新版：`ci/check-rust.sh {fmt|lint|build|test|all|self-test}` 单入口，`--packages a,b` 作用域；CI 作业 `rust-static` / `rust-build-test` 调同一入口。
  - 结果：build PASS（0 警告）、test PASS（502 用例）；fmt FAIL 284 处（foundation 17 / mercury 23 / platform 18 / resources 129 / server-runtime 97）、lint FAIL（deny 级 foundation/src/math/quaternion.rs:64:9、foundation/src/time.rs:6:1；resources/src/zip.rs:1140 因依赖 crate 先失败未及编译）——属各 crate 所属 lane，不属 CI 面。
  - 证据：失败定位到 `path:line:col`（manifest.tsv + logs/rust-<lane>.log）；`ci/check-rust.sh self-test` 5 项全 PASS：fmt/lint/build/test 各能咬住注入缺陷、clean 夹具放行。
- [x] **BW-22-01-03 接入 Python 检查**。**验收** 失败定位模块。
  - 原版：BigWorld `scripts/package-daily.sh` 只做打包，Python 侧由 `make bw-run-all-unit-tests`（C++ 单测）带过，无 pytest 门禁。
  - 新版：`ci/check-python.sh` 提供 compile / test 两条 lane（compileall + pytest），pytest 退出码 4（用法错）与 5（未收集到用例）单独解释；CI 作业 `python`。
  - 结果：当前仓 tests/python、scripts、python、tools 均无 Python，两 lane SKIP 并注明由 BW-18 开启；注入 `tests/python/scratch_probe.py`（语法错误）后 python-compile FAIL 定位 `tests/python/scratch_probe.py:1`、python-test 报 no tests collected 并指向 compile lane；注入失败断言后 python-test FAIL 定位 `tests/python/test_bad.py::test_fails`。
  - 证据：`ci-out/manifest.tsv` 上述三行 + `logs/python-{compile,test}.log`；`ci/check-python.sh self-test` 3 项全 PASS（syntax defect 定位到 broken_module.py:1、failing test 检出、clean 树放行）。
- [x] **BW-22-01-04 接入 TypeScript 检查**。**验收** 失败定位扩展源码。
  - 原版：BigWorld 无 TypeScript 门禁，`ci.yml` 只跑 `ctest`，`daily-build.yml` 只打包。
  - 新版：`ci/check-typescript.sh {all|self-test}` 遍历树内每个 `tsconfig*.json`（tracked 与未提交都扫，按 basename 过滤而非 git pathspec——`tsconfig*.json` 作 pathspec 匹配不到嵌套路径），逐个 `tsc --noEmit -p` 并跑该包自带 `test` 脚本；CI 作业 `typescript` 调同一入口。
  - 结果：BW-20 的 `extensions/vscode/` 已到 main，lane 真跑而非 SKIP——发现 2 个 tsconfig（`tsconfig.json` 与 `tsconfig.test.json`）均 `ts-typecheck` PASS，`ts-test` 跑包自带 `vitest run` 共 **149 用例全通过**；注入 `src/bw4_probe_bad.ts` 后 FAIL 定位 `extensions/vscode/src/bw4_probe_bad.ts(2, 5): error TS2322`（用项目自带 tsc 5.7.3 而非机器全局），删掉即转绿；无行列坐标的 TS18003 也给出具体信息而非空白 detail。
  - 关键修正：无 `node_modules` 时 lane 报出 **87 个**错误且全是缺依赖级联（TS2591 `Cannot find name 'Buffer'` ×45、TS2307 `Cannot find module 'vscode'` ×15、TS7006 隐式 any ×13、TS2304 `setTimeout`、TS2503 `NodeJS` 命名空间），装依赖后归零——所以 lane 先按项目锁文件装 devDeps（`pnpm-lock.yaml` → `pnpm install --frozen-lockfile --prefer-offline`，否则 `npm ci` / `npm install`），安装是否成功以「项目自带 tsc 是否就位」为准（pnpm 会把 esbuild/keytar 的 postinstall 跳过并 exit 非零，而 tsc 已可用）；安装失败的 lane SKIP 并注明日志路径，不拿缺依赖产生的类型错误冒充门禁结果。lane 名与日志名按目录扁平化（`extensions/vscode` → `extensions-vscode`），否则嵌套项目的日志写在含斜杠的路径下、失败只剩空白 detail。
  - 证据：`ci-out/manifest.tsv` ts-typecheck/ts-test 各 PASS×2 + selftest-ts / selftest-ts-clean PASS；`ci/check-typescript.sh self-test` 退出码 0。
- [x] **BW-22-01-05 配置变更路径条件**。**验收** 文档和代码改动触发对应作业。
  - 原版：BigWorld `.github/workflows/ci.yml` 与 `daily-build.yml` 全量作业无条件触发，不按改动路径选择检查。
  - 新版：`ci/changes.sh {--machine} [base] [head]` 把改动路径映射为 lanes {ci,toolchain,rust,python,typescript,docs}：`ci/` 与 `.github/workflows/` 触发全部 lane（harness 自检随之跑）；根 `Cargo.toml`/`Cargo.lock`/`rust-toolchain.toml` 置 `rust_all=1`；crate 改动按各自 Cargo.toml 的 package name 归入 `rust_packages`（`apps/server` 解析为 `boundless-server`）；markdown/docs → docs。`.github/workflows/ci.yml` 每个作业用 `contains(lanes, …)` + `rust_packages`/`rust_all` 门控，`harness` 作业在 `ci/` 变更时跑全部四条 self-test。
  - 结果：scratch 仓 11 例全对——markdown→docs、`tests/python`→python、tsconfig→typescript、crate→`rust_packages=foundation`、无 manifest 的 crate→`rust_all=1` 兜底（否则两个 rust 作业会静默跳过，此为本次修掉的缺陷）、根 Cargo.toml→graph-wide、`ci/`→全 lane、crate+docs 混合→`rust,docs` + `boundless-resources`、两个 crate→`foundation,boundless-resources`、clean→仅 toolchain；死链 FAIL detail 由「1 dead」改为直接给出 `index.md -> no-such-page.md`。另修 push 事件基准缺陷：原实现一律取 `origin/main` tip 为基准，分叉久的分支会把他人推入 crate 的改动全算成本分支 diff（实测报 `rust_packages=mercury,platform`、117 个文件假象），改为 push 事件用 `git merge-base origin/main HEAD` 作基准（PR 仍用 `origin/$base_ref`），修正后本分支 diff 只含 6 个 CI 文件、`rust_packages=` 空。本地等价 7 作业序列 exit=0，manifest 25 行 0 FAIL。
  - 证据：`ci-out/changes.tsv` + 上表 11 例 + `ci-out/manifest.tsv`（toolchain PASS、rust×4 与 python×2 SKIP 并注明开启项、ts-typecheck/ts-test PASS、docs-links PASS、四组 selftest 全 PASS）；作业定义见 `.github/workflows/ci.yml`（run 号见下方交付汇总）。

本组 run 号与结论汇总在 BW-22-02 末尾的「BW-22 交付汇总」一张表里（同一 workflow 面被逐条改造，表按 run 排列）。

门禁要点：三条 lane 在本席位可复跑（`ci/toolchain-check.sh --offline` 冷缓存 exit 0；`ci/check-typescript.sh` 对 `extensions/vscode` 真门禁并通过 149 个 vitest 用例；`ci/check-docs.sh` 47 链接 0 死）。Rust 侧 `cargo fmt`/`clippy` 在全仓口径仍红，属各 crate 所属席位的历史债务，不属 CI 面；rebase 到 `a2c0f66` 后实测 **211 处 fmt**（resources 104 / mercury 32 / server-runtime 34 / platform 24 / foundation 17）与 **32 处 clippy**（全部落在 `crates/resources` 的 `bwresource.rs` / `packed.rs` / `zip.rs`）——债务在席位间随合并移动，CI 面只负责把位置打到 `path:line:col`。

#### BW-22-02 测试矩阵与服务装配

编排三端原生 runner、真实 MySQL/PostgreSQL 后端、XML/SQLite、原版兼容差分和 SDK/插件测试。每套任务可在本地复跑，归档测试结果、覆盖率、服务日志及失败夹具。

- [x] **BW-22-02-01 配置三端 runner**。**验收** 每端产出启动测试记录。
  - 原版：BigWorld 的 OS 差异是**两份独立工作流**——`ci.yml`（`ubuntu-24.04`）与 `ci-windows.yml`（`windows-latest`），同一门禁逻辑复制两遍；全仓**没有任何 macos runner**。且没有「每端启动测试记录」这回事：作业跑 `ctest`，失败时靠 `if: failure()` 上传 tee 出来的 `.log` 散文件。
  - 新版：单一入口 `ci/runners.sh`，`.github/workflows/ci.yml` 的 `runners` 作业用 `strategy.matrix.os: [ubuntu-latest, windows-latest, macos-latest]` 三腿跑**同一个**入口（对应 BW-15-02/03/04 的三个首批目标）。每腿把角色列表从 `crates/server-runtime/src/app.rs` 的 `KNOWN_ROLES` **读出来**（不是硬编码，加角色即自动点亮），逐个 `boundless-server <role>` 起来并记录启动行与退出码，另跑一次未知角色做负例；产出 `ci-out/runners/<os>-<arch>.tsv`（os / arch / rustc / toolchain_resolved / git_sha / version / binary + 每角色一行结果）与机器可读 `runners/<os>-<arch>.json`。
  - 结果：Linux 腿真跑 **8/8 角色全部起来**（`[baseapp] starting (boundless minimal entry)` 等，退出码全 0），未知角色 `not-a-real-role` 被拒且回显合法集合，json 报 `roles_total=8 roles_ok=8 unknown_role_refused=yes`。`self-test` 2 项全 PASS：坏角色会 FAIL 且**在记录里点名**（`badrole FAIL`）、干净 stub 全过；自测用私有 `CI_OUT` 沙盒，不会覆盖同进程里已发生的真实记录。
  - 关键设计：矩阵作业**故意忽略** `vars.CI_RUNNER`——一台自托管 runner 不可能同时是三个操作系统，而这正是矩阵的意义所在。选路上把 `runners` 挂在「runner 真正执行的代码」上（`crates/platform/**`、`apps/server/**`、`crates/server-runtime/**`、`rust-toolchain.toml`、根 `Cargo.toml`/`Cargo.lock`），而不是挂在 `ci/` 上：改 CI 脚本时由 `harness` 作业的 `ci/runners.sh self-test` 覆盖，不必每次都烧三台 runner。另加 `workflow_dispatch.inputs.runners` 手动开关，可强制跑矩阵（新 runner 镜像也靠它烟测）。
  - 三端矩阵已在 CI 真跑（run **44760793199**，`gh workflow run ci.yml -f runners=true`）：**ubuntu-latest success、windows-latest success、macos-latest failure**。前两端证明整个 workspace 在 Windows 上也能编译并把 8 个角色起起来；macOS 腿记录的是真编译错误——`crates/server-runtime/src/signals.rs:517` 用了 `libc::SIGRTMIN()`/`SIGRTMAX()`/`sigwaitinfo`（仅 Linux 有），`error[E0425] cannot find function 'sigwaitinfo' in crate 'libc'`，`could not compile 'server-runtime' (lib) due to 4 previous errors`。这正是 BW-15-04（macOS 未开始）应有的样子，由记录如实报出而非被平均掉。
  - 因此该作业是**报告型而非门控型**：`continue-on-error: true`，理由与翻转条件都写在作业注释里——`docs/architecture/compatibility-matrix.md` 中只有 Linux x86-64 是「已开工」目标，Windows/macOS 未开始；若因 macOS 编译不过就红整条 push，反而会把这个作业真正的产出（三端就绪度记录）藏起来。两端开工后删掉 `continue-on-error` 即可转门控。
  - 附带修掉的调用错误：`ci/runners.sh` 起初写成 `if ! ci_pipe …`，而 `ci_pipe` 的约定是把退出码放进 `$CI_RC` 由调用方判断（`ci/check-rust.sh:79` 等五处皆如此），故原写法会把编译失败当成功——本地一次真实构建没产出可执行文件时暴露，改为 `${CI_RC:-1}` 判断。
  - 证据：`ci-out/runners/linux-x86_64.tsv` 与 `.json`、`logs/runners-linux-x86_64.log`；CI 侧 `ci-results-runners-{ubuntu,windows,macos}-latest-44760793199` 三个 artifact（macOS 那份含 4 条 E0425 原文）；`ci/runners.sh`、`ci/changes.sh`（`runners` lane）、`.github/workflows/ci.yml` 的 `runners` 作业。
- [x] **BW-22-02-02 编排 MySQL 和 PostgreSQL**。**验收** 可隔离起库并执行存储用例。
  - 原版：BigWorld 关于 MySQL 的全部 CI 产物只有一个 `fixwork/programming/bigworld/build/make/test_mysql.sh`，它验证的是「**能否编译一个 MySQL 程序**」——`mysql_config --include` 加一段 `gcc -E -x c++-header` 包住 `#include <mysql/mysql.h>`。`libmysqlclient-dev` 在 `daily-build.yml` 里只是构建依赖。Linux 门 `ci.yml` 与 `ci-windows.yml` **都没有 `services:`、没有 docker、没有任何起库动作**：门禁序列是 configure → build → `ctest --output-on-failure | tee ctest.log` → `Upload logs on failure`。也就是说 CI 从未对真实后端执行过**一条**语句，存储层能不能跑完全不在门内。
  - 新版：`ci/store.sh` 为每个后端**独立起一个私有服务器**——MariaDB 走 `mariadb-install-db` + `mariadbd --skip-networking --socket=<沙盒内>`（只走 Unix socket，不占 TCP 端口，因此镜像上 3306 上跑着什么都无关）；PostgreSQL 走 `initdb` + `pg_ctl -w start`（`-w` 阻塞到 postmaster 应答，所以无需就绪轮询）。二者都在沙盒内用 `boundless_ci_$$` 建私有库、逐用例执行、**收尾 DROP**，并回读 `SHOW DATABASES` / `pg_database` 断言库确已消失，把「零残留」写进记录而不只是口头保证。
  - 用例 `tests/store/common/id_vocabulary.sql` 由**语句本身充当断言**：按 `crates/foundation/src/ids.rs` 钉死的 `DatabaseId = i64` 与 `PENDING_DATABASE_ID = -1`，插入 `-1`、`i64::MIN`、`0`、`i64::MAX` 四个端点。后端若把 id 列收窄成 32 位，就会在这几条插入上直接拒绝——用例只在整段 i64 域都能往返时才绿，不存在「空跑通过」。
  - 结果：本机双腿 PASS，`store-mysql` 与 `store-postgres` 各 `1/1 cases ok, isolated <backend> left nothing behind`，两条记录 `database_dropped=yes`，且 `ps` 实测**残留沙盒 0、残留服务进程 0**。`self-test` 2 项 PASS：负例报 `ERROR 1264 (22003) … Out of range value for column 'id'`，DROP 后无残留。
  - 关键设计一：「**缺工具**」与「**真故障**」必须分开。provisioner 找不到自己的工具时退出码 2，lane 据此记 **SKIP** 并点名缺失件；工具齐了却起不来才是 **FAIL**。干净 runner 上没有数据库服务端，若把这种情况记成 FAIL，等于用一条假红灯把真正的问题盖住。
  - 关键设计二：**同一条代码路径吃两种后端来源**。句柄是「本 runner 私有 socket 路径」或「host:port / 主机名」，mysql 与 postgres 的 helper 各自按句柄形态拼连接参数，本地沙盒与 workflow 的 `services:` 容器走的是同一份逻辑。故 CI 里由 `storage` 作业供 `mysql:8` + `postgres:16`，本地则由脚本自己起——两边都可复跑。
  - 附带修掉的三处：① `ok` 只声明未赋值，`set -u` 下 `ok=$((ok+1))` 直接炸 lane；② 服务进程是在命令替换（子 shell）里 fork 的，pid 随手写死在变量里会随子 shell 一起死掉、服务成孤儿，改为写进沙盒内 pid 文件由 EXIT trap 读；③ bash 在**致命信号**下不执行 EXIT trap，而取消一个 job 发的正是这个信号——`trap 'exit 143' TERM` 等把信号转成普通退出，清理才真的会跑（修前实测残留沙盒目录与存活进程，修后 0 残留）。
  - 附带加固一处（自己抓到自己）：self-test 的 `CREATE DATABASE` 起初硬编码 `--socket=`，在 TCP 句柄下连不上会**静默失败**，随后「未知数据库」的错误被当成「列太窄」的证据，自我测试于是在**错误原因**上绿灯。现改为先断言私有库真存在（不存在就 FAIL「nothing is being tested」），且要求拒绝原因是 `out of range`/`1264`——别的拒绝理由不足以判 PASS。
  - 证据：`ci-out/store/mysql.tsv` 与 `postgres.tsv`（`provisioned` / `database` / `case result` / `database_dropped`）；`ci-out/manifest.tsv`；`ci/store.sh`、`ci/changes.sh`（`store` lane）、`.github/workflows/ci.yml` 的 `storage` 作业（`services: mysql:8, postgres:16`）、`tests/store/common/id_vocabulary.sql`。CI run **44770615698**（push 触发，`completed/success`，`Storage cases on a real backend` success，四条 lane 全 PASS，归档 `4 manifest row(s) round-tripped`；artifact 内 `provisioned=127.0.0.1:3306` 与 `127.0.0.1` 证明 CI 侧走的是 `services:` 容器而非本机起库），详见下方 BW-22 交付汇总。
- [x] **BW-22-02-03 编排 XML 与 SQLite**。**验收** 各有独立结果。
  - 原版：BigWorld 的存储后端是**三个库**——`lib/db_storage/`、`lib/db_storage_mysql/`、`lib/db_storage_xml/`，**没有 sqlite 后端**。XML 后端把 id 写成十进制属性再读回：`lib/db_storage_xml/xml_database.cpp:951` 写 `writeUInt( "id", spaceID )`、`:1002` 读 `readUInt( "id" )`；而这个写入器的签名是 **32 位无符号**（`lib/resmgr/datasection.hpp:637` 与 `datasection.cpp:830` 的 `writeUInt( const BW::StringRef&, unsigned int value )`），64 位版本另立他法（`datasection.hpp:643` 的 `writeInt64(..., int64 value)`）。也就是原版 XML 侧的 id 宽度就是 32 位。而 CI 对这件事**从不校验**：`.github/workflows/` 四份工作流（`ci.yml`/`ci-windows.yml`/`daily-build.yml`/`deploy-docs.yml`）没有 `services:`、没有起库动作；`libsqlite3-dev`、`libmysqlclient-dev` 只以**构建依赖**身份出现（`daily-build.yml:78-80`），sqlite3 在 `daily-build.yml:254` 只是 vcpkg 三元组的解析产物；`bwengine_mysql/xml` 仅在 `ci-windows.yml:355` 的一句注释里被提及。
  - 新版：`ci/store.sh` 新增两条腿，与 mysql/postgres 共用同一副骨架（起私有库 → 逐用例执行 → 收尾删除 → 回读断言零残留），但**各写各的结果文件**。SQLite 腿：`sqlite3 -bail` 对沙盒内的私有文件执行用例——`-bail` 是必需的，不加它 sqlite3 会**越过失败的语句继续跑**，并给一个只执行了一半的用例报成功；该文件就是全部服务端，没有进程可泄漏。XML 腿：`python3 -c` + 标准库 `xml.etree.ElementTree`（无外部依赖），遍历所有元素的属性、认 `id`/`entityid`，要求十进制可解析、**至少一个** id（没有 id 的文件否则空跑通过）、且每个 id 落在 `crates/foundation/src/ids.rs` 钉死的 `DatabaseId = i64` 域内；校验输出 `<case>.ids`（每行一个十进制 id），既是证据也是留档。
  - 用例：`tests/store/xml/id_vocabulary.xml` 与 SQL 用例共用同一套 i64 词汇——`id="-1"`（`PENDING_DATABASE_ID`）、`"0"`、`"9223372036854775807"`（`i64::MAX`）、`"-9223372036854775808"`（`i64::MIN`），即专打上面那段 32 位宽度。
  - 结果：四腿全 PASS，四条**独立**记录 `ci-out/store/{mysql,postgres,sqlite,xml}.tsv`，`provisioned` 形态各不相同（mysql = 私有 socket 路径、postgres = pg run 目录、sqlite = 文件目录、xml = 目录），各自 `id_vocabulary ok`、`database_dropped=yes`。`/proc/*/cmdline` 全文扫描与 `/tmp/bw4-store.*` 实测**零残留沙盒、零存活服务进程**。`self-test` 2 项 PASS（负例 `ERROR 1264 (22003) … Out of range value for column 'id'`）。`actionlint`、`shellcheck -S warning`、`xmllint --noout` 均绿。
  - 关键设计：两条新腿同样遵守「**缺工具**」≠「**真故障**」——没有 `sqlite3` 或没有 `python3` 时退出码 2，lane 记 SKIP 并点名缺失件；`tracker_for` 把这两条腿指到 **BW-09-03**（sqlite 与 xml 的 substrate 同一张单）。
  - 附带修掉两处（自己抓到自己）：① `xml_apply` 起初给校验器传了 **三个** argv，而脚本只读两个，于是 `out` 变成了**目录**、`open()` 抛 `FileNotFoundError`——lane 的 FAIL 消息读起来像「文件路径不存在」的环境问题，而不是用例失败；改为在脚本内由用例文件名派生输出名。② `provision_postgres` 起初把 `psql` 检查放在 `CI_STORE_PG_HOST` 捷径**之后**，于是「runner 上没有 psql」会落成 FAIL 而非 SKIP，与 mysql 腿（先查客户端、再走 host 捷径）不对称；已把检查提到捷径之前，并实测该分支确实可达（`rc=2`、`no psql on this runner`）。
  - 证据：`ci-out/store/{mysql,postgres,sqlite,xml}.tsv` 四条独立记录；`ci/store.sh`（`provision_sqlite`/`sqlite_apply`/`provision_xml`/`xml_apply`、`tracker_for`）、`tests/store/xml/id_vocabulary.xml`、`.github/workflows/ci.yml` 的 `storage` 作业新增 `sqlite3` 安装与 `actions/setup-python@v6`（`python-version: '3.12'`）。CI run **44774877216**（push 触发，HEAD `843f5c7`，`completed/success`，7 作业全 success：`Storage cases on a real backend` 内 `Storage clients`/`Python`/`Run the storage cases` 全 success，归档 `ok: 6 manifest row(s) round-tripped, bundle 4.0K`）。artifact `ci-results-storage-44774877216` 内四条记录 `provisioned` 分别为 `127.0.0.1:3306`（mysql，证明走 `services:` 容器）、`127.0.0.1`（postgres）、`/tmp/bw4-store.df8rGc/sqlite`、`/tmp/bw4-store.df8rGc/xml`（后两条是 runner 就地起库），四条 `database_dropped=yes`；psql 检查顺序修正后的收口复跑为 run **44775436811**（HEAD `1edea0c`，`completed/success`，同为 `6 manifest row(s) round-tripped`），详见下方 BW-22 交付汇总。
- [x] **BW-22-02-04 接入兼容性差分**。**验收** 替换路径可在本地复跑。
  - 原版：BigWorld 的 `process_defs` 是**工具源码加编译好的二进制**（`programming/bigworld/tools/process_defs/` 6 个文件；`game/bin/server/el7/tools/process_defs` 为 128 MB ELF），消费 `alias.xml` + `*.def` + `entities.xml` 并吐出诊断流。而 CI 里它**从不运行**：`.github/workflows/` 四份工作流只有 configure → build → `ctest`，`bwengine_mysql/xml` 仅在 `ci-windows.yml:355` 的一句注释里被提及。也就是说原版 CI 从未比对过「替换件输出」与「原版工具输出」——差分这件事在原版里**根本不存在**，不是没做好。
  - 新版：单一入口 `ci/parity.sh`，两半一边一条。**原版半边**：把夹具装成 `Fixture.def`（原版按 `.def` 文件名命名实体，消息写 `for Fixture.hp`，所以两边必须用同一个实体名才是在比同一件事），在私有沙盒内 `process_defs -v -p <probe>/scripts`，并导出 `BW_RES_PATH=探针树:reference res 根`（与 `referenceBaseline.sh` 同序）；只留 `Parser::method:` 形态的诊断——先剥 `[DataDescription]` 标签，再要求 body 含 `::`；级联记账行（`EntityDescriptionMap: Failed to load or parse def …`、`Failed to parse .def files`）只有**一个**冒号、`WARNING` 被 `^ERROR: ` 锚点排除，故天然被滤掉。**替换半边**：`extensions/vscode/out` 的 `checkDefText`（BW-20-01-05 的产物），类型表用 reference 自己的 `alias.xml` 而非手抄，逐条打印 `d.message`。`out/` 缺时先 `pnpm run build`（实测 29 包、tsc 可用），装不出才算缺 substrate。
  - 差分断言：原版每条诊断 body 必须在我方输出里**逐字出现**。少报一条、或措辞改了，lane 就红并点名。
  - 用例：`tests/parity/def/{good,bad-flags,bad-metadatatype}.def` 三个夹具，来自 `extensions/vscode/tools/verify/diagnostics.cjs` 里**内联**的三份文本，落成可复跑的文件。
  - 结果：`parity-def` **PASS**——原版 **3 条诊断全部复现**（`Invalid Flags section 'WRONG' for hp`、`Could not find MetaDataType 'NOPE'`、`Unable to find data type 'NOPE' for Fixture.hp`），`good.def` 无诊断（干净夹具不发明诊断）。`self-test` 2 项 PASS：种植一个报错信息的替换件后，**lane 自己的断言**把它拒了；干净夹具无原版诊断，无从分歧。本地零设置可跑（reference 树取 `$HOME/workspaces/BigWorld`）。`/tmp/bw4-parity.*` 与 `/proc/*/cmdline` 实测零残留。
  - 关键设计：**SKIP 一次点名全部缺失件**（原版工具、`alias.xml`、res 根，三条一起）并指向 **BW-00-04-05**（单组件替换运行），不逐条挤牙膏。托管 runner 上没有 reference 树，该作业因此**绿而 SKIP**——这不是红灯，是记录：把「替换路径为什么在此不可跑」写下来，正是本条目的产出。
  - 附带修掉四处（自己抓到自己）：① 原版把诊断写成 `ERROR: [DataDescription] Parser::method: body`，标签在 `::` **之前**，故原本 `^[^ ]+::` 的锚点会**拒绝每一条真实诊断**；改为先剥 `[Tag] `。② 漏设 `BW_RES_PATH` → 原版 `No res path set.` 直接拒跑，被差分当成「我方少报一条」而 FAIL——一条环境缺陷读起来像门禁结果。③ self-test 注入失效：`PORT_DRIVER` 在 source 时求值，`VAR=x func` 这种前缀赋给 shell **函数**根本到不了 `port_bodies`，于是「种植的分歧不可见」被报成 lane 咬不动；改为调用时读 `${CI_PARITY_PORT_DRIVER:-$PORT_DRIVER}`。④ 同一次修里把 self-test 从「换了 driver」升级为「跑 lane 自己的断言」，否则等于没测门禁。
  - 附带修掉一处（workflow 面）：`missing_substrates` 原把缺件报成 `/home/cui/...` 下的路径——托管 runner 的 artifact 里出现开发机绝对路径，读起来像 CI 真去那儿找过。改为 reference 树缺省时报「no reference tree (set BW_REFERENCE or CI_PARITY_REFERENCE)」，默认值改用 `$HOME` 相对，本地仍零设置可跑。
  - 证据：`ci-out/parity/def.tsv`（`fixture` / `reference_diagnostic` / `result` 三列，3 行全 `reproduced`）；`ci/parity.sh`、`tests/parity/def/*.def`、`ci/changes.sh`（`parity` lane）、`.github/workflows/ci.yml` 的 `parity` 作业与 `harness` 作业新增的 `ci/parity.sh self-test`。CI run **44778616339**（HEAD `c56b598`，`completed/success`，8 作业 success + 3 skipped）：`Compatibility differential against the reference tool` 作业 **success**，其内 `parity-def SKIP` 点名三件缺失件与 BW-00-04-05；`Harness self-tests` 内 `ci/parity.sh self-test` 同样 `selftest-parity SKIP`，两条 SKIP 都没让作业变红；reference 树解析改用 `$HOME` 后的收口复跑为 run **44779041655**（HEAD `ea8e403`，`completed/success`，8 作业 success + 3 skipped，SKIP 内已改为 `/home/runner/workspaces/BigWorld/...`），详见下方 BW-22 交付汇总。
- [x] **BW-22-02-05 接入 SDK 与插件用例**。**验收** 产物能在宿主中加载。
  - 原版：BigWorld 有「把产物装进宿主」的原型——`programming/bigworld/tools/plugin_system/plugin_loader.cpp:57` 用 `::LoadLibrary(pluginFileName)` 逐个装载插件 DLL——但 CI 里**从不**做这件事：`.github/workflows/` 四份工作流只有 configure → build → `ctest`，没有任何一步把编译产物交给宿主进程加载；SDK 树在原版根本不存在（SDK 是本仓规划，`docs/guide/client-sdk.md` 定了 `sdk/core, sdk/ffi, sdk/python, sdk/engines` 四件布局）。也就是说「产物能在宿主中加载」在原版 CI 里**没有对应物**，不是没做好。
  - 新版：`ci/sdk.sh` 四个检测器，各按自己的 substrate 独立判 PASS / SKIP / FAIL：① `sdk-artifact`——整棵 `sdk/` 不在 → SKIP 点名 BW-21-01-01，树已开建但四件不全 → FAIL（开建了就必须查，不许报走）；② `plugin-host`——`crates/plugin*` 不在 → SKIP 点名 BW-13-06-03（原版 plugin_system 的 load 行为用例归它写）；③ `editor-host`——`extensions/vscode/package.json` 未声明 `@vscode/test-electron` → SKIP 点名 BW-20-03-03；④ `built-load`——本仓今天唯一能真答的一条：`node` 直接 `require` 编译产物 `out/workspace/diagnostics.js`，且入口 `checkDefText` 必须是可用函数——「产物能被宿主加载」以最朴素的宿主（node）落到可执行断言。
  - 关键设计：**SKIP 存根必须能点亮才算门禁**。自测把四件 substrate 种进私有根（`CI_SDK_ROOT`），逐条要求检测器从 SKIP 翻成真判定；再反手验一棵空树上「凡缺 substrate 必 SKIP、绝不假 PASS」。这里踩了同一类坑的第二次：`ROOT="${CI_SDK_ROOT:-$REPO_ROOT}"` 在 source 时求值，`VAR=x func` 前缀赋给 shell **函数**根本到不了函数体——改为 `cur_root()`/`cur_ext()` 调用时求值后自测 4/4 翻转。
  - 选路与 workflow：`ci/changes.sh` 新增 `sdk` lane（`sdk/* | tests/sdk/* | crates/plugin*/* | tests/plugins/* | ci/sdk.sh`）；`ci.yml` 新增 `SDK and plugin host-load cases` 作业——先把扩展核心编出来（`out/` 不入库）再跑 `ci/sdk.sh`；`harness` 作业的「Prove every lane bites」补 `ci/sdk.sh self-test`。
  - 结果：本地 `sdk-built-load` **PASS**、三条 SKIP 各点名自己的 ticket；self-test 4/4 PASS；`shellcheck -S warning` 与 `actionlint` 双绿。CI 首跑**红**（见证据）——pnpm 12 对 ignored postinstall scripts（`@vscode/vsce-sign`、`keytar`）直接 `ERR_PNPM_IGNORED_BUILDS` 退出 1，而 tsc 其时已在位；本机沙盒复现同一失败并证实 `node_modules/.bin/tsc` 存在、`tsc -p .` 照常产出、`require` 出的 `checkDefText` 可用。修法与 TypeScript lane 同门：**装不装干净不是门，产物在不在才是门**——`pnpm install || true` 后直接 `./node_modules/.bin/tsc -p .`，缺 tsc 的树照样当场红。
  - 证据：CI 收口 run **44781880340**（HEAD `5b8ba93`，`completed/success`，9 作业 success + 3 skipped）：`SDK and plugin host-load cases` **success**，manifest 四行——三条 SKIP 路径均在 `/home/runner/work/boundless/boundless` 下（托管 runner 自己的树，非开发机回显），`sdk-built-load PASS "node loaded the compiled extension core and its entry point is usable"`，归档 `ok: 4 manifest row(s) round-tripped, bundle 4.0K`；同 run `Harness self-tests` 内 `ci/sdk.sh self-test` 4/4 PASS。首跑 run **44781223162**（HEAD `652b54f`）`failure`，红在 `Build the extension core` 的 `ERR_PNPM_IGNORED_BUILDS`，即上段修掉的缺陷。本地等价命令 `ci/sdk.sh` 与 `ci/sdk.sh self-test` 均 rc=0。详见下方 BW-22 交付汇总。
- [x] **BW-22-02-06 归档覆盖率、日志和夹具**。**验收** 失败可取回证据。
  - 原版：BigWorld `.github/workflows/ci.yml` 与 `ci-windows.yml` 的上传步骤叫「Upload logs on failure」`if: failure()`，只传 `cmake-configure.log` / `cmake-build.log` / `ctest.log` 这类 tee 出来的散文件；`scripts/package-daily.sh` 产出的是安装包（tar.gz / .deb / .rpm），不是测试证据——没有覆盖率、没有失败夹具、也没有回读校验。
  - 新版：`ci/archive-results.sh` 在 `CI_OUT` **同级**打 `ci-results-<name>-<sha>-<utc>.tar.gz`（manifest + `logs/` + `coverage/` + `fixtures/` + `toolchain.json`），打包后**回读**、行数比对通过才交给上传。`.github/workflows/ci.yml` 每个作业加 `Bundle results`（`if: always()`）与 `Upload results`（`ci-results/<job>-<run_id>`、`if: always()`、`if-no-files-found: warn`）——绿跑的 manifest 正是归档 lane 要校验的东西，红跑的日志才是留档的意义所在，所以两者都不再是 `failure()`。bundle 不落在 `CI_OUT` 内：tar 归档自身目录会无限增长。
  - 关键设计：行数在打包**前**快照并作为参数传入校验，而不是回读时重新计数。manifest 是 append-only，归档 lane 自己那行在 tarball 已生成**之后**才落盘；若重新计数，一个缺行的 bundle 会对着「已经变长」的 manifest 判通过——本次自测的截断负例正是这条。
  - 结果：`ci/archive-results.sh self-test` 3 项全 PASS（满负载 CI_OUT round-trip、截断 bundle 被拒、空树如实报告）；本地 7 作业等价门 27 行 round-trip、bundle 56K；解包后 3 条 FAIL 行与其具名日志俱在（`logs/rust-test.log` 60K，含 panic `left: 8 / right: 0` 原样可读）。
  - 附带发现（不属 CI 面，本席位未改一行代码）：在分叉点 `2e63e65` 上整仓并行跑时，`crates/server-runtime/src/signals.rs:825` 的 `signals::tests::ignore_signal_swallows_raises` **非确定性失败**（门内 160 过 1 败；整库复跑 1/5 再败；单跑 10/10 过）。诊断是测试间共享进程级信号状态：`PENDING_SIGNALS` 与计数 handler 均为进程全局，本用例 `ignore_signal(SIGUSR2)` 与兄弟用例的 handler 注册交叉时，它的计数器被他人投递累加到 8。server-runtime 席位随后在 `6a1bbd2` 修掉了同一根因（`The pending bitset is shared process state that a sibling test may have dirtied`），rebase 后两次 `rust-test` 均 PASS、flake 不再复现。该 flake 会间歇性让 `Rust build and test` 作业变红——正是归档 lane 存在的理由。
  - 证据：分叉点树 `ci-out/manifest.tsv` 的 `archive` 行 `ok: 27 manifest row(s) round-tripped, bundle 56K`；rebase 后 `a2c0f66` 上再跑一次 `ok: 15 manifest row(s) round-tripped, bundle 60K`；bundle 解包后 3 条 FAIL 行与其具名日志俱在（`logs/rust-test.log` 60K，含 panic `left: 8 / right: 0` 原样可读）；CI 首跑 run 号见下方 BW-22 交付汇总。

#### BW-22 交付汇总（CI 面真跑记录）

workflow 为 `.github/workflows/ci.yml`（push / pull_request / workflow_dispatch）。BW-22-01 与 BW-22-02 的条目共用这张表，因为同一个 workflow 面被逐条改造：

| run | HEAD | 触发 | 结果 | 说明 |
| --- | --- | --- | --- | --- |
| 44745195746 | `0a64405` | push | completed/cancelled | 被后续 push 取代 |
| 44747373936 | `51de90d` | push | completed/**failure** | 6/8 绿；`Toolchain pin and lockfile` 红——干净 runner 注册表为空，`cargo build --offline` 失败被误报成「锁文件坏」。本地冷 `CARGO_HOME` 复现同一失败；修法是 lane 先 `cargo fetch` 再离线构建 |
| **44749719511** | `2e63e65` | push | completed/**success** | BW-22-01 收口：`Select lanes`/`Toolchain`/`Python`/`TypeScript`/`Documentation`/`Harness` 全 success，两个 Rust 作业按选路 **skipped**——路径条件在生产环境生效 |
| 44757456657 | `95c8f75` | push | completed/**failure** | BW-22-02-06 上线首跑：7 个作业的 `Bundle results` 全 success，但 `Upload results` 全红——bundle 文件名里的 UTC **冒号**被 Actions artifact 路径校验拒绝（`The path for one of the files in artifact is not valid … Contains the following character: Colon`）。旧上传步骤挂 `if: failure()`，绿作业从未真正执行过上传，故该缺陷一直潜伏；改成 `if: always()` 才暴露 |
| **44760793199** | `1ef471e` | workflow_dispatch（`runners=true`） | completed/**failure** | BW-22-02-01 三端矩阵：`ubuntu-latest` success、`windows-latest` success、`macos-latest` failure（`server-runtime/src/signals.rs:517` 用了仅 Linux 有的 `SIGRTMIN`/`SIGRTMAX`/`sigwaitinfo`）。作业随后改为 `continue-on-error` 报告型 |
| **44770615698** | `c6217fc` | push | completed/**success** | BW-22-02-02 上线首跑：`Storage cases on a real backend` **success**，lane 四条全 PASS（`selftest-store`/`selftest-store-drop`/`store-mysql`/`store-postgres`），归档 `ok: 4 manifest row(s) round-tripped, bundle 4.0K`。artifact 内 `provisioned=127.0.0.1:3306`（mysql）与 `127.0.0.1`（postgres）证明走的是 workflow 的 `services:` 容器而非本机起库，两条 `database_dropped=yes` |
| **44774877216** | `843f5c7` | push | completed/**success** | BW-22-02-03 上线首跑：7 作业全 success，`Storage cases on a real backend` 六条全 PASS（两条 selftest + `store-mysql`/`store-postgres`/`store-sqlite`/`store-xml`），归档 `ok: 6 manifest row(s) round-tripped, bundle 4.0K`。artifact 内四条 `provisioned` 分别是 `127.0.0.1:3306`、`127.0.0.1`（`services:` 容器）与 `/tmp/bw4-store.df8rGc/sqlite`、`…/xml`（runner 就地起库），四条 `database_dropped=yes`——同一张表里「各有独立结果」成立 |
| **44775436811** | `1edea0c` | push | completed/**success** | BW-22-02-03 收口：`psql` 检查提到 `CI_STORE_PG_HOST` 捷径之前（与 mysql 腿对称，缺客户端记 SKIP 而非 FAIL）后的复跑，`Storage cases on a real backend` success、归档同为 `6 manifest row(s) round-tripped`。该 HEAD 是本条目的最终推送状态 |
| **44778616339** | `c56b598` | push | completed/**success** | BW-22-02-04 上线首跑：新增 `Compatibility differential against the reference tool` 作业 **success**，其内 `parity-def SKIP`（托管 runner 无 reference 树，点名三件缺失件与 BW-00-04-05，SKIP 不红）；`Harness self-tests` 内 `ci/parity.sh self-test` 同为 `selftest-parity SKIP`。本地等价命令真跑出 `parity-def PASS`（3 条原版诊断全复现） |
| **44779041655** | `ea8e403` | push | completed/**success** | BW-22-02-04 收口：缺 reference 树时的 SKIP 不再回显开发机绝对路径（改由 `$HOME` 解析，CI 上点名 `/home/runner/workspaces/BigWorld/...`），本地仍零设置可跑。该 HEAD 是本条目的最终推送状态 |
| 44781223162 | `652b54f` | push | completed/**failure** | BW-22-02-05 上线首跑：新增 `SDK and plugin host-load cases` 作业 **failure**——`Build the extension core` 里 pnpm 12 对 ignored postinstall scripts（`@vscode/vsce-sign`、`keytar`）以 `ERR_PNPM_IGNORED_BUILDS` 退出 1，其余 8 作业全绿 |
| **44781880340** | `5b8ba93` | push | completed/**success** | BW-22-02-05 收口：`SDK and plugin host-load cases` **success**——三条 SKIP 各点名 BW-21-01-01 / BW-13-06-03 / BW-20-03-03，`sdk-built-load` **PASS**（node 在托管 runner 上加载了编译出的扩展核心），归档 `ok: 4 manifest row(s) round-tripped, bundle 4.0K`；`Harness` 内 `ci/sdk.sh self-test` 4/4 PASS。该 HEAD 是本条目的最终推送状态 |
| **44783874538** | `47d67ed` | push | completed/**success** | BW-22-03-01 上线首跑：新增 `PR short load test` 作业 **success**——`load-scenarios`/`load-driver`/`load-run` 三条 SKIP 各点名 BW-17-03-05 / BW-17-01-01（场景契约已由 `ci/load.sh` 立起，substrate 归 BW-17），归档 `ok: 3 manifest row(s) round-tripped, bundle 4.0K`；`Harness` 内 `ci/load.sh self-test` 5/5 PASS。该 HEAD 是本条目的最终推送状态 |
| **44784647214** | `32e2713` | push | completed/**success** | BW-22-03-02：新增 `.github/workflows/load-schedule.yml`（cron `17 2 * * *` + dispatch inputs `clients`/`duration`，缺省 32/600s）；dispatch 当场试跑 404（workflow 须落默认分支才可触发，PR 到 main 后零改动激活）。本地等价命令证 knob 透传：`(budget 900s, clients 64)`。该 HEAD 是本条目的最终推送状态 |
| 44786087965 | `3d2789e` | push | completed/**failure** | BW-22-03-03 上线首跑：新增 `Fault drill` 作业，首跑即红 rc=101——artifact 三件套（topology.tsv / run.txt / fault-drill.log）解包定位 `harness.rs:694` `#[test]` 重复属性，`-D warnings` 冷编译硬错误；本机 target/ 陈旧故本地未见。这正是失败路证据可取回的验收本身 |
| **44786815265** | `085bea8` | push | completed/**failure** | BW-22-03-03 中跑：`fix: drop duplicate test attribute in harness` 后 `Fault drill` **success**、`Rust build and test` success；`Rust format and lint` 红为 `cargo-fmt`/`cargo-clippy` 未装（setup action 缺省空 components 表） |
| **44787515629** | `e5a17f2` | push | completed/**failure** | BW-22-03-03 收口：`rust-static`/`harness` 显式钉 `components: rustfmt clippy` 后 `rust-fmt` **PASS**；`Fault drill` **success**。仅剩 `rust-lint` 红在 `crates/resources`（base64.rs:68:8 等约 32 处，BW-22-01 已记账的历史债，clippy workspace wrapper 使 `-p` 选路豁免不了）。本条目自有作业全绿，该 HEAD 是本条目的最终推送状态 |
| **44788192807** | `26425bd` | push | completed/**failure** | BW-22-03-04 前的 docs 提交：唯一红仍为 `Rust format and lint`（`crates/resources` 历史债），报告条目自有作业未受影响 |
| **44789187301** | `7dcb8cd` | push | completed/**success** | BW-22-03-04 上线首跑：`Comparison report` **success**（裸 runner 12 行：scenario 如实 `none recorded yet`、db 三项如实 absent）、`Harness self-tests` 内 `ci/report.sh self-test` 两行 PASS。唯一红仍为 `rust-lint` 历史债。该 HEAD 是本条目的最终推送状态 |
| 44789687782 | `b69126b` | push | completed/**failure** | BW-22-03-04 的 docs 提交：唯一红仍为 `Rust format and lint`（lane 分支 push 的 diff 基线是与 main 的 fork 点，故全仓 lint 债每跑必现），报告条目不受影响 |
| **44790503820** | `02d90bb` | push | completed/**failure** | BW-22-04-01 上线首跑：`Installer package and launch smoke` **success**（release 构建→前缀→2.6M tarball→干净前缀安装→启动烟测全 PASS）、`Harness self-tests` 含 `ci/package.sh self-test` 3/3。唯一红仍为 `rust-lint` 历史债。该 HEAD 是本条目的最终推送状态 |
| **44791771769** | `4e4590c` | push | completed/**failure** | BW-22-04-02 上线首跑：`Docs site package and version index` **success**（构建 20 页→index 戳 0.1.0/4e4590c→864K tarball→版本元数据对账 PASS）；`Documentation site and links` 的 docs-build 腿首次真跑 PASS（修掉门错根 package.json 的旧条件 + link checker 排除 node_modules）。唯一红仍为 `rust-lint` 历史债。该 HEAD 是本条目的最终推送状态 |
| **44792575020** | `a051884` | push | completed/**failure** | BW-22-04-03 上线首跑：`Engine SDK archives and import smoke` **success**（`sdk/engines/` 不在 → 如实 SKIP 点名 BW-21-01-01）；`Harness self-tests` 含 `ci/sdk-archive.sh self-test` 4/4。唯一红仍为 `rust-lint` 历史债。该 HEAD 是本条目的最终推送状态 |
| **44793741916** | `092e962` | push | completed/**failure** | BW-22-04-04 上线首跑：`VSIX package and clean-host install` **success**（新编译→vsce 160K→档案完整性→干净宿主核心加载 PASS，editor 腿如实 SKIP 点名 BW-20-03-03——首跑曾咬中 `require('vscode')` 主入口裸 node 装不了的契约缺陷并修正）；`Harness self-tests` 含 vsix 自测 3/3。唯一红仍为 `rust-lint` 历史债。该 HEAD 是本条目的最终推送状态 |
| **44795047237** | `f28df98` | push | completed/**failure** | BW-22-04-05 上线首跑：`Harness self-tests` **success** 含 checksums 自测 4/4（回环 PASS、单字节篡改→`CHANGED` 点名、删文件→`MISSING` 点名、错版本戳→`MISSTAMPED` 对 header 咬住）；installer/docs/sdk/vsix 四个打包面作业全 success。唯一红仍为 `rust-lint` 历史债。该 HEAD 是本条目的最终推送状态 |
| **44796370170** | `b45f330` | push | completed/**failure** | BW-00-03-02 收口：`Toolchain pin and lockfile` **success**——新增 `.python-version` 单一声明源，四处 `setup-python` 改读 `python-version-file`，CI 照录 `Successfully set up CPython (3.12.16)` 且 lane 行 `python_pinned=3.12 python_resolved=3.12.16`（CI 内不一致即 FAIL 的硬门）。唯一红仍为 `rust-lint` 历史债。该 HEAD 是本条目的最终推送状态 |

本地等价门（`CI_OUT` 走默认仓根路径，与 CI 一致）在 `a2c0f66` 上全跑一次：28 PASS / 3 SKIP / 2 FAIL，两条 FAIL 均为 `rust-fmt`（211 处）与 `rust-lint`（32 处，全在 `crates/resources`），属各 crate 所属席位的历史债务，不属 CI 面；`archive` 行 `ok: 32 manifest row(s) round-tripped, bundle 60K`。

门禁结论：本席位负责的门面（选路、工具链、TypeScript、文档、归档、三端启动记录、全部自测）在本机与 CI 均可复跑且真绿；Rust 格式与 lint 在全仓口径仍红，且债务随合并在各席位间移动，CI 面的职责是把位置打到 `path:line:col` 并保证可取回，而非替他人还债。

#### BW-22-03 压测与回归报告

PR 跑短 smoke，定时或手动任务跑容量、稳定性和故障场景。生成不同版本可比较的报告，记录硬件、拓扑、数据库后端及场景版本。

- [x] **BW-22-03-01 接入 PR 短压测**。**验收** 小规模场景产出结果。
  - 原版：BigWorld 有真正的压测载体——`bots` 应用（`programming/bigworld/server/tools/bots/` 源码，`game/bin/server/el7/server/bots/bots` 为 194 MB ELF），模拟客户端连上集群；`bots_config.cpp` 以 `bots/` 前缀配置（serverName、port、transport 默认 UDP 可选 TCP/WebSockets、controllerType `Patrol` + `server/bots/test.bwp`），数量靠 watcher 在运行时增减（`add_bots_callable_watcher.cpp`/`del_bots_callable_watcher.cpp`）。但 CI **从不跑它**：`.github/workflows/` 四份工作流 grep `bots` 零命中，压测在原版是**对活集群的运维动作**，不是 CI 面。
  - 新版：`ci/load.sh` 把「小规模场景产出结果」落成可执行契约，BW-17 的驱动将来直接对着它写——场景是 `tests/load/scenarios/` 下的可执行文件，带 `LOAD_DURATION`（缺省 `CI_LOAD_DURATION`=30s）/ `LOAD_CLIENTS`（缺省 4）/ `LOAD_TARGET`（透传）环境量，stdout 必须打 `metric<TAB>value` 行；lane 用 `timeout` 卡墙钟，**超时杀掉并点名**、退非零点名、零指标点名；每场景落 `ci-out/load/<name>.tsv`，stderr 噪声不入账。三个 case 各对各的 substrate：`load-scenarios`（场景集缺 → SKIP 点名 BW-17-03-05）、`load-driver`（`apps/bots` 缺 → SKIP 点名 BW-17-01-01，目录在而 `Cargo.toml` 不在 → FAIL）、`load-run`（两者齐才真跑）。
  - 关键设计：**SKIP 存根必须能点亮**。自测把驱动 crate 与两个场景种进私有根——`good.sh` 打两行指标（其中一行走 stderr），要求 lane 只记 stdout 那行；`slow.sh` `sleep 300`，要求在 2s 预算内被杀且死因写明「exceeded the 2s wall budget」；再反手验空树上三 case 全 SKIP、绝不假 PASS。搭建时就抓到一个自己的 bug：`if ! out="$(…)"; then rc=$?` 里 `$?` 是**取反后**的 0 不是命令退出码——写完即修，未进过一次 commit。
  - 选路与 workflow：`ci/changes.sh` 新增 `load` lane（`tests/load/* | apps/bots/* | ci/load.sh`）；`ci.yml` 新增 `PR short load test` 作业（无 substrate 的今天在托管 runner 上如实 SKIP）；`harness` 作业补 `ci/load.sh self-test`。
  - 结果：本地 `ci/load.sh` rc=0（三条 SKIP 各点名自己的 ticket）、`ci/load.sh self-test` 5/5 PASS（含 stderr 不入账与 runaway 被杀）、`shellcheck -S warning` 与 `actionlint` 双绿。本仓 substrate 现状（Explore 全仓核过）：无 bots/load 生成器、`apps/` 只有 `server` 且角色进程打印一行即退（`server-runtime/src/app.rs:45-51`）、无跨进程集成测试——「跑真场景」的 substrate 归 BW-17（`todo.md:1175` `apps/bots/`、`:1269` BW-17-01-01 客户端连接层、`:1297` BW-17-03-05 PR smoke），CI 面的职责是把承接契约先立起来。
  - 证据：CI run **44783874538**（HEAD `47d67ed`，`completed/success`，10 作业 success + 3 skipped）：`PR short load test` **success**，manifest 三行 SKIP 路径均在 `/home/runner/work/boundless/boundless` 下、归档 `ok: 3 manifest row(s) round-tripped, bundle 4.0K`；同 run `Harness self-tests` 内 `ci/load.sh self-test` 5/5 PASS（`selftest-load` ×4 + `selftest-load-empty`）。本地等价命令 `ci/load.sh` 与 `ci/load.sh self-test` 均 rc=0。详见下方 BW-22 交付汇总。
- [x] **BW-22-03-02 接入定时容量与长稳任务**。**验收** 可配置并发和持续时间。
  - 原版：唯一带 `schedule` 的工作流是 `daily-build.yml`（cron `23 21 * * *`，48 小时无提交则跳过打包），它做的是**装包**（checkout → 腾盘 → 装依赖 → 抬 UDP 缓冲 → 读 pin → build），从头到尾没有任何压测/长稳场景——「定时容量与长稳」在原版 CI 里**不存在**；且该文件第 16 行自己写明 GitHub 只从**默认分支**跑 `schedule`，本条目在实现时同样撞上这一约束（见证据）。
  - 新版：`.github/workflows/load-schedule.yml`——容量与长稳共用一张脸：都跑 BW-22-03-01 立下的同一份场景契约，只是旋钮拧大。定时触发（cron `17 2 * * *`，避开全球定时任务扎堆的整分）是长稳形状——缺省 clients/600s 长预算；`workflow_dispatch` 手动触发是容量形状——`clients`/`duration` 两个输入逐 run 可配，缺省 32/600s。两个输入经 `CI_LOAD_CLIENTS`/`CI_LOAD_DURATION` 进 `ci/load.sh`，lane 的 step 行把生效值打出来（`PR short load test (budget Ns, clients M)`），旋钮是否真到位在日志里可查而非自说自话。`concurrency group: load-schedule` 不取消进行中的 run（定时与手动串行，不互踩），`timeout-minutes: 120` 兜底。
  - 结果：本地等价命令 `CI_LOAD_CLIENTS=64 CI_LOAD_DURATION=900 ./ci/load.sh` step 行如实打出 `(budget 900s, clients 64)`；push run **44784647214**（HEAD `32e2713`）`completed/success`；`actionlint` 绿。**workflow_dispatch 当场试跑未成**：`gh workflow run load-schedule.yml --ref lane/bw22-ci` 返回 HTTP 404（`workflow not found on the default branch`）——GitHub 只认默认分支上存在的 workflow，dispatch 与 schedule 都要等本文件随 PR 落到 main 才生效；届时零改动即激活（场景集 BW-17-03-06 与驱动 BW-17-01-01 落地前，每次 run 如实 SKIP 点名 ticket）。此为平台约束而非配置缺陷，本地入口已证同一代码路径。
  - 证据：`.github/workflows/load-schedule.yml`（cron + dispatch inputs）、`ci/load.sh` 的 step 行改动；本地 knob 透传输出（budget 900s / clients 64）；CI run **44784647214** completed/**success**；dispatch 404 的原文（`HTTP 404: workflow load-schedule.yml not found on the default branch`）。详见下方 BW-22 交付汇总。
- [x] **BW-22-03-03 接入手动故障场景**。**验收** 复现失败时有拓扑和日志。
  - 原版：BigWorld CI 无任何故障演练——四份工作流只有 configure/build/ctest；崩溃恢复靠人工 kill（BW-18-03-04 的考据已记「场景无法重放」），「复现失败时有拓扑和日志」在原版 CI 里没有对应物。
  - 新版：`ci/fault.sh`——故障演练走本地与 CI 共用的 `run-tests` 入口（BW-18-04-01），跑 BW-18-03-04 的进程故障夹具（crash / restart / 迁移重放，filter 缺省 `multiproc`，`CI_FAULT_FIXTURES` 可换）。**验收落在证据三件套，且成败两路都留**：`ci-out/fault/topology.tsv`（commit 取自 run 归档头——演练实际测的树；entry 全命令可复跑；fixtures 指明夹具族；`cluster=none` 如实标注今天只是进程内夹具演练，活拓扑演练点名 BW-05-06-04）、`fault/run.txt`（共享入口的 run 归档：label/run 命令/commit/when + 每用例 PASS/FAIL 行）、`logs/fault-drill.log`（全程输出）。归档与拓扑在**判分之前**落盘——红演练正是这条 lane 存在的理由，不许它先过及格线才留证据。
  - 关键设计：**自测专证失败路**——种一个打印 `FAIL a_migration_scenario_replays_identically` 且 exit 1 的 stub 入口，要求 lane 红且三件套俱在（topology 的 commit 从 stub 归档头读到 `deadbee` 而非工作树 HEAD）；再证绿路同证据、无入口时 SKIP 不假 PASS。
  - 选路与 workflow：`ci/changes.sh` 新增 `fault` lane（`crates/test-support/* | tests/fault/* | ci/fault.sh`——夹具变了 push 就重演）；`ci.yml` 新增 `Fault drill` 作业；`.github/workflows/fault-scenarios.yml` 手动触发（`workflow_dispatch`，input 选夹具，无 schedule——演练回答特定失败问题，不是夜间测量），与 load-schedule 同因待 PR 落 main 才可 dispatch。
  - 上线即咬到真缺陷：首跑 run **44786087965** 里 `Fault drill` 红，rc=101。从 artifact 解包读证据三件套（正是验收要的那条失败路）定位：`crates/test-support/src/harness.rs:694` **`#[test]` 属性重复**，在 `-D warnings` 冷编译下是硬错误——本机 target/ 是旧的所以本地绿，CI 冷构建当场揭穿（本地过 ≠ 代码对，缓存才是骗子）。修掉（`fix: drop duplicate test attribute in harness`，1 行删除），test-support 112 项测试复绿。
  - 附带修掉一处（workflow 面）：修夹具让 rust-static 首次真跑，随即暴露 `cargo-fmt`/`cargo-clippy` 未装——setup action 缺省给**空** components 表，一条安装路径落成 minimal profile；harness 作业之前绿是侥幸（`rustup show` 回退把缺省 profile 全装了）。`rust-static` 与 `harness` 两作业显式钉 `components: rustfmt clippy`，下一跑 `rust-fmt` 即 PASS。
  - 结果：本地 `ci/fault.sh` rc=0（`fault-drill PASS 26 fixture case(s) green`）、`ci/fault.sh self-test` 3/3 PASS、`shellcheck -S warning` 与 `actionlint` 双绿。剩余红与本条目无关：`rust-lint` 在 `crates/resources` 的历史债（base64.rs:68:8 等约 32 处，BW-22-01 收口已记账）——clippy 的 workspace wrapper 会把 unit graph 里的成员依赖一起 lint，故 `-p` 选路豁免不了它；CI 面职责是把位置点名，不替他人还债。
  - 证据：CI run **44786087965**（HEAD `3d2789e`）`Fault drill failure`——首跑咬中 harness.rs 重复属性，artifact 三件套可解包复读；run **44786815265**（HEAD `085bea8`）`Fault drill` **success** + `Rust build and test` success（`rust-fmt` 红为缺组件）；run **44787515629**（HEAD `e5a17f2`）`Fault drill` **success**、`rust-fmt` **PASS**、`rust-lint` FAIL 点名 `crates/resources`（历史债，位置已列）。本地等价命令均 rc=0。详见下方 BW-22 交付汇总。
- [x] **BW-22-03-04 生成对比报告**。**验收** 版本、硬件、场景及 DB 信息齐全。
  - 原版：BigWorld CI 无任何报告产物——四份工作流只 configure/build/ctest，硬件、拓扑、DB 后端信息从不出现在 CI 输出里；跨版本可比对只能靠人肉记。「生成不同版本可比较的报告」在原版 CI 里没有对应物。
  - 新版：`ci/report.sh`——一次落一张 `ci-out/report/report.tsv`，TSV `section\tkey\tvalue` 行让任意两次 run 能按任意行 join。四段验收各对应一段：`version`（commit / rustc release / 生成时间）、`hardware`（arch / CPU 型号 / 核数 / 内存）、`scenario`（树里全部 run 归档 + 故障演练归档按 label 去重各记一行 summary，`ci-out/load/*.tsv` 每份记指标行数；没有就如实写 `none recorded yet`，绝不编造）、`db`（mysql/psql/sqlite3 逐个 `--version`，不在 runner 上就写 `absent on this runner`）。第五段 `compare` 可选：给 `CI_REPORT_BASELINE` 指到基线 run.txt，就走共享 diff 入口（BW-18-04-02）产出 REGRESSED/healed 行——这是整张报告存在的意义。
  - 关键设计：**验收是「齐全」，lane 就自己读回自己**——report 写完 grep 回读四段段名，缺任何一段即 FAIL 点名缺哪段，不许缺段的报告出 lane。自测两路：种一个私有根 + 假归档，要求四段齐全且 scenario 行如实反映种进去的 `1 passed`（这一条抓到过自己的 bug：段函数不带 `CI_REPORT_ROOT` 前缀调用时扫的是真仓而非私有根——假绿当场现形）；再种打印 REGRESSED 的 stub diff，要求基线真的流进 compare 段。同 label 去重（故障演练的 ci-out 副本与 target 原件各存一份，不许记两行）。
  - 选路与 workflow：`ci/changes.sh` 新增 `report` lane（`ci/report.sh`）；`ci.yml` 新增 `Comparison report` 作业（报告只读不编，故不装 Rust）；`harness` 作业补 `ci/report.sh self-test`；`load-schedule.yml` 在负载后追加「Generate the comparison report」步——夜测的负载记录与报告落同一个 artifact，两夜可比；新建 `.github/workflows/report.yml` 手动触发（`baseline` input 透传 `CI_REPORT_BASELINE`，装 Rust 因为 diff 入口要编译）。
  - 结果：本地 `ci/report.sh` rc=0（15 行如实：commit `26425bd`、toolchain 1.100.0、`Intel i9-10880H` 14 核、test-support-multiproc 26 passed、load-good/load-slow 指标行数、mysql/psql/sqlite3 三版本、compare `not given`）；`ci/report.sh self-test` rc=0（`selftest-report` PASS 13 行、`selftest-report-compare` PASS）；`shellcheck -S warning` 与 `actionlint` 双绿。
  - 证据：CI run **44789187301**（HEAD `7dcb8cd`）：`Comparison report` 作业 **success**，托管 runner 上 `report-summary PASS 12 row(s): report/report.tsv`（无归档的裸 runner 形状：scenario 如实 `none recorded yet`、db 三项如实 absent，比本地少的行数正是环境差异的如实记录）；同 run `Harness self-tests` **success**，两行 report 自测 PASS 照录。本 run 唯一红 `Rust format and lint` 为 `crates/resources` 历史债（BW-22-01 已记账，非本条目）。本地等价命令均 rc=0。详见下方 BW-22 交付汇总。

#### BW-22-04 发布物流水线

构建并归档三端二进制与安装包、VitePress 站点及匹配的文档索引、SDK 包和 VSIX。校验版本、散列和打包内容，使用干净环境对安装/导入/运行做 smoke。

- [x] **BW-22-04-01 编排三端安装包**。**验收** 每端烟测可安装和启动。
  - 原版：`daily-build.yml` 是唯一装包面——cron `23 21 * * *` + **48 小时无提交跳过**的保鲜规则，Linux 出 tar.gz / deb / rpm、Windows 出自包含 zip；且文件头自己写明约束：**不建 tag、不发 Release、结果只是 workflow artifact**（14 天后回收）。「每端烟测可安装和启动」在原版没有对应步骤——装包后没有安装+启动验证。
  - 新版：`ci/package.sh`——装包与烟测同一条 lane：release 编译 → 装前缀（`bin/boundless-server`）→ 归档 `ci-out/package/boundless-<workspace版本>-<platform>-<arch>.tar.gz`（成员序固定）→ **烟测装「装出来的」而非「编出来的」**：解到干净前缀后运行安装好的二进制，要求 exit 0 且打出 startup 行（与 runners lane 同一份可观测契约，BW-22-02-01）。lane 打包它所在的平台；另两端由 workflow 的 matrix 供。版本号只读 workspace 现值用于文件名——**不 bump、不打 tag、不发 Release**（全局红线），产物只是 artifact。
  - 三端编排：`.github/workflows/package-daily.yml`——原版规则的直译：cron `19 3 * * *`（避开整分）、48 小时保鲜（scheduled 无提交就跳过重编译，dispatch 无条件）、`concurrency: package-daily` 不取消进行中、timeout 120 分钟、产物 artifact 14 天。matrix ubuntu/windows/macos，Windows/macOS 两腿 `continue-on-error` **report 而不 gate**（今天只有 Linux 编译，`signals.rs` 的 libc 专属 API 卡死另两端——与 runners 作业同一份考据；BW-15-03/04 启动后翻掉 continue-on-error 即可）。schedule 只认默认分支，落 main 即激活。
  - 关键设计：**烟测必须咬假货**。自测种一个 exit 0 打 startup 行的 stub 当「编好的二进制」，走完整 stage→archive→smoke 链路要求 PASS 且 tarball 真存在；再换 exit 3 的 stub 要求 FAIL 且点名退出码；最后种不存在的二进制路径要求 staging FAIL 而不是打包空气。子进程拿私有 manifest——stub 驱动的 package 行不许混进真构建的账。
  - 结果：本地 `ci/package.sh` rc=0（真 release 构建 2.6M tarball，烟测打出 `[baseapp] starting (boundless minimal entry)`）、`ci/package.sh self-test` 3/3 PASS、`shellcheck -S warning` 与 `actionlint` 双绿。
  - 证据：CI run **44790503820**（HEAD `02d90bb`）：`Installer package and launch smoke` **success**（托管 runner 上四行 PASS 照录：build/stage/archive 2.6M/smoke），同 run `Harness self-tests` **success** 含 `ci/package.sh self-test` 三行 PASS。唯一红仍为 `rust-lint` 历史债。本地等价命令均 rc=0。详见下方 BW-22 交付汇总。
- [x] **BW-22-04-02 发布文档与版本化索引产物**。**验收** 版本元数据一致。
  - 原版：BigWorld 根有 `package.json`（`docs:dev|build|preview` 三入口）+ VitePress 站点，但 CI 四份工作流**从不构建文档**——docs 产物只在本地存在；「版本化索引」与「版本元数据一致」在原版没有任何对应物（站点的 version 与交付的 RPM/deb 版本之间无一处对账）。
  - 新版：`ci/docs-package.sh`——构建（`pnpm --dir docs run build`，BW-19-01-04 的真实入口，锁文件是钉住契约，无锁 → SKIP 点名）、盖章（`index.json`：version/commit/generated/pages/bytes——写的是**实际构建出来**的树的元数据）、归档（`boundless-docs-<version>-<commit>.tar.gz`）、对账（`docs-consistency`：把树里**每一处独立的版本元数据源**互相校——workspace Cargo.toml、`docs/package.json` 自身的 version 字段、VitePress config 里声明了的 `siteVersion`、index 戳与活 commit——不一致就 FAIL 点名哪个源、差多少；还没声明的源如实记 absent，不发明）。版本号只读不 bump、不打 tag、不发 Release（红线）。
  - 关键设计：**对账门必须咬假货**。自测种私有树三路：各源一致 → PASS 且归档真含 `dist/index.html`；`docs/package.json` 版本 9.9.9 + `siteVersion` 9.9.9 → FAIL 且**两个源都点名**；index 戳是旧 commit `deadbee` 而活树是 `cafe123` → FAIL 原文点名戳过期。
  - 顺带修正（同一 substrate 变化的连带）：`ci/check-docs.sh` 的 build 腿原本写在 BW-19 落地之前（门的是还不存在的根 `package.json`，落地后永远 SKIP）——改为门 `docs/package.json` 的真实入口，**docs-build 腿从永久 SKIP 变成真门**；连带抓出并修掉 `ci/check-docs-links.py` 的潜伏 bug：walker 不排除 `node_modules/`，pnpm 一装依赖、上万份 vendored README 全被当站点内容扫（本机首跑当场红）。排除集 `{.git, .vitepress, node_modules, dist}`。
  - workflow：`ci.yml` 新增 `Docs site package and version index` 作业（node+pnpm，产物另传 `docs-package-*` artifact）；docs 作业简化为只装 pnpm 工具（依赖由 lane 自己装，本地与 CI 同一代码路径）；`package-daily.yml` 新增 `docs-package` 作业（同一 48 小时保鲜规则，Ubuntu 一腿即可，产物 `package-docs-*`）；`ci/changes.sh` 新增 `docspkg` lane（`docs/* | ci/docs-package.sh`）；harness 作业补 `ci/docs-package.sh self-test`。
  - 结果：本地 `ci/docs-package.sh` rc=0（真构建 20 页、860K tarball、index 戳 0.1.0/commit、consistency PASS 如实记 `docs/package.json declares no version (absent is recorded, not invented)`）、`ci/check-docs.sh` rc=0（links+build 双 PASS）、self-test 3/3、`shellcheck -S warning` 与 `actionlint` 双绿。
  - 证据：CI run **44791771769**（HEAD `4e4590c`）：`Docs site package and version index` **success**（docs-site/docs-index `version=0.1.0 commit=4e4590c`/docs-package 864K/docs-consistency 四行 PASS 照录）；`Documentation site and links` **success**（docs-build 腿首次在托管 runner 上真跑 PASS）；`Harness self-tests` **success** 含 docspkg 自测三行。唯一红仍为 `rust-lint` 历史债。本地等价命令均 rc=0。详见下方 BW-22 交付汇总。
- [x] **BW-22-04-03 归档各引擎 SDK**。**验收** 可以从空项目导入。
  - 原版：BigWorld 无引擎 SDK 归档面——Unity/Unreal 接入只有源码示例（`programming/bigworld/sdk/`、`examples/client_integration/`），交付走 RPM（`bigworld-devel`）与人工拷贝，CI 从不打包也从不验「空项目可导入」；首批拟支持引擎 Unity/Unreal/Godot 的说法见 todo.md BW-21 总纲。
  - 新版：`ci/sdk-archive.sh`——三引擎（`sdk/engines/{unity,unreal,godot}`）逐一处理：树不存在 → SKIP 点名各自的打包 ticket（unity=BW-21-03-03、unreal=BW-21-04-01、godot=BW-21-05-01，与 todo.md 1492/1508/1523 行的验收原文「空项目可导入/加载」逐字对上）；树存在而无包清单（unity 认 `package.json`、unreal 认 `*.uplugin`、godot 认 `plugin.cfg`/`project.godot`）→ **FAIL**——开建了就必须查，不许报走（与 `ci/sdk.sh` 同一纪律）；齐备 → 归档 `ci-out/sdk/boundless-sdk-<engine>-<版本>-<commit>.tar.gz` 并做**空项目导入烟测**：mktemp 出干净项目目录，在其中执行适配器自带的 `import-check` 可执行文件（唯一参数为 SDK 路径），exit 0 即验收成立——「可从空项目导入」落成可执行契约，BW-21 三家将来直接对着它写。
  - 关键设计：**烟测门专咬两条偷懒路**——包清单在而 `import-check` 不在 → FAIL（不许靠删掉检查来假装可导入）；`import-check` 在空项目里 exit 3 → FAIL 点名退出码。自测四路：绿适配器走完归档+导入（tarball 真含 `import-check`）、缺席两引擎如实 SKIP 并点名 ticket、无 import-check 的包 FAIL、失败的导入 FAIL——rc=0（收口时 rc=1 抓到自测子进程把 FAIL 行写进共享 manifest 的账，同 package.sh 修法：子进程拿私有 `CI_OUT`）。
  - 选路与 workflow：`ci/changes.sh` 新增 `sdkarc` lane（`sdk/engines/* | ci/sdk-archive.sh`）；`ci.yml` 新增 `Engine SDK archives and import smoke` 作业（无 Rust，纯归档+烟测）；`package-daily.yml` 新增 `sdk-archive` 作业（同一 48 小时保鲜规则，产物 `package-sdk-engines-*`）；harness 作业补 `ci/sdk-archive.sh self-test`。版本号只读 workspace 现值用于文件名——不 bump、不打 tag、不发 Release（红线）。
  - 结果：本地 `ci/sdk-archive.sh` rc=0（真树现状：`sdk/engines/` 整树不在 → 单行 SKIP 点名 BW-21-01-01）、self-test 4/4 PASS、`shellcheck -S warning` 与 `actionlint` 双绿。
  - 证据：CI run **44792575020**（HEAD `a051884`）：`Engine SDK archives and import smoke` **success**（托管 runner 上 `sdk-archive SKIP no sdk/engines/ tree ... tracked by BW-21-01-01` 照录——诚实登记而非假 PASS）；同 run `Harness self-tests` **success** 含 `ci/sdk-archive.sh self-test` 四行 PASS。唯一红仍为 `rust-lint` 历史债。本地等价命令均 rc=0。详见下方 BW-22 交付汇总。
- [x] **BW-22-04-04 归档 VSIX**。**验收** 可以在干净宿主安装。
  - 原版：BigWorld 无 VS Code 扩展、无 VSIX 面——编辑器侧交付不存在于原版 CI；本条的锚是本仓 BW-20 席位落地的 `extensions/vscode`（`vsce` 已在其 devDependencies）与原版 `daily-build.yml` 的「装包不发布」约束形态。
  - 新版：`ci/vsix.sh`——**先新编译再打包**（陈旧 `out/` 不许进包：`tsc -p .` 后才跑 `vsce package --no-dependencies`，用扩展自带的 vsce，打包规则与发布者同一条路径），归档 `ci-out/vsix/boundless-vscode-<扩展版本>-<commit>.vsix`；`vsix-verify` 查档案完整性——`extension.vsixmanifest`、`extension/package.json`、package.json 声明的 `main` 条目必须在档内，且 manifest 的 version 与 package.json 一致（自测抓到过 sed 未锚定前缀导致假失配——`<PackageManifest>0.0.1` ≠ `0.0.1`——补 `.*` 锚定修掉）；`vsix-install` **干净宿主安装烟测**：mktemp 出干净扩展目录、python stdlib 解包（runner 不保证有 unzip）、从**安装副本**加载宿主无关核心 `out/workspace/diagnostics.js` 并断言 `checkDefText` 可用——与 `ci/sdk.sh` 的 built-load 同一份契约，但加载的是装出来的而非编出来的。
  - 关键设计：**安装烟测首跑即咬中真缺陷**——最初让 node 加载 package.json 的 `main` 条目，真跑 rc=1：`extension.js:46` `require('vscode')`——扩展主入口**理应**依赖编辑器宿主 API，裸 node 加载不了它。契约修正为加载宿主无关核心（这正是 BW-20-03-03 编辑器宿主缺失的边界：入口级安装验证归编辑器腿）；`vsix-editor-host` 腿在 `@vscode/test-electron` 未声明前如实 SKIP 点名 BW-20-03-03。自测三路：完整 vsix 验证+安装全过、缺 main 条目 FAIL 点名、manifest/package 版本失配 FAIL 点名（python stdlib 造 vsix，不依赖 vsce）。
  - 选路与 workflow：`ci/changes.sh` 新增 `vsix` lane（`extensions/vscode/* | ci/vsix.sh`）；`ci.yml` 新增 `VSIX package and clean-host install` 作业（node+pnpm，产物 `vsix-*`）；`package-daily.yml` 新增 `vsix-archive` 作业（同一 48 小时保鲜规则，产物 `package-vsix-*`）；harness 作业补 `ci/vsix.sh self-test`。版本号只读扩展现值用于文件名——不 bump、不打 tag、不发 Release、不 publish（红线）。
  - 结果：本地 `ci/vsix.sh` rc=0（五腿：build/package 160K/verify/install/editor-SKIP 全部如实）、self-test 3/3、`shellcheck -S warning` 与 `actionlint` 双绿。
  - 证据：CI run **44793741916**（HEAD `092e962`）：`VSIX package and clean-host install` **success**（五行 verdict 照录：`boundless-vscode-0.0.1-092e962.vsix (160K)`、installed core loads and checkDefText is usable from a clean host、editor-host SKIP 点名 BW-20-03-03）；同 run `Harness self-tests` **success** 含 vsix 自测三行。唯一红仍为 `rust-lint` 历史债。本地等价命令均 rc=0。详见下方 BW-22 交付汇总。
- [x] **BW-22-04-05 校验发布元数据与散列**。**验收** 改动产物会检测失败。
  - 原版：`daily-build.yml` 的装包面没有任何散列与元数据校验——原版 `scripts/` 与 `.github/workflows/` 全树 grep 不到 checksum/sha256 面，产物被改动无从检测；「校验版本、散列」在原版 CI 不存在。
  - 新版：`ci/checksums.sh`——`write` 给每个打包产物（`package/*.tar.gz`、`docs/*.tar.gz`、`docs/index.json`、`sdk/*.tar.gz`、`vsix/*.vsix`）算 SHA256 写 `ci-out/checksums/SHA256SUMS`，文件头携带发布元数据三行（`# version:` 只读 workspace 现值 / `# commit:` / `# generated:`）；`verify` 逐行重算——缺文件报 `MISSING`、字节改动报 `CHANGED`，都点名文件；`metadata` 对账发布元数据自洽——header 的 commit 必须是活 commit，文件名戳着活 commit 的产物其版本戳必须与 header 版本一致（`MISSTAMPED` 点名差在哪），早前 commit 的产物如实计数 `stale: N` 而非悄悄混账。
  - 关键设计：**校验门必须咬假货**。自测四路：种产物走 write→verify 回环 PASS 且 SHA256SUMS 真在盘上；单字节篡改 → `CHANGED` 点名该文件；删文件 → `MISSING` 点名该文件；种 `boundless-vscode-9.9.9-<活commit>.vsix`（版本戳错）→ `MISSTAMPED` 对 header 咬住。红路自测子进程拿私有 `CI_OUT`（同 sdk-archive 的修法——stub 驱动的 FAIL 行不许混进真构建的 manifest 账本）。
  - 选路与 workflow：**盖章发生在每个打包作业内**而非中心作业——产物活在各自 runner 的 `ci-out/`，一个作业看不见别人的产物：`package-daily.yml` 四个打包作业（installer matrix / docs-package / sdk-archive / vsix-archive）各在产物上传前加 `Stamp and verify checksums` 步骤，并把 `ci-out/checksums/SHA256SUMS` 并入各自的 artifact 上传路径——每个交付出去的 artifact 自带可核对的散列表。push 面由 harness 作业补 `ci/checksums.sh self-test`（`ci/*` 变更经既有 `ci` lane 触发 harness，无需新 lane——checksums 的自证就是自测咬合，与 archive-results 同型）。版本号只读不 bump、不打 tag、不发 Release（红线）。
  - 结果：本地 `ci/checksums.sh` rc=0（真产物 4 件全 hash：server tarball / docs tarball / `docs/index.json` / vsix；verify PASS；metadata PASS 且如实记 `stale: 3 artifact(s) from earlier commits, verified but noted`——本机 ci-out 跨次累积，CI 干净 runner 无 stale）、self-test 4/4 PASS、`shellcheck -S warning` 与 `actionlint` 双绿。
  - 证据：CI run **44795047237**（HEAD `f28df98`）：`Harness self-tests` **success** 含 checksums 自测四行 PASS（`selftest-sums` / `-tamper` / `-missing` / `-stamp` 照录）；同 run installer/docs/sdk/vsix 四个打包面作业与选路、报告全 success。唯一红仍为 `rust-lint` 历史债（`crates/resources` 同一集合，非本条目面）。本地等价命令均 rc=0。详见下方 BW-22 交付汇总。

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

- [x] **BW-28-01-01 提取公开 VitePress 章节**。**验收** 标题、锚点及语言可定位。
  - 证据：原版结果——引用仓库文档是散落的 markdown/HTML（`BigWorld/docs/` 有 `.vitepress` 的 zh locale 树），但没有任何机器可定位的章节索引，标题、锚点、语言全靠人眼。新版结果——`integrations/mcp/docs/`（独立 crate `boundless-docs-mcp`，不入根 workspace 以免和他席位冲突）的 `src/markdown.rs` 把 `docs/**/*.md` 抽成结构化章节：front matter 的 `title`/`lang`/`private`、标题行内标记剥离、VitePress/GitHub 锚点规则（CJK 保留、空格转 `-`、去标点）、代码围栏内的 `#` 不当标题、章节体止于同级或更高级标题、行号为含 front matter 的全文档 0 基行、locale 目录（如 `zh/guide/x.md`）推断语言、`private: true` 打标记供公开边界使用。证据链接——`cargo test` 17 例全绿（tests/extract.rs）；`cargo llvm-cov` 行覆盖 100%（141/141），clippy 0 警告（仅 serde 依赖暂待下一项启用）。
- [x] **BW-28-01-02 按 Git 版本建索引**。**验收** 两个版本同名文档可区分。
  - 证据：原版结果——引用仓库文档树没有版本维度，读者只能看当前工作区，同一个页面路径在不同发布之间无法区分。新版结果——`src/source.rs` 的 `GitSource` 只从 Git 读内容（`git ls-tree -r <ref>:docs` 列文件、`git show <ref>:<path>` 取内容、`git rev-parse --short <ref>` 解析版本键，路径全部经 Git 解析而非磁盘文件，磁盘改动不改变版本含义）；`src/index.rs` 的 `DocIndex` 按版本分区，`VersionDocs { label, commit, docs }` 既按 ref 标签（`v1`/`v2`/`HEAD`）也按 commit 哈希可查，同一页面路径在两个版本下并存。证据链接——`cargo test --test version_index` 6 例全绿（真实 git 仓库夹具）：同一 `guide/entities.md` 在 v1/v2 下内容互不相等、标签解析为 ≥7 位十六进制 commit 且用该 commit 亦能查到同一页、索引经 `to_json`/`from_json` 往返相等、`private: true` 页不入索引而公开页在、`.vitepress/config.ts` 与 `img/logo.png` 被跳过只留 `guide/page.md`、不存在的 ref 返回错误（含 ref 名）而非 panic。
- [x] **BW-28-01-03 更新修改过的文档章节**。**验收** 增量刷新后搜索命中新文本及当前锚点。
  - 证据：原版结果——引用仓库没有索引，也就没有「改了文档还能检索到」这一层。新版结果——`DocIndex::refresh` 用 `git ls-tree -r <ref>:docs` 的 blob 哈希做变更指纹：blob 未变的页整页沿用（`unchanged` 计数），变了才重新读盘抽取，返回 `RefreshReport { updated, removed, unchanged }`，同一 ref（`HEAD` 移动）原地覆盖。证据链接——`tests/incremental.rs` 4 例（编辑一页只更新那页、锚点与正文随标题改写更新、重复刷新零变更）；`tests/refresh_search.rs` 把刷新接到搜索上取证：页内容从 `## Old heading` / `Obsolete marker wording.` 改为 `## **New** heading` / `Current marker wording.` 后，`RefreshReport.updated == ["guide/entities.md"]`、`unchanged == 1`，搜索 `Current marker` 命中章节 `New heading`（`anchor == new-heading`，snippet 含 `Current marker`），搜索 `obsolete` 返回空。
- [x] **BW-28-01-04 移除删除的文档章节**。**验收** 增量刷新后检索结果和链接都不再包含被删除章节。
  - 证据：原版结果——被删文档没有「删了就消失」的保证，读取接口会指向一个已不存在的对象。新版结果——刷新把消失的页从版本里摘出（`RefreshReport.removed == ["guide/gone.md"]`），`Session::find` 取不到它、`Session::paths` 不再列它；对它的 `read_doc` 返回 `isError: true`（链接即报错）；一节被删后该页锚点收敛为 `["two","keep"]`，该节正文再无命中。证据链接——`integrations/mcp/docs/tests/refresh_search.rs`：`a_deleted_page_is_out_of_the_index_and_the_results`、`a_deleted_section_leaves_no_hit_or_link`；`tests/incremental.rs` 的删除路径。
- [x] **BW-28-01-05 验证公开边界**。**验收** 私有夹具不会进入索引。
  - 证据：原版结果——引用仓库的 `private: true` 只是文档约定，没有机器可校验的公开边界，抓取器和读者一样能看到私有页。新版结果——front matter 的 `private: true` 在 `DocEntry::from_source` 直接返回 `None`，该页根本不进 `DocIndex`，因此索引路径、搜索命中、`read_doc` 三层都取不到它；边界由内容驱动而非缓存——同一页去掉标记（`private: false`）刷新后即重新出现并被检索到。证据链接——`integrations/mcp/docs/tests/public_boundary.rs` 2 例：`a_private_page_never_enters_the_index`（`guide/draft.md` 与 `zh/guide/internal.md` 两个私有夹具都不在 `DocIndex::paths` 里，全索引正文/直接体无一处含 `Vault clearance phrase`，搜索该短语返回空，`read_doc` 对私有页回 `isError`、对公开页回 `isError: false`）；`a_page_becomes_public_when_the_private_flag_is_dropped`（去掉标记后 `RefreshReport.updated == ["guide/draft.md"]`，搜索命中、`read_doc` 成功）。

#### BW-28-02 MCP 文档查询

提供文档搜索、按版本读取和链接解析，返回来源、路径、章节和版本。首发支持本地 stdio，由 VS Code 或其他 MCP 客户端接入。

- [x] **BW-28-02-01 提供 stdio MCP 会话**。**验收** 客户端能列出搜索和读取入口。
  - 证据：原版结果——引用仓库没有 MCP 面，文档只能靠人翻目录。新版结果——`src/mcp.rs` 提供 stdio 会话：JSON-RPC 2.0、每行一条消息，`initialize`（回 `protocolVersion 2025-06-18`、`serverInfo boundless-docs-mcp`、`capabilities.tools`）、`ping`、`tools/list`、`tools/call`、通知不回包、未知方法 `-32601`、坏 JSON `-32700`（id null）；工具层失败按 MCP 惯例回 `isError` 结果而非协议错误。二进制 `boundless-docs-mcp --root <工作区> --version <ref>` 直接可被 MCP 客户端启动。证据链接——`cargo test` 45 例全绿（含 `tests/mcp_session.rs` 12 例）；`tests/e2e_stdio.rs` 用真实子进程实跑一次工具调用：spawn 二进制（`--root` 指向真实 git 仓库、`--version v1`）→ 写入 initialize/notifications/tools/list/tools/call → 收 3 条回复：`serverInfo.name == boundless-docs-mcp`、`tools/list` 恰好 `["search_docs","read_doc"]`、搜索回 `Defining properties` 且 `version == v1`，进程正常退出。clippy 0 警告。
- [x] **BW-28-02-02 提供文档搜索**。**验收** 固定查询命中预期章节。
  - 证据：原版结果——引用仓库文档无搜索入口，也没有章节级索引可供定位。新版结果——`src/search.rs` 按词加权：章节标题整词 8 分 > 标题子串 4 > 页标题 2 > 正文 1；分词保留 CJK 短语为一个词（`"Cell 迁移 v2!"` → `cell / 迁移 / v2`），按路径+行号稳定排序，`limit` 钳到 1..50，空查询与未建索引的版本返回空；snippet 取正文中第一条含查询词的行、超过 140 字截断加省略号。打分与 snippet 只用章节**自己的**正文（`ExtractedSection.direct`，止于下一个任意层级标题），因此父章节不会抢走子章节的命中。证据链接——固定查询 `Flags cell` 命中 `guide/entities.md` 的 `Defining properties`（`anchor == defining-properties`、snippet 含 `Flags`、`version == v1`）；中文查询 `属性` + `lang=zh` 全部命中 `zh/` 前缀页且 `lang == zh`；`limit:1` 只回 1 条。`cargo test` 45 例全绿（`src/search.rs` 内 5 例单测 + `tests/mcp_session.rs`），覆盖率 search.rs 98%、markdown.rs 100%。
- [x] **BW-28-02-03 提供版本化读取**。**验收** 返回路径、版本、章节和源链接。
  - 证据：原版结果——引用仓库没有按版本读取文档的接口，读者只能翻当前工作区，同一路径在两个版本间无法区分。新版结果——`read_doc` 返回 `{version, commit, path, lang, title, source, sections[]}`：`version` 是 ref 标签（`v1`/`v2`/`HEAD`），`commit` 是该标签解析到的短哈希（用哈希当 version 也能读，等于按版本钉死）；`sections[]` 每项带 `title/anchor/level/line/body` 与自己的 `source` 链接；页级 `source` 指向整页。源链接由 `src/links.rs` 按 VitePress 渲染规则生成（`guide/entities.md` → `guide/entities.html`，带锚点追加 `#renaming-a-class`），`--docs-url` 给了就拼成站点绝对地址，没给就退回仓库内相对路径 `docs/...`；`anchor` 参数只回该节；缺路径、缺锚点、缺版本都回 `isError` 结果而非协议错误。证据链接——`integrations/mcp/docs/tests/read_doc.rs` 5 例：`a_read_returns_path_version_sections_and_a_source_link`（`source == https://docs.boundless.dev/guide/entities.html`、节链接 `…#renaming-a-class`、zh 页 `lang == zh` 且链接为 `…/zh/guide/x.html`）、`a_read_at_a_pinned_version_sees_that_versions_text`（v1/v2 同名页章节互不相同、按 `v1` 的 commit 哈希读结果与按标签一致）、`an_anchored_read_returns_only_that_section`、`a_read_without_a_docs_url_links_repository_relative`（`docs/guide/entities.html`）、`a_read_reports_missing_pages_and_versions_as_tool_errors`。
- [x] **BW-28-02-04 解析 VitePress 链接**。**验收** 读取结果的锚点可打开。
  - 证据：原版结果——引用仓库的文档正文里 `#anchor`、`./other.md#x` 这类 VitePress 链接只是字符串，机器无从知道它们指向哪个页面哪个锚点，读者点不到。新版结果——`src/links.rs` 的 `links_in` 解析章节正文里的 `[label](target)`：图片 `![…](…)` 不当链接，`http/https/mailto` 等外部链接原样保留；站内链接按当前页所在目录做相对归一化（`../` 上爬、`./` 平级、`/` 从文档根算起、爬到根为止），`.md` 按 VitePress 渲染规则换成 `.html`，锚点原样带上；空 target 指向本页、`#anchor` 指向本页该节。`read_doc` 的每个 section 多回 `links[]`（`label` + 可打开的 `target`），节自己的锚点链接在 `source`。证据链接——`integrations/mcp/docs/tests/open_links.rs` 2 例：`a_link_of_a_read_opens_on_the_rendered_site`（`#renaming-a-class` → `https://docs.boundless.dev/guide/entities.html#renaming-a-class`；`../cells/migration.md#steps` → `…/cells/migration.html#steps`；外链 `https://vitepress.dev` 不动；再对三个页面的全部链接做一次不变量遍历，逐条把 target 拆回页面与锚点并断言该锚点确实存在，无一悬空）、`a_link_to_an_unknown_anchor_is_seen_as_dangling`（`#no-such-section` 解析出链接但索引里查无此锚点，可被识别）。`src/links.rs` 另 4 例单测覆盖本页锚点、相对上爬下探（含 locale 目录与 `..` 越界收敛）、外链与图片保留、越界链接收敛到文档根。

#### BW-28-03 检索和发布验收

用实体属性、Cell 迁移、PostgreSQL 运维等真实问题验证命中章节及链接。CI 在索引过期、引用失效或内容超出公开文档范围时报告问题。

- [x] **BW-28-03-01 验证 EntityDef、Cell 迁移和 PostgreSQL 查询**。**验收** 三项均命中对应参考。
  - 证据：原版结果——引用仓库这三个话题（属性定义、Cell 迁移、PostgreSQL 运维）散在各自的源目录里，没有任何统一检索入口，问题只能靠人翻目录。新版结果——三个真实问题各自命中对应参考：`EntityDef Flags` 命中 `zh/guide/entities.md` 的 `定义属性`；`迁移步骤 快照` 命中 `zh/guide/cell/migration.md` 的 `迁移步骤`（同页其他章节只按打分靠后，全部命中都不越出该页）；`PostgreSQL 备份` 命中 `zh/ops/postgresql.md` 的 `PostgreSQL 备份与恢复` 且 snippet 含 `pg_dump`。命中即可用：把命中的 `path` + `anchor` + `version=v1` 直接交给 `read_doc`，回唯一一节且 `source == https://docs.boundless.dev/zh/guide/entities.html#定义属性`（CJK 锚点也能带上）。证据链接——`integrations/mcp/docs/tests/real_queries.rs` 5 例：`the_three_references_are_all_indexed`、`an_entity_def_query_hits_the_property_reference`、`a_cell_migration_query_hits_the_migration_reference`、`a_postgresql_query_hits_the_operations_reference`、`a_hit_reads_back_through_the_read_tool`，`cargo test` 全绿。如实登记：本 worktree 尚无 BW-19 的 `docs/` 产出，三个参考按 BW-19 VitePress 的 `zh/guide/…` 约定用真实 git 仓库里的合成夹具建，路径与语言分布与真实文档一致；测试只依赖 `GitSource` 与 `docs/**/*.md` 约定，`docs/` 落地后同一查询可直接跑在真实文档上。
- [x] **BW-28-03-02 接入索引时效检查**。**验收** 改文档不重建时检查失败。
  - 证据：原版结果——引用仓库没有索引这层东西，自然也没有「文档改了但索引没跟上」的检查，索引过期只能靠人肉发现。新版结果——`src/health.rs` 的 `freshness` 把已存索引和仓库当前 ref 逐页比对：commit 移动报 `moved: <旧> -> <新>`，blob 变化报 `changed: <路径>`，新增页报 `added:`，删页报 `removed:`，索引里没有该版本报 `missing version <ref>`，ref 解析不了则直接返回 Git 自己的报错。门以 `boundless-docs-mcp --check <index.json> --root <工作区> --version <ref>` 暴露：一致时打印 `the index matches <ref> (<commit>)` 且退出 0，任何一条不一致逐条打印原因并退出 1——改文档而不重建必然失败。证据链接——`integrations/mcp/docs/tests/freshness.rs` 6 例（用真实子进程实跑 `--check`）：`an_index_that_matches_its_ref_passes`（退出 0、stdout 含 `matches HEAD`）、`a_document_change_without_a_rebuild_fails`（退出 1、stderr 含 `changed: guide/entities.md`）、`a_document_added_without_a_rebuild_fails`（`added: guide/cells.md`）、`a_document_removed_without_a_rebuild_fails`（`removed: guide/entities.md`）、`a_version_the_index_never_saw_fails`（`missing version v2`）、`an_unresolvable_ref_fails_with_what_git_says`（`reference v9`）；`src/health.rs` 另 3 例单测覆盖「重建后是新的」「未重建的编辑既 changed 又 moved」「索引缺该版本」。
- [x] **BW-28-03-03 接入链接及公开范围检查**。**验收** 坏链接和越界样本被识别。
  - 证据：原版结果——引用仓库的文档没有链接检查，也没有公开范围检查，坏链接和越界内容无人发现。新版结果——`health::scope` 逐页逐节解析站内链接（外链与图片跳过），把链接目标解析回 `(页面, 锚点)`：页面不在索引报 `no such page`；页面在但无此锚点报 `no such anchor`；页面在索引里但其源文件标了 `private: true`（越界样本）报 `private page indexed`。报告行形如 `guide/near.md > Here: no such page at docs/guide/gone.html`。链接解析只取章节**自己的**文本（`SectionEntry.direct`），父章节不再重复统计子节的链接。`--check` 把时效与链接/公开范围两类问题一起报，全干净才退出 0，任一问题退出 1。证据链接——`integrations/mcp/docs/tests/scope_gate.rs` 3 例真实子进程：`a_link_to_a_missing_page_fails_the_gate`（退出 1、stderr 含 `no such page at docs/guide/gone.html`）、`a_link_to_a_missing_anchor_fails_the_gate`（退出 1、含 `no such anchor at docs/guide/entities.html#nope`）、`a_link_that_opens_passes_the_gate`（退出 0、stdout 含 `the index matches HEAD`）；`src/health.rs` 另 4 例单测：`a_clean_scope_passes`、`a_link_to_a_missing_page_is_reported`、`a_link_to_a_missing_anchor_is_reported`、`a_private_page_in_the_index_is_reported`（手搭索引塞入 `private: true` 页后 `scope` 报 `private page indexed: guide/secret.md`）。
  - 本 crate 覆盖门（BW-28 全 12 项交付后补测）：`cargo llvm-cov`（llvm-cov 0.9.1）**TOTAL 99.10% 行**（1336 行 / 12 未覆盖，区域 98.72%、函数 98.67%），过 ≥98% 门；逐文件 errors 100 / markdown 100 / index 100 / health 99.32 / mcp 98.97 / links 99.48 / search 98.37 / source 98.41 / main 97.20。新增测试：`src/errors.rs` 4 例（四种 `DocsError` 的 `Display`、`From<io::Error>`/`From<serde_json::Error>`、`git_exit` 空 stderr 只报退出码与带 stderr 带原文两路）、`src/search.rs` 2 例（语言过滤只留本语言树、结尾术语不被吞）、`src/links.rs` 2 例（`.md` 源路径不再叠后缀、空链接目标指回本页）、`src/health.rs` 2 例（站外链接不视为文档链接、多条结论按页面与目标排序）、`tests/mcp_session.rs` 4 例（`ServerConfig::default()` 与命令行默认一致、无 method 回 `-32600`、`read_doc` 缺 path 报工具错误、请求间空行不产生回复）、`tests/cli.rs` 9 例（未知旗标与缺值旗标退出 2、版本不可解析不服务、`--check` 缺文件与索引不可读均失败、全新索引与所建版本一致退出 0、`--langs` 决定索引哪棵树、`--prefix`+`--docs-url` 贯穿到源链接、读不到输入即以错误结束会话）、`tests/tree_listing.rs` 1 例（以假 git 喂坏列表：无 tab 行、无 blob id 行、非 `.md` 行全部跳过，只留 `guide/good.md`）。`cargo test` 108 例全绿、`cargo clippy --all-targets --all-features` 0 诊断。如实登记余下 12 行的归属（health.rs 与 mcp.rs 行覆盖已 0 遗漏）：`src/main.rs:55` 是 `match` 里写死的 `unreachable!` 臂（旗标已在上一处穷尽，不可达），`main.rs:161-162` 是 `serve` 写入 stdout 失败才走的分支（输入侧错误已由 `an_input_that_cannot_be_read_ends_the_session_with_an_error` 钉死）；`src/search.rs:39` 为空查询早退、`search.rs:134` 与 `links.rs:68` 为词法循环的回边；`src/source.rs:89` 是列表行缺分隔符时的 `continue`（`tree_listing` 的夹具已喂该行）。

### BW-29 KBEngine 差异和可选能力复核

按前面的 KBEngine 清单逐项记录「已有等价、列入后续实现、暂缓」及依据。确认列入后给每个新增能力分配实施编号和验收样例。

#### BW-29-01 地图导航差异

验证 KBEngine `navigation/` 中的 Recast/Detour、平铺数据及 CellApp 异步加载是否在当前构建与脚本入口可达。用跳点、跨 tile、不可达及异步加载期的地图夹具与 BW-10 对照，记录旧 KBEngine 导航资源导入是否进入范围。

- [x] **BW-29-01-01 核对 Recast/Detour 构建及脚本入口**。**验收** 记录可达性及目标。
  - 证据：Recast/Detour 在 KBEngine 是 **vendored 源码内嵌**，非外部依赖——`kbe/src/lib/navigation/` 45 个文件（`Recast{,Contour,Mesh,MeshDetail,Rasterization,Region,Filter,Layers,Alloc,Area,Assert}.*`、`Detour{NavMesh,NavMeshQuery,NavMeshBuilder,TileCache,TileCacheBuilder,Crowd,ObstacleAvoidance,ProximityGrid,PathCorridor,PathQueue,LocalBoundary,Node,Alloc,Assert,Common}.*`，KBEngine 自有薄层 `navigation.*`、`navigation_handle.*`、`navigation_mesh_handle.*`、`navigation_tile_handle.*`、`findpath_9tile_bak.cpp`，`Readme.txt` 记 Recast & Detour Version 1.4, 2009-08-24）。构建入口两份且**无条件编入、无独立开关**：`kbe/src/lib/CMakeLists.txt:1996-2031`（`add_library(kbe_navigation STATIC ...)` 逐文件列出 + `KBEngine::navigation` ALIAS）、`kbe/src/lib/navigation/Makefile`（`LIB=navigation`，末尾 `include .../common.mak`）；消费方 `kbe/src/CMakeLists.txt:197`（`kbe_cellapp` 链 `KBEngine::navigation`）；开关核查：`cmake/KBEngineOptions.cmake` 仅 8 个 option（KBE_ENABLE_WATCHERS/USE_OPENSSL/USE_LOG4CXX/USE_JEMALLOC/BUILD_BOOTSTRAP_ONLY/ENABLE_TESTING/ENABLE_CODE_COVERAGE/BUILD_LEGACY_XML_TARGET）无 navigation 项，全仓 `find_package` 无 Recast/Detour 命中，`CMakePresets.json`、`DEPENDENCY_UPGRADE_PLAN.md` 亦无，唯一 submodule 是 `BigWorld-Engine-14.4.1`。**脚本入口不可达**：该 worktree 是引擎仓（`kbe/{bin,res,src,tools}` + `typings/`、`docs/`、`tools/`、`tests/`），无 `assets/`、`interfaces/` 目录，也无 `recastnavigation.py`/`navigation_tile.py`/`navigation_mesh.py`/`engine.py`；脚本面仅存于 typing 桩 `typings/_KBEngine_common.pyi:129`(`canNavigate`)/`:139`(`navigate`)/`:150`(`navigatePathPoints`)，矩阵在 `typings/API_MATRIX.md:323`（签名源 `server/cellapp/entity.cpp:52-64`）；C++ 可达链 `cellapp/entity.cpp:2588-2599`（`Entity::navigatePathPoints()` → `pSpace->pNavHandle()` → `findStraightPath(...)`）。**目标记录**：导航生成与运行时在 Boundless 已由 BW-10-05（生成）、BW-10-04（waypoint 寻路）、BW-10-06（cellapp/Python 寻路）承载且均待办，故本条判定与 BW-10 同源、列入后续，不新增范围。
- [ ] **BW-29-01-02 对照 KBEngine 平铺导航查询**。**验收** 跨 tile 与不可达样本有结果。（参考面已定位：平铺查询为 `kbe/src/lib/navigation/navigation_tile_handle.cpp:325`（`listPathRes(wspath, L"tmx", results)`）+ `findpath_9tile_bak.cpp`；依赖平铺导航查询与 tile 缓存实现未建（属 BW-10-06 cellapp/Python 寻路），跨 tile 与不可达样本夹具需其承载，随 BW-10-06 立项解锁）
- [ ] **BW-29-01-03 对照异步地图加载**。**验收** 加载期调用状态有证据。（参考面已定位：异步加载在 `kbe/src/server/cellapp/loadnavmesh_threadtasks.cpp`，与 `navigate_handler.cpp`、`spacememory.cpp` 同组；依赖 CellApp 空间与导航句柄实现未建（属 BW-10-06/BW-10-04），加载期调用状态需真实运行取证，随其立项解锁）
- [x] **BW-29-01-04 记录旧导航资源导入决策**。**验收** 列明纳入或暂缓及理由。
  - 证据：两侧都**不存在独立导入/转换脚本**，故无脚本可纳入。KBEngine 是运行时二进制直读——`kbe/src/lib/navigation/navigation_mesh_handle.cpp:428`（`Resmgr::getSingleton().listPathRes(wspath, L"navmesh", results)`）→ `:443` `_create(layer++, ...)` 逐文件读，`tryReadNavmesh()`（`:473` 起）按 `NavMeshSetHeader{version, tileCount, dtNavMeshParams}` + `NavMeshTileHeader{tileRef, dataSize}` 直读，版本常量 `RCN_NAVMESH_VERSION = 1`；tile 版 `navigation_tile_handle.cpp:325`（`listPathRes(wspath, L"tmx", results)`）；仓内无任何 `*.py` 提及 navmesh/detourtilecache（零命中），生成端由外部 recastnavigation 插件导出（`docs/api/kbengine/cellapp/KBEngine.md:95`）。BigWorld 侧是**工具端生成器而非导入器**——`tools/common/recast_processor.cpp:23`（`RecastGenerator::collectTriangles(chunk)`）、`:140`（`navmeshCache_->calcDone(...)`），离线入口 `tools/offline_processor/command_process_chunks.cpp:492-506`（`space.settings` 的 `navmeshGenerator` 只接受 `"recast"`），旧资源往返靠 `editor_chunk_navmesh_cache_*` + `navmesh_processor.*`。**决策：暂缓**（不纳入本轮）——没有独立导入脚本可对齐，两侧语义分别是「运行时直读二进制 navmesh」与「从 chunk 几何重算」，Boundless 的对应验收已在 BW-10-05-05「验证旧导航资产读取」与 BW-10-05-06「验证新产物往返」承载，本条不重复立项；若届时需要 KBEngine navmesh 二进制格式兼容，作为 BW-10-05-05 的子验收登记，不在此开新任务。

#### BW-29-02 运维工具差异

按 `kbcmd`、GUIConsole、WebConsole、pycluster 分别核对进程发现、日志、状态、profile 和集群管理，记录对应 BW-23~27 或可选独立控制台需求。

- [x] **BW-29-02-01 复核 kbcmd**。**验收** 为已确认命令关联任务或记范围项。
  - 证据：入口 `kbe/src/server/tools/kbcmd/main.cpp`（`main()` 461 行、命令分派 473-481 逐一字符串匹配，无第三方框架），全部子命令仅四个：`--clientsdk=<unity|ue4>`（`process_make_client_sdk` 139 行，支持 `--outpath=`/`--zip=`/`--tar=`）、`--getuid`（427 行，取服务器组 UID）、`--newassets=python`（301 行，工程脚手架）、`--help`（439 行）；同目录 `profile.cpp` 仅 11 行，只是 PythonApp 定时基类挂钩，**无 profile 命令**。据此纠正 KB-11 条目预设：**kbcmd 是纯代码生成/脚手架工具，不承担进程发现、日志、状态或 profile**——后者在 KBEngine 由 pycluster + pycommon 承担。关联任务：`--clientsdk` → BW-21（多引擎客户端 SDK）与 BW-29-04-01；`--newassets` → BW-29-05-03 脚手架（模板树随 BW-03）；`--getuid` 依赖 KBEngine 服务器组 UID 概念，Boundless 无对应物，**记范围项不立项**。补充：四项工具在 BigWorld 原版均不存在（`tools/` 只有 asset/editor 工具），是 KBEngine 自有增量，本条复核定位正确。
- [x] **BW-29-02-02 复核 GUIConsole**。**验收** 为状态及管理工作流记录差距。
  - 证据：入口 `kbe/src/server/tools/guiconsole/guiconsole.cpp` 为 MFC Win32 桌面程序，**仅 Windows**（启动脚本 `kbe/tools/server/guiconsole/!(win)start.bat`）；Tab 创建见 `guiconsoleDlg.cpp:429-447`，功能模块按窗口成组：状态 `StatusWindow.cpp`(397 行)、日志 `LogWindow.cpp`(706)、Profile `ProfileWindow.cpp`(494)、Watcher `WatcherWindow.cpp`(579)、空间视图 `SpaceViewWindow.cpp`、图表 `GraphsWindow.cpp`+`LineChartCtrl.cpp`（tick/负载曲线）、调试与远程 `DebugWindow.cpp`/`ConnectRemoteMachineWindow.cpp`、进程启动布局 `StartServerWindow.cpp`/`StartServerLayoutWindow.cpp`/`SetLayoutNameWindow.cpp`/`TimingLengthWindow.cpp`。Boundless 侧运维界面在 VS Code 插件（BW-20）：`extensions/vscode/src/workspace/cluster.ts` 的 `CLUSTER_ROLES`(13 行) 与 `ClusterControl.roles()`(77/83 行，返回 `{role, pid, commandLine, logPath}`)、`extension.ts` 的 `showClusterStatus`/`showClusterLogs`，覆盖进程发现+状态+日志查看。**差距登记**：GUIConsole 的状态窗口、日志窗口、Profile 窗口有插件侧等价面（对应 BW-23/BW-24/BW-25 各自章节）；三项无对应物——tick/负载曲线图、Cell Viewer 空间视图、远程机器连接——**记范围项**，随 BW-25 进程与集群运行状态立项时决定是否纳入，本轮不另立任务。
- [x] **BW-29-02-03 复核 WebConsole**。**验收** 为日志、状态和 profile 记录差距。
  - 证据：入口为 Django 工程 `kbe/tools/server/webconsole/`（`manage.py` + `run_server.sh/.bat`），路由挂 `KBESettings/urls.py:20`（`^wc/` → `WebConsole.urls`），端点清单在 `WebConsole/urls.py:18-69`——日志组（`views_log.py`）`/log/connect`(websocket 实时日志)、`/log/process_cmd`、`/log/connect/pull`；状态组（`views_status.py`）`/status/show_components`、`/status/connect`、`/status/process_cmd`；Profile 组（`views_profile.py`）`/profile/show_components`、`/profile/connect`、`/profile/process_cmd`；其余为组件管理/启停/布局、Telnet 控制台 `views_console.py`、Watcher `views_watcher.py`、空间视图 `views_spaceviewer.py`、用户权限 `auth.py`，依赖 `machines_mgr.py` 与 `dwebsocket/`。差距：日志、状态两组在插件侧 `showClusterLogs`/`showClusterStatus` 有部分等价；Profile 组无等价物（BW-23 Profiler 面向性能分析，非浏览器可达查询）；组件启停/布局、Telnet 控制台、Watcher、空间视图、用户权限**均无对应物**。**判定**：WebConsole 的整体形态（Django+WebSocket 浏览器控制台）**记范围项**，是否纳入随 BW-25 立项决定；本轮不登记为独立任务，不写兼容验收。
- [x] **BW-29-02-04 复核 pycluster**。**验收** 为集群管理工作流判定范围。
  - 证据：单文件控制器 `kbe/tools/server/pycluster/cluster_controller.py`（`__main__` 741 行），集群工作流即其 Handler 清单——状态查询 `ClusterQueryHandler`(88，按 UID 查全网机器进程)、启动 `ClusterStartHandler`(146，按模板+机器 IP+gid 起整套进程) 与 `ClusterStartServerHandler`(559，起 machine 守护)、停止 `ClusterStopHandler`(210，`sendStop` 222)、日志 `ClusterLogWatchHandler`(327)/`ClusterSendLogHandler`(368，本地日志注入)、布局保存与加载 `ClusterSaveProcessHandler`(406)/`ClusterLoadProcessHandler`(460，即 role/进程布局下发)、Flags 下发 `ClusterSetFlagsHandler`(617，`sendSet` 627)；CLI 动词 `start`/`stop`/`startserver`/`save`/`load`/`setflag`/默认 query（741-820 行），外部引用见 `sdk_templates/server/python_assets/safe_kill.sh:31`（`stop $XUID`）；公共层在 `kbe/tools/server/pycommon/`（`Machines.py`、`Component_Status.py`、`LoggerWatcher.py`、`ServerApp.py`、`Watcher.py`、`SpaceViews.py`）。Boundless 侧 `crates/`、`apps/` 全量 grep `kbcmd|guiconsole|webconsole|pycluster|cluster|process_tree|log_tail` 为空，无集群控制器代码。**范围判定**：单机进程发现/状态/日志已由 BW-20 插件侧覆盖并归 BW-25 收口；跨机集群启停、Flags 下发、布局保存加载三项**记范围项**，作为独立控制台/集群管控需求随 BW-25 立项评估，本轮不立项。

#### BW-29-03 存储与外部服务差异

分别复核 Redis 后端、`interfaces` 外部服务、订单/回调和邮件辅助的适用场景及启用条件。若纳入，逐项定义兼容性和端到端验收。

- [x] **BW-29-03-01 复核 Redis 后端**。**验收** 记录构建可达性及纳入决策。
  - 证据：KBEngine 的 Redis 后端是**自有纯 C++ 实现**（非 BigWorld 能力）：源文件 `kbe/src/lib/db_redis/`（`db_interface_redis.h`（`class DBInterfaceRedis : public DBInterface`，头注释给出 `tbl_Account:1 = hashes(...)` 的键设计）、`db_interface_redis.cpp`、`entity_table_redis.h/.cpp/.inl`、`kbe_table_redis.h/.cpp`、`db_transaction.cpp`、`redis_watcher.cpp`、`redis_helper.h`、`db_exception.cpp`）。构建可达性：**该树已无 SConscript**（`find` 零命中，仅 CMake）——`kbe/src/lib/CMakeLists.txt:2114-2124` 定义 `kbe_db_redis` 静态库 + `KBEngine::db_redis` alias，`:2126-2132` 链 `KBEngine::dependency_hiredis`，`:2134-2137` `target_compile_definitions(... KBE_USE_REDIS)`，`:2155-2163` 的 `kbe_db_runtime` 同时链 `KBEngine::db_mysql` + `KBEngine::db_redis`；外部依赖 hiredis 在 `vcpkg.json:14` 声明并有 vendored 副本 `kbe/src/lib/dependencies/hiredis/`。运行时由配置字符串选择：`db_interface/db_interface.cpp:145-156`（`strcmp(db_type,"mysql")`/`"redis"` → `new DBInterfaceRedis(name)`）、`:215` `initInterface`；配置入口 `res/server/kbengine_defaults.xml:330-331`（`databaseInterfaces/<default>` 注释「数据库类型（mysql、redis）」）。BigWorld 侧 `\bredis\b` 与 `hiredis` 命中均为 **0**。纳入决策：**暂缓**——Redis 非原版能力，Boundless 主存储由 BW-09-06 PostgreSQL 承载（含事务/并发锁/连接恢复），如后续需要 Redis 作为可选后端，在 BW-09 的存储后端扩展点上评估，本轮不立项。
- [x] **BW-29-03-02 复核 Interfaces 外部服务**。**验收** 记录入口、用途及取舍。
  - 证据：Interfaces 是**独立网关进程**（C++ + Python 脚本驱动）：进程代码 `kbe/src/server/tools/interfaces/`（`interfaces.cpp/.h`、`interfaces_tasks.cpp`、`interfaces_interface.cpp`、`orders.cpp/.h`、`profile.cpp`、`main.cpp`、`Makefile`）；进程配置 `res/server/kbengine_defaults.xml:234-270` 的 `<interfaces>`（`entryScriptFile=kbemain`、`host=localhost`、`port_min=30099`、`port_max=30199`、`orders_timeout=3600`、`telnet_service/port=33000`）；dbmgr 侧转发开关 `kbengine_defaults.xml:296-313` 的 `<InterfacesServiceAddr>`（`enable`/`addDefaultAddress`/`<item><host>+</port>` 地址池），结构体在 `kbe/src/lib/server/serverconfig.h`（`getInterfaces()`、`getDBMgr().InterfacesServiceAddr`；另 `serverconfig.h:165` 的 `externalInterface` 是**对外网卡名/IP**，与外部服务无关）。用途实现：`kbe/src/server/dbmgr/interfaces_handler.cpp:262`(`createAccount`)/`:361`(`loginAccount`)/`:544`(`charge`)/`:585`(`onChargeCB`)，脚本侧注册 `interfaces.cpp:186-189`（`chargeResponse`、`accountLoginResponse`、`createAccountResponse`、`executeRawDatabaseCommand`），脚本入口模板 `sdk_templates/server/python_assets/scripts/base/kbemain.py`。取舍：**不照搬**——它的存在是为了对接第三方平台（账号创建/登录、订单 charge、原始 SQL），属运营侧扩展而非原版兼容要求（BigWorld 无对应物）；Boundless 侧的账号/Billing 扩展点在 BW-09-04「Billing 与扩展」，如需第三方平台对接，在该章节下按「可关闭的适配器」形态立项，本轮记范围项。
- [x] **BW-29-03-03 复核订单与回调**。**验收** 记录端到端路径和范围。
  - 证据：端到端路径已逐段定位——Python `KBEngine.charge()` → `kbe/src/server/baseapp/baseapp.cpp:3495` `__py_charge`（参数校验 `ordersID, dbid, datas, pycallback`）、`:3577` `Baseapp::charge`、`:3590` 发 `DbmgrInterface::charge`（声明 `baseapp.h:399`）→ Dbmgr `kbe/src/server/dbmgr/dbmgr.h:179,184`、`interfaces_handler.cpp:544` `InterfacesHandler_Interfaces::charge`（`:575` 打包 `InterfacesInterface::charge` 发给 interfaces 进程；空实现对照 `:117-125` 的 `InterfacesHandler_Dbmgr::charge/onChargeCB = "no implement!"`）→ Interfaces 侧记账 `tools/interfaces/orders.h:19-46`（`address/ordersID/cbid/dbid/baseappID/dbmgrID/state(FAILED|SUCCESS)/postDatas/getDatas/timeout`、`OrdersCharge`），`interfaces.cpp:465`(`eraseOrders`)/`:477`(`hasOrders`)/`:512`（写 `postDatas`）/`:563`（`appendBlob(task->postDatas)` 外发），`:186` 注册 `KBEngine.chargeResponse` → 回调接收 `interfaces_handler.cpp:585` `onChargeCB` → `baseapp.cpp:3609` `Baseapp::onChargeCB`（`:3632` 构造 Python 回调）。LoginApp 侧的 HTTP 回调服务是另一条通路：`kbengine_defaults.xml:793-797` 的 `http_cbhost`/`http_cbport=21103`（注释「http回调接口，处理认证、密码重置等」），`loginapp/http_cb_handler.h:21-23`（`onAccountActivated`/`onAccountBindedEmail`/`onAccountResetPassword`）、`http_cb_handler.cpp:32`（绑定端口）、`:316-335`（回填 `backlink_hello_message`）、`:344/370/396`（三个回调实现）、`:296-306`（`DbmgrInterface::accountBindMail` 转发）。BigWorld 侧对照：`\borders\b` 7 个文件全部是可靠消息顺序字段（`lib/network/udp_bundle.cpp:59-60,87-88` 的 `reliableOrders_`/`reliableOrdersExtracted_`、`lib/cstdmf/main_loop_task.cpp:460` 注释），`\bpurchase\b` 为 0——订单/购买在原版不存在。范围：**记范围项，随 BW-09-04「Billing 与扩展」立项**，本轮不落地、不写兼容验收。
- [x] **BW-29-03-04 复核邮件辅助**。**验收** 记录依赖和范围。
  - 证据：邮件辅助是独立的 curl/SMTP 线程任务，代码 `kbe/src/lib/server/sendmail_threadtasks.h`（`SendEMailTask` 基类含 `smtpUrl/fromAddress/toAddress/messageHtml/useSsl`，`:66` `SendActivateEMailTask`、`:88` `SendResetPasswordEMailTask`、`:110` `SendBindEMailTask`）与 `sendmail_threadtasks.cpp`（`:6` `#include "curl/curl.h"`、`:50-56` `buildSmtpUrl` 生成 `smtp://`/`smtps://`(465)、`:77` `useSsl`、`:94-98` `smtp_auth` 分支）；触发点 `baseapp.cpp:5729`（`new SendBindEMailTask`）、`loginapp.cpp:669`（`new SendActivateEMailTask`）、`loginapp.cpp:825`（`new SendResetPasswordEMailTask`）、`dbmgr/dbtasks.cpp:1310`（`bindEMail`）。依赖：运行时 **libcurl** + SMTP 配置；配置入口 `kbengine_defaults.xml:232` 引入 `kbe/res/server/email_service_defaults.xml`，SMTP 字段解析 `serverconfig.cpp:1589-1609`（`smtp_server`/`smtp_port`/`smtp_auth`）、`serverconfig.h:63-67`；回调落在 LoginApp 的 `http_cb` HTTP 服务（见 BW-29-03-03）。BigWorld 侧对照：`\bsmtp\b` 为 0，`\bmail\b` 5 个文件全部是实体 mailbox 消息通道（`lib/entitydef/mailbox_base.hpp/.cpp`、`server/baseapp/client_entity_mailbox_wrapper.cpp`、`lib/input/key_code.hpp/.cpp`），与邮件发送无关。范围：**暂缓**——账号激活/重置密码/绑定邮箱的邮件通知是运营侧增值能力，Boundless 无对应计划；如纳入需先有 BW-09-04 的账号体系与 libcurl 依赖登记，本轮不立项。

#### BW-29-04 SDK 和 Python 开发差异

分别复核在线 SDK 下载、JavaScript 模板、Python assets 模板、类型存根。比对 BW-21/27 已覆盖工作流后登记剩余项。

- [x] **BW-29-04-01 复核 LoginApp SDK 下载**。**验收** 登记支持传输与范围。
  - 证据：KBEngine `loginapp/clientsdk_downloader.cpp`（`.h` 50 行/`.cpp` 288 行）全库无 HTTP/FTP/OBS 分支——唯一传输是把已登录的自有网络通道当下载通道：`loginapp.cpp:1540-1575` 收 `importClientSDK` 后 fork/exec `kbcmd --clientsdk=` 本地生成到 `{assets}/_tmp/{options}`（Windows 走 CreateProcessW、Linux 走 fork+execv），再按 `clientWindowSize` 分片沿 `Network::Channel` 推回（`clientsdk_downloader.cpp:227-284`，60 秒超时自删）；产物为 `sdk_templates/client/{unity|ue4}` 生成的客户端 SDK 源码目录（kbcmd `--zip=`/`--tar=` 可打包，`kbcmd/main.cpp:207-275`）。判定：**暂缓照搬**——在线推送深度绑定 KBEngine 自有 Channel 协议与 kbcmd 生成器，Boundless SDK 交付走 BW-21 多引擎发布矩阵（离线产物）；如未来需要在线推送，在 BW-21-06 SDK 发布与矩阵下另立任务，不在此复刻。
- [x] **BW-29-04-02 复核 JavaScript SDK 模板**。**验收** 登记兼容性需求与取舍。
  - 证据：KBEngine js 侧模板仅 2 个文件（`kbe/res/sdk_templates/client/js/kbengine.js` + `README.md`），是静态手写的浏览器 JS 客户端库；`kbcmd/client_sdk.cpp:48-64` 的 `createClientSDK()` 只认 `unity`/`ue4`（分别 `ClientSDKUnity`/`ClientSDKUE4`，其余返 NULL），无 `"js"` 分支——故 js SDK 不参与代码生成、与实体定义改动不同步。兼容性需求登记：若纳入，必须先接入 BW-21-01 公共核心 SDK 的生成管线（否则每次实体变更都要手改 js）；取舍：**暂缓纳入**，按「仅示例、明确不同步」定位收录，不作为兼容验收项；对照 BigWorld 原版 `grep -rli "sdk_template|clientsdk"` 零命中，确认此为 KBEngine 侧新增能力。
- [x] **BW-29-04-03 复核 Python assets 模板**。**验收** 登记已有工作流和缺口。
  - 证据：KBEngine 脚手架是 `kbcmd --newassets=python`（`kbcmd/main.cpp:301-330`，分派 476 行；根入口 `new_assets.sh`/`.bat`）从 `kbe/res/sdk_templates/server/python_assets/` 整树复制，真实清单含启动脚本 `start_server*.bat/.sh`、`kill_server`、`safe_kill`、`guiconsole.bat`、`gensdk.sh`、`start_bots`、配置 `res/server/kbengine.xml`/`server_errors.xml`、脚本 `entities.xml`、`entity_defs/{Account.def,types.xml}`、`{base,bots,cell,db,interface,logger}/kbemain.py`、`interface/Poller.py`、`common/{Functor,KBEDebug}.py` 及各占位目录 `.gitignore`。已有工作流对照：Boundless 服务端 python 布局与 `python/` 模块目录由 BW-03 承载（现 `python-host` 为 4 行空壳、`python/` 目录未建），故模板树与脚手架命令归 BW-03 立项；缺口登记：工程脚手架入口（新工程目录生成）、`entity_defs` 模板、`start/kill` 运维脚本三项均无对应物，暂不另立，随 BW-03 python 布局一起交付。
- [x] **BW-29-04-04 复核类型存根**。**验收** 登记生成及 IDE 接入范围。
  - 证据：KBEngine 的 `.pyi` 是**旁路静态桩**（adjacent 模式，与 `.py` 同名同目录、运行时不导入），由 `tools/generate_kbengine_entity_stubs.py`（`generate_outputs()` 1117-1206、`write_outputs()` 1209-1232，默认 `--scripts-dir kbe/res/sdk_templates/server/python_assets/scripts`、`--mode adjacent`、`--check`）从 `entities.xml`/`entity_defs` 生成，产物落 `typings/`（`_KBEngine_common.pyi`、`_KBEngine_{baseapp,cellapp,loginapp,dbmgr,logger,bots,interfaces}.pyi`、`KBEngine/{__init__.pyi,hooks.pyi}`、`stubs/`、`examples/`）与 `scripts/{side}/<Entity>.pyi`、`components/<Component>.pyi`；配套 `generate_kbengine_server_errors_stub.py`（`server_errors.h` → `SERVER_ERR_*`）、`validate_kbengine_typing.py`、`build_kbengine_typing_bundle.py`（`COMPONENT_NAMES = base/cell/interface/login/db/logger/bots`）与 `install_kbengine_typing_bundle.py`。IDE 接入靠仓库根 `pyrightconfig.json` 的 `stubPath: typings/stubs` + 7 组件与 6 examples 的 `executionEnvironments`/`extraPaths`、`typeCheckingMode: basic`，**不塞进 client_sdk**。范围登记：存根生成器与旁路桩体系为 KBEngine 特有，Boundless 的 IDE 接入面是 BW-27-01 LSP（`integrations/` 目前仅 `mcp`，无 LSP/DAP 落地）；生成管线依赖 `entity_defs`，属 BW-03/BW-27-01 的公共依赖——**列入后续**，在 BW-27-01 LSP 立项时一并定义「从实体定义生成类型信息」，不在本轮单独立项。

#### BW-29-05 工作流差异

分别复核 `xlsx2py`、安装脚本和新工程脚手架。确认是否纳入，并为选中的功能登记样例输入、输出与平台。

- [x] **BW-29-05-01 复核 xlsx2py**。**验收** 记录代表性表格及输出和范围。
  - 证据：转换器在 `kbe/tools/xlsx2py/xlsx2py/xlsx2py.py`（用法 `python xlsx2py <data_file> <excel_file>`），配套 `ExcelTool.py`、`xlsxtool.py`、`xlsxError.py`、`config.py`、`py2excel.py`（反向）与表域后处理器 `character.py`、`itemchar.py`、`petchar.py`、`syschar.py`、`functions.py`。输入约定（`config.py`）：`EXPORT_PREFIX_CHAR = '@'`（**sheet 名以 `@` 前缀才导出**）、`EXPORT_DEFINE_ROW = 1`（表头第 1 行）、`EXPORT_KEY_NUMS = 1`（首列主键）、`EXPORT_MAP_SHEET = '映射表='`、单元格签名 `.`=非空校验/`$`=替换/`!`=主键标记（`xlsx2py.py:401-403` 绑定 `isNotEmpty`/`needReplace`/`isKey`）。输出：`openFile()`（`xlsx2py.py:379`）`codecs.open(self.outfile,"w+",'utf-8')` 写**单个 UTF-8 `.py` 数据模块**，`main()` 支持一个输出 py 对多个 xlsx。代表性表格（`kbe/tools/xlsx2py/rpgdemo/xlsxs/`）：`spaces.xlsx`、`dialogs.xlsx`、`skills.xlsx`、`entities.xlsx`、`avatar_init.xlsx`，驱动脚本 `rpgdemo/{all,basePath,entities,dialogs,skills,spaces,avatar_init}.bat` + 清单 `rpgdemo/nfiles`，示例命令 `python ../xlsx2py/xlsx2py.py pydatas/d_entities.py xlsxs/entities.xlsx`。平台：已从 win32com/MS Office 迁到 `openpyxl`（`readme.txt` L3-4、`ExcelTool.py` L4/L15），跨平台纯 Python 3，脚本本身仍 `gb2312` 编码。**范围：暂缓**——BigWorld 原版无表格转码工作流（`find -iname "*xlsx*|*csv*"` 在非 third_party 区零命中，资产描述是 XML：`tools/res_packer/xml_packer.cpp`、`packer_helper.cpp:78` `AutoConfig::configureAllFrom("resources.xml")`、`entities.xml` + `entity_defs/*.def`），策划表驱动在原版不存在，Boundless 的实体/资源定义走 BW-13 资源管线；如后续需要 xlsx 策划表导入，按独立工具立项，不在此登记兼容项。
- [x] **BW-29-05-02 复核安装脚本**。**验收** 登记与 BW-14/15 的重合及差距。
  - 证据：KBEngine 14.4.1 的"安装"由三段组成——CMake 构建（根 `CMakeLists.txt:1` `cmake_minimum_required(VERSION 3.21)`、`:7-8` `CMAKE_CXX_STANDARD 17`/`_REQUIRED ON`，另有 `kbe/src/CMakeLists.txt` 与 `kbe/src/kbengine.sln`）、**vcpkg manifest 依赖清单**（`vcpkg.json`：apr、apr-util、curl、fmt、gtest、libmariadb、openssl、python3、hiredis、tinyxml2、utfcpp、zlib + `vcpkg-ports/`、`vcpkg-configuration.json`、`CMakePresets.json`）、shell 包装器（`new_assets.sh` 推导 `KBE_ROOT` 并设 `KBE_RES_PATH`/`KBE_BIN_PATH` 后调 `kbcmd --newassets=python`）。**全库已无 SConstruct**（`find -iname SConstruct` 计数 0）。BigWorld 原版**无 install.sh**：顶层 `BigWorld/scripts/` 只有构建与打包脚本（`build-{cstdmf,entitydef,network,python,resmgr}-cmake.sh`、`build-windows-server.ps1`、`package-daily.sh`、`package-daily-windows.ps1`、`check-docs-links.py`），安装说明是 PDF（`docs/pdf/Server Installation Guide.pdf`、`Server Build Guide.pdf`，`README.md:68-69` 引用），构建产物为 `bigworld-{bwmachined,devel,web-integration}-14.4.1.el7.x86_64.rpm`。**重合**：Boundless 的构建/平台入口是 `rust-toolchain.toml`（`channel = "1.100.0"`）+ workspace `Cargo.toml`（依赖方向注释 BW-16-01），发行面归 BW-14-04 可安装发布物、平台面归 BW-15-01 平台接口，两者已由编号覆盖。**差距登记**：依赖清单形态从 vcpkg manifest 换成 Rust workspace 依赖（已在 Cargo.toml 内以注释登记），安装文档从 PDF 换成 `docs/` 文档站（BW-19）——均无需新增任务；本轮不立项。
- [x] **BW-29-05-03 复核 assets 脚手架**。**验收** 登记可复用输入输出及范围。
  - 证据：入口 `kbcmd --newassets=python --outpath=...`，实现 `kbe/src/server/tools/kbcmd/main.cpp:301` `process_newassets()`（命令注册 476 行、帮助 452-454 行）；模板源 `main.cpp:312` → `kbe/res/sdk_templates/server/python_assets/`，类实现 `server/tools/kbcmd/server_assets.cpp`（`ServerAssets::create()` 用 `Resmgr::matchPath("sdk_templates/server/" + name())` + `copyAssetsSourceToPath()` 整树复制）；模板树内容 `README.md`、`gensdk.{bat,sh}`、`guiconsole.bat`、`kill_server.{bat,sh}`、`safe_kill.{bat,sh}`、`start_server.{bat,sh}`、`start_server_background.{bat,sh}`、`start_bots.{bat,sh}`、`res/`、`scripts/`；默认配置来自 `kbe/res/server/{kbengine_defaults.xml,email_service_defaults.xml,log4cxx_properties_defaults,messages_fixed_defaults.xml,server_errors_defaults.xml}` + `kbe/res/scripts/`、`kbe/res/client/`。**可复用输入输出登记**：输入 = 引擎树内 `sdk_templates/server/python_assets/` 模板 + `kbe/res` 默认配置；输出 = 一份新工程目录树（脚本 + 配置 + 占位 `.gitignore`），**无代码生成**。对照：BigWorld 无 `--newassets`，唯一近似物是 `BigWorld/programming/bigworld/scripts/generate-fantasydemo-assets.py`（重写 `programming/fantasydemo/res/` 下的 `server/bw.xml`、`scripts/{entities.xml,db.xml}`、`scripts/entity_defs/{alias.xml,*.def}`、`scripts/{base,cell,client}/*.py`，已提交进仓库、可字节级复现）与 `cmake/samples/{cstdmf,entitydef,network,python,resmgr}/CMakeLists.txt`（构建样例非工程脚手架）。**范围：暂缓**——工程脚手架依赖 `python_assets` 模板树与 `kbengine_defaults.xml` 配置体系，二者分别随 BW-03（python 模块布局）与 BW-09/BW-13（配置与资源管线）立项；模板树的等价物（原 fantasydemo 资产树）已在 BW-14-01 Demo 脚本和数据覆盖，本轮不单独立项。

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
