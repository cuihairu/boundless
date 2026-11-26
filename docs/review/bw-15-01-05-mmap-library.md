# BW-15-01-05 封装内存映射与共享库查找

Acceptance: **三端加载相同测试资源**。行保持 `- [ ]`（验收是三端；本席交 Linux 腿端到端实测——同 BW-15-01-01/03 先例）。

## 交付面

- **原语（本 lane 既有）**：`crates/platform/src/mmap.rs`——`Mmap::map_file`（unix mmap / windows `MapViewOfFile`）/`Mmap::anonymous`（unix MAP_ANONYMOUS / windows `VirtualAlloc`）/`DynamicLibrary::open`+`symbol`（dlopen/dlsym，windows 臂 `519a46a`）。
- **端到端**：`crates/platform/tests/mmap_library_baseline.rs` 四面：
  1. **固定资源映射**：64 KiB 索引模 251 图样文件 → `map_file` → 全字节逐一比对（同图样任何腿读到不同字节即点名失败）；
  2. **匿名映射**：长度正确 + 首读全零（类型只暴露只读 `Deref`，可观测契约是零读，非写穿）；
  3. **共享库查找**：`DynamicLibrary::open` 按 platform 各自名（linux `libc.so.6`/macOS `libSystem.B.dylib`/windows `kernel32.dll`）+ `symbol("malloc")` 非空 + 不存在符号名报错（双向数据面）;
  4. **查找确定性**：同库两次 open 同符号同地址（动态加载器按 soname 去重）——插件图跨载稳定。
- 墙钟只记录不断言；`PLATFORM_MMAP_OUT=<path>` 落 TSV。

## Linux x86_64 实测（2026-11-26 本机）

| 面 | 读数 | 判读 |
|---|---:|---|
| map_file | 34 µs | 64 KiB 页缓存热映射 |
| 匿名映射 | 4096 B 首读全零 | 长度+零读契约 |
| dlopen | 22 µs | libc 已驻留，引用计数路径 |
| dlsym | 3 µs | `malloc` 解析 |
| 重开同址 | 相等 | soname 去重 |

## 三端状态

- **Linux x86_64**：上表，实测绿。
- **macOS / Windows**：windows 臂已有（`519a46a`，MapViewOfFile/VirtualAlloc/LoadLibrary 形状），macOS unix 臂同代码；两腿无 runner——待 BW-15-05 矩阵点亮跑同测试。

## 复跑

```sh
PLATFORM_MMAP_OUT="$PWD/docs/review/fixtures/bw-15-01-05/mmap-library-linux.tsv" \
    cargo test -p platform --test mmap_library_baseline -- --nocapture
```

## 门禁

- `cargo test -p platform --test mmap_library_baseline` 1 passed / 0 failed。
- `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` 全绿后推（见交付 commit）。

## 坑

- `Mmap` 只实现只读 `Deref` 无 `DerefMut`——「匿名页可写」不是类型可观测契约（底层页 RW 但测试写不进去），面收缩为长度+零读，与模块内联测试同形（模块测试注释明说：读是可观测契约）。
- 共享库正向面符号选各 C runtime 都导出的 `malloc`（kernel32 用 `GetCurrentProcessId`），负向面用绝不出现在任何库的长名——两个方向都是数据断言。
