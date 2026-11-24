# BW-10-02-01 Compiled Space 二进制容器解析 — 复核证据

- **任务编号**: BW-10-02-01（lane `lane/bw10-terrain`，todo.md 行 1338）
- **参照路径勘误**: 巡检单写的 `/home/cui/workspaces/kbengine/BigWorld-Engine-14.4.1`
  在本机不存在（目录为空）；本任务全部参照取自既有权威树
  `/home/cui/workspaces/BigWorld/programming/bigworld/lib/`。
- **原版构建目标/源码**:
  - 容器: `lib/compiled_space/binary_format_types.hpp`（Header/SectionHeader/MAX_SECTIONS）、
    `lib/compiled_space/binary_format.cpp`（`BinaryFormat`、`Stream` 读协议、
    `findSection`/`findAndOpenSection`）、`binary_format.hpp:107-182`（`read(T&)`/
    `read(ExternalArray<T>&)` 前缀协议）。
  - 字节表: `string_table_types.hpp`、`string_table.cpp`（`StringTable::readFromStream`
    验证链）。
  - 资产清单: `asset_list_types.hpp`、`asset_list.cpp:80-161`（读端；`preloadAssets`
    为客户端渲染面，未移植）。
  - 设置: `compiled_space_settings_types.hpp`、`compiled_space_settings.cpp`。
  - 写端（用于产样）: `binary_writers/{binary_format_writer,string_table_writer,
    asset_list_writer,settings_writer,space_writer}.cpp`。
- **Boundless 模块**: `crates/compiled-space`（`binary_format` / `string_table` /
  `asset_list` / `settings`）+ `crates/compiled-space/tests/space_bin.rs`。
- **平台**: Linux x86_64，Rust 工具链 1.100.0，llvm-cov 0.9.1，g++（系统 /usr/bin/c++）。

## 夹具来源登记（BW-10-02-01）

**`docs/review/fixtures/compiled_space/space.bin`（340 字节）是原版写端代码的原生产物**，
非手造字节：

1. 原版 `tools/asset_pipeline` space_converter 无法在 Linux 运行（其 CMake 根在非
   Windows 平台 FATAL_ERROR，BW-00-01-02 已证），故改为直编原版 writer 源：
   `g++ -std=c++11 -DNDEBUG -DCODE_INLINE -w -c lib/compiled_space/binary_writers/{binary_format_writer,string_table_writer,asset_list_writer,settings_writer,space_writer}.cpp -I lib`，
   链接参照树 Linux 既有构建库 `build/cmake-resmgr/{libresmgr,libmath,libcstdmf}.a` +
   `-lz -lpthread`。
   （`-DCODE_INLINE` 把 `vector3.ipp`/`boundbox.ipp` 定义内联进本翻译单元，
   与 cmake-resmgr 库内符号形态一致；链接顺序须 resmgr→math→cstdmf。）
2. `gen_space.cpp`（与样本同目录存档）以原版 `StringTableWriter`/`AssetListWriter`/
   `SettingsWriter`/`BinaryFormatWriter` 写出：3 个资产（model/texture/effect，其中
   model 路径重复一次以触发写端去重）+ 1 个清单外字符串（chunk 路径）+ AABB
   (-50,-10,-50)~(50,30,50)。段序 BWST→BWAL→BWCS 与 space_converter 的
   SpaceWriter 接线一致。运行时 `BinaryFormatWriter::write` 走 BWResource 单例落盘，
   故 harness 内有 `BWResource` 栈实例 + `BWResource::init`。
3. 样本与生成器均在 `md5sum 05d7aa28065d76dc19097b50c49743c8d` 之文件逐字节一致后归档。

## 原版结果（源码核对）

- 容器 `BWTB` v1：Header 24B（magic/version/headerSize/fileSize/numSections），
  段表紧跟字节 24（`(SectionHeader*)(pHeader_+1)`，**reader 从不读 headerSize_**）；
  SectionHeader 24B（magic/version/offset/length），段偏移绝对、载荷紧邻。
- **writer 从不写 fileSize_**（memset 头后仅填四字段），真样本该字段为 0，reader
  也从不校验。
- 读协议（binary_format.hpp:107-182）：`read(uint32&)`=裸 4 字节；`read(T&)`=u32
  typeSize 前缀（须==sizeof(T)）+载荷；`read(ExternalArray<T>&)`=前缀+u32 count+
  count×sizeof(T) 裸数据；`seek` 钳到长度；EOF 处 `readRaw(0)` 成功。
- BWST：ExternalArray<Entry{offset,length}>（typeSize==8）+ 裸 u32 stringDataSize +
  裸字符串数据；验证链=逐条 offset+length≤size（uint32 回绕加法）、尾 NUL、
  Σ(length+1)==stringDataSize。
- BWAL：ExternalArray<Entry>（typeSize==8）。**Entry 无 pragma pack**，{u8 type,
  3 填充字节, u32 stringTableIndex}=8B，写端不初始化填充字节——真样本里是栈垃圾
  （df ac 91 / de ac 91），读端不校验不清零。
- BWCS：`read(Header)`=typeSize==24 前缀 + AABB（min/max 各 3×LE f32）。

## 原版怪癖登记（逐面移植时保留并在代码处引用）

1. `findAndOpenSection` 版本不符消息参数颠倒（binary_format.cpp:343-349）：实参序为
   `foundVersion, requiredVersion`，打印成 “expected <found>, got <required>”。移植按原样复现。
2. `AssetList::read` 在条目数组读坏时 `close()` 后**仍返回 true**（asset_list.cpp:97-102），
   isValid()=false 才是失败信号。移植以 `Ok`+cleared+`valid=false` 复现。
3. `StringTable::readFromStream` 对 stringDataSize 的 u32 读取**不检查返回值**
   （string_table.cpp:75-77），EOF 时按 0 继续。
4. NUL 缺失与 Σ 不符两条失败**无消息**（静默 false）。
5. 原 `openSection` 对越界段返回带 NULL 缓冲的流（先 UB）；移植返回 `None`，
  使 `findAndOpenSection` 的 “failed to map … into memory” 分支可达（原版该分支为死码）。
   —— 已在 binary_format.rs 的 open_section 文档注释登记为有意分歧。
6. `StringTable::read`/`CompiledSpaceSettings::read` 失败时 findAndOpenSection 与自身
   各打一条消息；移植把两行并进一个 `Err` 字符串（测试按两行完整断言）。

## 新版结果

- `cargo test -p compiled-space`：**lib 49 + 集成 4 全绿**；`cargo fmt -p compiled-space
  --check` 0 漂移；`cargo clippy -p compiled-space --locked --all-targets -- -D warnings`
  0 findings。
- llvm-cov 0.9.1（`--lcov` + awk LF/LH）：**行覆盖 918/918 = 100.00%**。
- 集成测试以真样本逐字段核对：头（BWTB/v1/headerSize 96/**fileSize 0**/3 段）、
  段表（BWST@96/184、BWAL@280/32、BWCS@312/28，载荷邻接 96+184=280=…=340）、
  字节表 4 串（33/36/33/34 字节、偏移 0/34/71/105）、资产清单 3 条（重复 model 被
  写端去重、padding 垃圾被容忍）、设置 AABB 精确 f32。
- 单元测试覆盖所有错误路径：坏 magic/版本、截断头/段表、三条 findAndOpenSection
  消息（含参数颠倒）、BWST 各验证分支（含静默分支与 stringDataSize EOF 按 0）、
  BWAL 坏数组仍 Ok 的怪癖、BWCS 坏前缀/截断。

## 证据链接

- `crates/compiled-space/src/{binary_format,string_table,asset_list,settings}.rs`、
  `crates/compiled-space/tests/space_bin.rs`。
- 夹具：`docs/review/fixtures/compiled_space/{space.bin,gen_space.cpp}`。
- 原版活基准：`programming/bigworld/build/cmake-resmgr/`（Linux 构建的
  libcstdmf/libmath/libresmgr，供产样链接）。

- **状态**: 待 push 复核。
