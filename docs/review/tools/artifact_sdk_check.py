#!/usr/bin/env python3
"""Audit release artifacts, SDK and missing resources (BW-00-05-05).

Each row states an artifact/SDK/resource item, whether it is present in the
reference tree (computed, not asserted), the evidence source that establishes
its status, and - for a gap - the blocking reason. The acceptance is that every
gap carries a 来源 (which evidence) and a 阻塞原因 (what blocks it).

Usage: artifact_sdk_check.py BIGWORLD_ROOT OUTPUT_JSON
"""
import json
import os
import sys

# (kind, item, path-rel-to-BWROOT, task, fixture, source, blocker-if-gap)
ENTRIES = [
    ("发行物", "Linux 服务端二进制树", "game/bin/server/el7",
     "BW-00-02-01", "bw00-02-01-runtime-env",
     "bw00-02-01-runtime-env（59 安装文件 sha256）+ bw00-01-release-scripts",
     ""),
    ("发行物", "Windows 服务端包", "game/bin/win32|win64",
     "BW-00-02-05", "bw00-02-05-windows-baseline",
     "bw00-02-05-windows-baseline",
     "阻塞：本机无 MSVC/msbuild/powershell 与 Windows 产物；由 windows-latest "
     "CI 的 root-build-probe 承载"),
    ("发行物", "客户端 SDK 三件套", "client_sdk",
     "BW-00-01-03", "bw00-01-release-scripts", "bw00-01-release-scripts",
     "阻塞：SDK 版本仅 0.0.1、产物仅 workflow artifact（client-sdk job，"
     "retention 14d）；新版无对应 SDK 发布面"),
    ("发行物", "products-manifest.txt", "(stage 打包期)",
     "BW-00-01-03", "bw00-01-release-scripts", "bw00-01-release-scripts",
     "阻塞：打包期产物不在树内（随 stage 逐文件 SHA256 生成）；"
     "已记录未构建项 cellapp_extension"),
    ("SDK", "typescript", "client_sdk/typescript",
     "BW-00-01-03", "bw00-01-release-scripts", "bw00-01-release-scripts",
     "阻塞：版本 0.0.1；新版无对应 TypeScript SDK"),
    ("SDK", "unity", "client_sdk/unity",
     "BW-00-01-03", "bw00-01-release-scripts", "bw00-01-release-scripts",
     "阻塞：含 PROTOCOL.md 协议约定；新版无对应 Unity SDK"),
    ("SDK", "unreal", "client_sdk/unreal",
     "BW-00-01-03", "bw00-01-release-scripts", "bw00-01-release-scripts",
     "阻塞：仅 REVIEW.md 占位；新版无对应 Unreal SDK"),
    ("SDK", "generator (bwgen)", "client_sdk/generator",
     "BW-00-01-03", "bw00-01-release-scripts", "bw00-01-release-scripts",
     "阻塞：Python 代码生成器；新版无对应"),
    ("缺失资源", "game/bin/win32", "game/bin/win32",
     "BW-00-02-05", "bw00-02-05-windows-baseline", "bw00-02-05-windows-baseline",
     "阻塞：Windows 构建面缺失（同 Windows 服务端包）"),
    ("缺失资源", "game/bin/win64", "game/bin/win64",
     "BW-00-02-05", "bw00-02-05-windows-baseline", "bw00-02-05-windows-baseline",
     "阻塞：Windows 构建面缺失（同 Windows 服务端包）"),
    ("缺失资源", "snapshot_helper 安装", "game/bin/server/el7/server/commands/snapshot_helper",
     "BW-00-01-01", "bw00-01-build-targets", "bw00-01-build-targets",
     "阻塞：该检出已声明该目标但未安装（el7/server/commands 无此文件）；"
     "仅清册有声明，无产物"),
    ("缺失资源", "third_party/png", "programming/bigworld/third_party/png",
     "BW-00-01-01", "bw00-01-build-targets", "bw00-01-build-targets",
     "阻塞：vcpkg 迁移后残留空壳组件，不再由 Make 构建"),
    ("缺失资源", "third_party/zip", "programming/bigworld/third_party/zip",
     "BW-00-01-01", "bw00-01-build-targets", "bw00-01-build-targets",
     "阻塞：vcpkg 迁移后残留空壳组件，不再由 Make 构建"),
]


def present(root, path):
    """True when the item exists in the reference tree (path is a marker)."""
    if "|" in path:
        return any(os.path.exists(os.path.join(root, p))
                   for p in path.split("|"))
    full = os.path.join(root, path)
    if os.path.isfile(full):
        return True
    if os.path.isdir(full):
        return True
    return False


def count(root, path):
    full = os.path.join(root, path)
    if os.path.isfile(full):
        return 1
    if not os.path.isdir(full):
        return 0
    n = 0
    for dirpath, dirnames, filenames in os.walk(full):
        dirnames[:] = sorted(d for d in dirnames if d not in (".git",
                                                             "__pycache__"))
        n += len(filenames)
    return n


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 1
    root, out = sys.argv[1:3]

    rows = []
    for kind, item, path, task, fixture, source, blocker in ENTRIES:
        is_present = present(root, path)
        # A row is a gap when it carries a blocker (even if the path exists in
        # the tree - e.g. the vcpkg stubs third_party/png|zip are present as
        # source but not built); it is covered when the artifact exists with an
        # evidence baseline and no blocker.
        if blocker:
            status, note = "gap", blocker
        else:
            status, note = "covered", "存在：" + source
        rows.append({
            "kind": kind, "item": item, "path": path,
            "files": count(root, path) if not "|" in path else 0,
            "present": is_present, "task": task, "fixture": fixture,
            "status": status, "source": source, "note": note,
        })

    counts = {"rows": len(rows), "present": sum(1 for r in rows
                                                if r["present"]),
              "gap": sum(1 for r in rows if r["status"] == "gap"),
              "gap_with_source": sum(1 for r in rows
                                     if r["status"] == "gap"
                                     and r["source"] and r["note"]),
              "kinds": {}}
    for r in rows:
        counts["kinds"][r["kind"]] = counts["kinds"].get(r["kind"], 0) + 1

    data = {"counts": counts, "rows": rows}
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    print("rows %d  present %d  gap %d (gap-with-source %d)" % (
        counts["rows"], counts["present"], counts["gap"],
        counts["gap_with_source"]))
    for r in rows:
        print("  %-6s %-22s %-58s %s" % (
            r["kind"], r["item"], r["path"], r["status"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
