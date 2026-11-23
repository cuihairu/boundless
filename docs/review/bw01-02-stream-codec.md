# BW-01-02-01 定长整数和浮点读写 — 复核证据

- **任务编号**: BW-01-02-01（lane BW-01-02 流与基础编解码，todo.md 行 163）
- **原版构建目标/源码**: `programming/bigworld/lib/cstdmf/binary_stream.{hpp,ipp,cpp}`、`memory_stream.{hpp,ipp,cpp}`。夹具由原版源码在本机直接编译运行产出（allocator 以 malloc/free 桩替代，绕开 MemoryDebug→profiler 链接墙；`target/bwharness/{stubs.cpp,harness.cpp}`，产物不入库，夹具字节已钉进测试）。
- **Boundless 模块**: `crates/foundation/src/stream.rs`（`BinaryStream` 定长读写 + `#[cfg(test)] mod original_fixtures`）。
- **平台**: Linux x86_64（gcc 15 编译原版夹具；Rust 工具链 1.100.0）。
- **输入夹具**: 原版运行时逐值落盘（每值独立一条流）：u8/i8/u16/i16/u32/i32/u64/i64/f32/f64/bool，含负数与边界（`0x7f`、`-128`、`0xdeadbeef`、`-123456789`、`0x0123456789abcdef`、e、3.14 等）。
- **原版结果**: 全部小端序（LE 主机上 `BW_HTONL` 为恒等，`binary_stream.ipp` 直接 `memcpy`）；bool 为单字节 0/1；f32/f64 按 IEEE 位型小端落盘（`00006040` = 3.5，`6957148b0abf0540` = π·10³ 等）。
- **新版结果**: `fixed_width_matches_original_bytes`（10 组记录逐字节断言）与 `bool_is_one_byte_per_value` 全绿；`cargo test -p foundation --lib` 通过。
- **证据链接**: `crates/foundation/src/stream.rs`（original_fixtures 模块）、commit `ae72697`。
- **状态**: 已验收（原版夹具逐字节往返）。

---

# BW-01-02-02 变长整数与长度前缀 — 复核证据

- **任务编号**: BW-01-02-02（todo.md 行 164）
- **原版构建目标/源码**: `lib/cstdmf/binary_stream.cpp`（`readStringLength`/`writeStringLength`）、`binary_stream.ipp`（`BW_PACK3`/`BW_UNPACK3`）。
- **Boundless 模块**: `crates/foundation/src/stream.rs`（`write_packed_int`/`read_packed_int`）。
- **输入夹具**: 15 个跨边界值：0、1、127、128、253、254、255、256、1023、65534、65535、65536、1000000、16777214、16777215。
- **原版结果**: `< 255` 单字节；`>= 255` 先写 `0xFF` 再写三低字节小端（`BW_PACK3`），上限 `1<<24`；读侧 `0xFF` 触发 `BW_UNPACK3`。夹具实测：`255→ffff0000`、`256→ff000100`、`65535→fffeff00`、`65536→ffffff00`、`16777215→ffffffff`。
- **新版结果**: `packed_int_matches_original_bytes`（15 值表格逐字节）与 `count_is_plain_u32_like_original_vector_writes` 全绿（容器元素个数是裸 u32，非 packed，原版 `writeVectorSize` 同）。
- **证据链接**: commit `ab4add4`。
- **状态**: 已验收（跨长度边界值逐字节吻合）。

---

# BW-01-02-03 字符串与内存流 — 复核证据

- **任务编号**: BW-01-02-03（todo.md 行 165）
- **原版构建目标/源码**: `lib/cstdmf/binary_stream.cpp`（`readString`/`writeString`）、`blob_or_null.cpp`、`memory_stream.{hpp,ipp}`（remaining/rewind 语义）。
- **Boundless 模块**: `crates/foundation/src/stream.rs`（`write_string`/`read_string`、`write_blob`/`write_blob_raw`、`write_blob_or_null`/`read_blob_or_null`、`remaining`/`rewind`）。
- **输入夹具**: 空串（`00`）、`abc`（`03616263`）、嵌零 `a\0b`（`03610062`）、255 字节串（`ffff0000` 前缀 + 255 字节）、裸 blob（`addBlob` 无前缀 `616263`）、BlobOrNull 三态。
- **原版结果**: 字符串 = packed 长度前缀 + 原始字节，无终结符，嵌零原样保留；`addBlob` 是裸 `memcpy` 无任何前缀；`BlobOrNull`：非空走普通 blob，null/空 写 packed 0 + 一字节标志（null=0，empty=1，实测 `0000`/`0001`）；读失败时 remaining 钳到 0。
- **新版结果**: `string_fixtures_match_original_bytes`、`add_blob_writes_raw_bytes_without_prefix`、`blob_or_null_fixtures_match_original_framing`、`remaining_length_after_partial_read_matches_original`（写三个 u32 读一个，remaining=8）全绿。
- **证据链接**: commit `8d1f90e`。
- **状态**: 已验收（空值、嵌入零和剩余长度吻合）。

---

# BW-01-02-04 压缩流 — 复核证据

- **任务编号**: BW-01-02-04（todo.md 行 166）
- **原版构建目标/源码**: `programming/bigworld/lib/network/zip_stream.{hpp,cpp}`（`ZipOStream`/`ZipIStream`，zlib `compress2`/`uncompress`，本机编译链接 `-lz`）。
- **Boundless 模块**: `crates/foundation/src/zip_stream.rs`（`write_compressed`/`read_compressed`/`read_compressed_into`/`peek_original_length`，flate2 rust_backend，等级 1）。
- **输入夹具**: 原版 100 字节与 5000 字节可压 pattern 容器、以及伪造超读容器（两条 string 编码长度后无负载）。
- **原版结果**: 容器 = string 编码原始长度 + string 编码压缩长度 + 裸 zlib 流（默认等级 1 = Z_BEST_SPEED）；`ZipIStream::init` 拒绝压缩长度越过流尾、在分配前拒绝 `origLen > compressedLen * 1032`（DEFLATE 最坏膨胀比，CR-P2-21）；支持调用方缓冲（过小拒绝）；Adler-32 由 `uncompress` 校验。
- **新版结果**:
  - Rust 解码原版 n100/n5000 容器还原出与原版相同的 pattern（`decodes_the_original_container_n100/_n5000`）。
  - n100 载荷下 flate2(等级 1) 与 zlib(等级 1) 输出**逐字节相同**，Rust 写出的容器与原版容器逐字节相等（`matches_the_original_container_byte_for_byte`）。
  - 反向：原版 `ZipIStream` 解码 Rust 写出的 n5000 容器（`target/bwharness/decoder.cpp`），输出 `declared_orig=5000,comp=422,recovered=5000,match=1` —— 双方可解码同一夹具。
  - 防御项全锚定：压缩长度越流尾 → `Truncated`；声明原始长度超 1032:1 → `CompressionRatioExceeded`（分配前）；调用方缓冲过小拒绝；`peek_original_length` 不消费。
- **已知差异**: n5000 载荷 flate2 输出 422 字节 vs zlib 409 字节（miniz_oxide 与 zlib 的匹配器启发不同），双方均可解；Rust `peek`（单字节窥视）在流尾返回 `None` 不置错误，原版 `MemoryIStream::peek` 会置 `error_` —— 差异登记，不影响读越界报告语义（读路径仍报错）。
- **证据链接**: commit `217e35a`；夹具十六进制钉入 `crates/foundation/src/zip_stream.rs` 测试常量。
- **状态**: 已验收（双方可解码同一夹具）。

---

# BW-01-02-05 截断和越界报告 — 复核证据

- **任务编号**: BW-01-02-05（todo.md 行 167）
- **原版构建目标/源码**: `lib/cstdmf/memory_stream.ipp`（`MemoryIStream::retrieve` 的越界块：`pCurr_ = pEnd_; error_ = true; return nBytes <= 2048 ? errBuf : NULL` —— `IF_NOT_MF_ASSERT_DEV` 失败块总是执行，不 abort）。
- **Boundless 模块**: `crates/foundation/src/stream.rs`（`take`/`take_n` 的钳位 + `mark_error`；original_fixtures 六条记录）。
- **输入夹具**: 六条原版运行记录：2 字节读 u32、空流读 u8、声明 5 字节只有 2 字节的字符串、`ff 00` 的 packed 载荷截断、错误后继续读（sticky）、2 字节流 retrieve 4 字节。
- **原版结果**: 全部 `error=1, remaining=0`（游标钳到末尾）；≤2048 字节的失败读返回内部 `errBuf`，故固定宽读把目标清零（夹具打印 `value=0x00000000`，初始值 `0xAAAAAAAA` 被覆盖）而不中止；错误后游标已钳到末尾，后续读同样失败（sticky）。
- **新版结果**: 六个锚定测试全绿，全部映射为同类错误 `StreamError::Truncated`：`short_fixed_width_read_reports_error`、`read_from_an_empty_stream_reports_error`、`string_overrun_reports_error`、`packed_int_payload_truncation_reports_error`、`error_is_sticky_across_reads`、`retrieve_beyond_end_reports_error`；每条断言 `has_error()` 且 `remaining()==0`。原版「目标清零」语义在 Rust 侧对应 `Err`（无出参），登记于测试注释。
- **证据链接**: commit `c394a0c`。
- **状态**: 已验收（异常输入产生同类流错误）。

---

## 门禁汇总（lane BW-01-02 收口时点）

- `cargo test -p foundation --lib`：159 passed / 0 failed。
- `cargo clippy -p foundation --all-targets -- -D warnings`：0 findings。
- `rustfmt --check --edition 2024`：`stream.rs`、`zip_stream.rs` 通过（仅本批文件）。
- 提交序：`ae72697`（01）→ `ab4add4`（02）→ `8d1f90e`（03）→ `217e35a`（04）→ `c394a0c`（05）。
