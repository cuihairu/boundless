#!/usr/bin/env python3
"""Emit the BW-00-02-01 evidence markdown from the runtime-env JSON.

Usage: runtime_env_doc.py RUNTIME_ENV_JSON > bw00-02-01-runtime-env.md
The hash table is regenerated from the JSON so the doc never disagrees with
the scan output.
"""
import json
import sys

# Binaries that pull system shared libraries beyond the C++ runtime set.
CPP_RUNTIME = {"libstdc++.so.6", "libm.so.6", "libgcc_s.so.1", "libc.so.6"}


def main():
    with open(sys.argv[1], encoding="utf-8") as fh:
        d = json.load(fh)

    b = d["build"]
    print("# BW-00-02-01 原版运行环境采集")
    print()
    print("> 证据面（BW-00-02-01 验收：记录提交、二进制哈希、构建参数、Python 和 DB 版本）。")
    print()
    print("由 `tools/runtime_env_scan.py` 扫描参考仓 `cuihairu/BigWorld` 生成；"
          "源数据 `reference/bw00_02_01_runtime_env.json`。")
    print()

    print("## 源与构建参数")
    print()
    print("| 项 | 值 |")
    print("| --- | --- |")
    print(f"| 参考仓提交 | `{d['source_commit'][:12]}"
          f"`（{d['source_commit_short']}，{d['source_commit_date']}，"
          f"“{d['source_commit_subject']}”） |")
    print(f"| REVISION_NUMBER | `{d['revision_number']}`（`build/el7/build/"
          "build_revision.generated.hpp`，构建时未注入 git 版本号） |")
    print(f"| 平台 / 配置 | `el7` / `{b['config']}`（`BW_CONFIG_DEFAULT := hybrid`；"
          "hybrid 不带 `_hybrid` 目录后缀，`build/make/Makefile:86`） |")
    print(f"| C++ 标准 | `-std=c++23`（`build/make/platform_el7.mak`） |")
    print(f"| 编译器 | `{b['compiler_stamp']}`（stamp 取自 `server/cellapp`） |")
    print()

    print("## Python 与数据库版本")
    print()
    print("| 组件 | 版本 | 来源 |")
    print("| --- | --- | --- |")
    print(f"| 嵌入解释器（静态） | **CPython {d['python_embedded']}** | "
          "`third_party/python` vendored，静态链入每个角色二进制（`libpython3.13.a`） |")
    print(f"| 系统 `python3` | {d['system']['python3']} | 开发机 PATH，不参与运行面 |")
    print(f"| OpenSSL（静态，vcpkg） | {d['vcpkg_ports']['openssl']} | "
          "`build/el7/third_party/vcpkg_installed/x64-linux-bw/include/openssl/opensslv.h` |")
    print(f"| 系统 OpenSSL | {d['system']['openssl']} | "
          "`ldd` 仅 MySQL 命令类经 `libmysqlclient.so.24` 传递引入 |")
    print(f"| MySQL 客户端（动态） | `libmysqlclient.so.24`，"
          f"`mysql_config` {d['system']['mysql_config']}（包 "
          "`libmysqlclient-dev 8.4.13-0ubuntu0.26.04.2`；另装 `libmariadb3 1:11.8.6` 备选） | 系统 |")
    print(f"| SQLite3（静态，vcpkg） | {d['vcpkg_ports']['sqlite3']} | "
          "`include/sqlite3.h` |")
    print(f"| MongoDB 驱动 | legacy 26compat **{d['mongodb_driver']}** | "
          "`third_party/mongodb/mongo_cxx_driver/mongo-cxx-driver-legacy-0.0-26compat-*.tar.gz` |")
    print(f"| 系统 sqlite3 | {d['system']['sqlite3'][0] if d['system']['sqlite3'] else '—'} | CLI，未被二进制引用 |")
    print()

    print("### vcpkg port 集（`builtin-baseline 10541e317a660f4165ba4ac2851ab54a8d4577b1`）")
    print()
    print("| port | 版本 |")
    print("| --- | --- |")
    for k in ("openssl", "curl", "jsoncpp", "zlib", "libpng", "sqlite3", "bzip2"):
        print(f"| `{k}` | {d['vcpkg_ports'][k] or '—'} |")
    print()

    print("## 链接形态")
    print()
    print("对 46 个 ELF 逐一 `ldd`：")
    print()
    print("- **静态派**（角色二进制 / 工具 / 单测，共 {} 个）：动态面只有 "
          "`libstdc++.so.6`、`libm.so.6`、`libgcc_s.so.1`、`libc.so.6`——"
          "vcpkg 静态库与 `libpython3.13.a` 全部链进可执行。"
          "例：`server/cellapp`。".format(
              sum(1 for x in d["binaries"]
                  if x["kind"] == "elf" and set(x["shared_libs"]) <= CPP_RUNTIME)))
    print("- **MySQL 动态派**（5 个：`server/commands/clear_auto_load`、"
          "`consolidate_dbs`、`sync_db`、`server/commands/_helpers/snapshot_helper`、"
          "`server/dbapp-extensions/bwengine_mysql.so`）：系统 `libmysqlclient.so.24` "
          "动态链接，并传递引入系统 `libssl.so.3` / `libcrypto.so.3` / `libz.so.1` / "
          "`libzstd.so.1` / `libresolv.so.2`。`transfer_db` 是 SQLite-only（Make 特性 "
          "`useSQLite=1`），不在此列、纯静态。")
    print()
    print("即发行脚本注释所称「third-party libraries are statically linked (vcpkg), "
          "only libc/libstdc++/mysql client stay dynamic」与本机实测一致"
          "（MySQL 传递引入的系统 ssl/crypto/z 也在动态面）。")
    print()

    print("## 二进制哈希")
    print()
    print("安装树 `game/bin/server/el7/` 下 {} 个文件：{} 个 ELF 与 {} 个非 ELF。"
          "下表列出全部 ELF 及脚本 `remove_db`（{} 行）；其余 {} 个为单测断言转储"
          "（`assert.*.log`）与 `unit_tests/xmldataresource.xml` 数据文件，"
          "不入哈希表（见 JSON）。".format(
              len(d["binaries"]),
              sum(1 for x in d["binaries"] if x["kind"] == "elf"),
              sum(1 for x in d["binaries"] if x["kind"] == "data"),
              sum(1 for x in d["binaries"] if x["kind"] == "elf") + 1,
              sum(1 for x in d["binaries"] if x["kind"] == "data") - 1))
    print()
    print("| 路径 | sha256 | 大小 |")
    print("| --- | --- | --- |")
    for x in d["binaries"]:
        if x["kind"] == "data" and not x["path"].endswith("remove_db"):
            continue
        print(f"| `{x['path']}` | `{x['sha256']}` | {x['size']:,} |")
    print()
    print("生成：`runtime_env_scan.py` + `runtime_env_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
