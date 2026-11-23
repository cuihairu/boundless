#!/usr/bin/env python3
"""Index the client and tools targets (BW-00-05-03).

The Make inventory carries only `tools/process_defs` and `tools/res_packer`;
the editors, exporters and offline tools are Windows-only products declared in
the CMake target groups (BW-00-01-02). This scan walks the CMake groups (minus
`server`, indexed in BW-00-05-02), lifts each *product* project (a project
whose dir is neither `lib/` nor `third_party/`) and classifies it by kind, then
attaches the index entry: the task + fixture that exercises it, or the concrete
gap source for the Windows-only / not-yet-implemented products.

Usage: client_tools_index.py CMAKE_TARGETS_JSON OUTPUT_JSON
"""
import json
import sys

# Windows-only products: the Linux host has no MSVC tree or binaries, and the
# baseline (BW-00-02-05) records the gap with the windows-latest CI carrying it.
WINDOWS_ENTRY = {
    "task": "BW-00-02-05", "fixture": "bw00-02-05-windows-baseline",
    "status": "gap",
    "note": "Windows 构建面；本 Linux 主机无产物与 MSVC 工具链，由 "
            "windows-latest CI 承载（BW-00-02-05 复测）",
}

# Make-inventory targets that also appear in a CMake group: their index is the
# Make-side mapping, not the Windows baseline.
MAKE_ENTRY = {
    "tools/process_defs": {
        "task": "BW-00-02-01", "fixture": "bw00-02-01-runtime-env",
        "status": "covered",
        "note": "运行时环境与进程定义（Make 侧已映射）"},
    "tools/res_packer": {
        "task": "BW-00-01-01", "fixture": "bw00-01-build-targets",
        "status": "gap",
        "note": "资源打包工具；无对应实现与证据"},
    "examples/client_integration/python/simple": {
        "task": "BW-00-02-04", "fixture": "bw00-02-04-client-login",
        "status": "covered",
        "note": "示例登录驱动与实体脚本（跨 BW-00-05-04 示例索引）"},
}


def kind(dir, name):
    if name == "bwclient" or dir == "client":
        return "客户端"
    if dir.startswith("examples/"):
        return "示例"
    if "exporter" in name or "exporter" in dir:
        return "导出器"
    if dir.startswith("tools/asset_pipeline/converters/"):
        return "资产转换器"
    if dir.startswith("tools/asset_pipeline/"):
        return "资产管线"
    if dir.startswith("tools/") and any(
            k in dir for k in ("worldeditor", "modeleditor",
                               "particle_editor", "editor_shared")):
        return "编辑器"
    if dir.startswith("tools/") and any(
            k in dir for k in ("offline_processor", "batch_compiler",
                               "jit_compiler")):
        return "离线工具"
    if dir == "tools/process_defs":
        return "运行时环境"
    if dir == "tools/res_packer":
        return "资源打包"
    if name in ("assetprocessor", "navgen"):
        return "离线工具"
    return "工具基座"


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 1
    cmake = json.load(open(sys.argv[1], encoding="utf-8"))
    out = sys.argv[2]

    rows = []
    no_product = []
    for group, g in sorted(cmake.items()):
        if group == "server":
            continue  # indexed in BW-00-05-02
        products = [p for p in g.get("projects") or []
                    if not p[2].startswith("lib/")
                    and not p[2].startswith("third_party/")]
        if not products:
            no_product.append((group, g.get("projects") or []))
            continue
        for _, name, dir, _ in products:
            entry = MAKE_ENTRY.get(dir, WINDOWS_ENTRY)
            rows.append({
                "target": name, "dir": dir, "group": group,
                "kind": kind(dir, name),
                "task": entry["task"], "fixture": entry["fixture"],
                "status": entry["status"], "note": entry["note"],
            })

    for group, projects in no_product:
        if group.endswith("_blob"):
            note = "打包 blob 变体（BW_BLOB_CONFIG=ON 后 INCLUDE 基座配置，" \
                   "本身不登记项目），随基座组索引"
        else:
            note = "组内仅库依赖（%s），无独立产物；本组索引随其承载面"
        base = group[:-len("_blob")] if group.endswith("_blob") else group
        rows.append({
            "target": group, "dir": "build/cmake/BWConfiguration_%s.cmake"
            % base, "group": group, "kind": "组（无独立产物）",
            "task": "BW-00-02-05", "fixture": "bw00-02-05-windows-baseline",
            "status": "gap",
            "note": (note % ("、".join(p[1] for p in projects[:6]))
                     if not group.endswith("_blob") else note),
        })

    notes = [r for r in rows if r["kind"] == "组（无独立产物）"]
    counts = {"products": len(rows) - len(notes),
              "group_notes": len(notes),
              "covered": sum(1 for r in rows if r["status"] == "covered"),
              "gap": sum(1 for r in rows if r["status"] == "gap"),
              "kinds": {}}
    for r in rows:
        counts["kinds"][r["kind"]] = counts["kinds"].get(r["kind"], 0) + 1

    data = {"counts": counts, "rows": rows}
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    print("rows %d  products %d  group-notes %d" % (
        len(rows), counts["products"], counts["group_notes"]))
    print("covered %d  gap %d" % (counts["covered"], counts["gap"]))
    print("kinds: " + ", ".join("%s=%d" % kv for kv in
                                sorted(counts["kinds"].items())))
    for r in rows:
        print("  %-14s %-34s %-6s %-8s %s" % (
            r["kind"], r["dir"], r["group"], r["status"], r["target"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
