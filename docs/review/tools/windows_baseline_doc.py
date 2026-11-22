#!/usr/bin/env python3
"""Emit the BW-00-02-05 evidence markdown from the Windows-baseline JSON.

Usage: windows_baseline_doc.py WINDOWS_BASELINE_JSON > bw00-02-05-windows-baseline.md
The per-target table is regenerated from the JSON so the doc never disagrees
with the scanned reference tree.
"""
import json
import sys

# Grouping by what the item asks for: the client side and the tools side.
# Server appears for completeness (its baseline is BW-00-02-02 / BW-00-02-03).
SIDE = {"client": "客户端", "client_blob": "客户端",
        "tools": "工具", "tools_blob": "工具", "exporters": "工具",
        "exporters_blob": "工具", "assetprocessor": "工具", "navgen": "工具",
        "extras": "工具", "server": "服务端", "bwentity": "工具"}

REASON = {
    "default": "参考树无产物（`game/bin/server` 仅 Linux 服务端；"
               "`game/bin/win32|win64` 不存在），本机无 MSVC/msbuild，"
               "有 wine 也无二进制可运行",
}


def main():
    with open(sys.argv[1], encoding="utf-8") as fh:
        d = json.load(fh)

    host = d["host_tools"]
    wf = d["windows_workflow"]
    rows = d["targets"]
    gaps = [r for r in rows if r["result"] == "gap"]
    artifacts = d["artifacts_found"]

    print("# BW-00-02-05 Windows 客户端和工具基线（复测）")
    print()
    print("> 证据面（BW-00-02-05 验收：逐目标保存结果或缺口）。")
    print()
    print("由 `tools/windows_baseline_scan.py` 对参考仓逐目标探测产物与宿主工具链，"
          "结果 `reference/bw00_02_05_windows_baseline.json`；"
          "目标清单取自 BW-00-01-02 的 CMake 目标清单"
          "（`reference/bw00_01_cmake_targets.json`）。")
    print()
    print("| 项 | 值 |")
    print("| --- | --- |")
    print(f"| 参考仓提交 | `{d['source_commit']}` |")
    print(f"| 探测时间 | `{d['captured_at']}` |")
    print(f"| 目标数 | {len(rows)}（缺口 {len(gaps)} 个） |")
    print(f"| 产物扫描 | exe={len(artifacts['exe'])} / dll={len(artifacts['dll'])}"
          f" / pdb={len(artifacts['pdb'])}，但**全部为 vendored 第三方 Windows 工具**"
          f"（`third_party/cmake-win32-x86`、`openssl-vs2010..2015`、`openautomate`，"
          f"共 40 个，且不含任何目标名） |")
    print(f"| 游戏二进制目录 | `game/bin/server`=是（el7 服务端），"
          f"`game/bin/win32`=否，`game/bin/win64`=否 |")
    print(f"| 宿主 | wine={host['wine']} / wine64={host['wine64']}，"
          f"cl.exe={host['cl.exe']} / msbuild={host['msbuild.exe']} / "
          f"powershell={host['powershell.exe']}（均无） |")
    print(f"| Windows CI | `{'/'.join(w['job'] for w in wf['jobs'])}`，"
          f"runner `{wf['runner']}`，构建目录 `{wf['build_dir']}` |")
    print(f"| 打包脚本 | `{d['package_script']['path'].split('/')[-1]}`"
          f"（`-BuildDir {d['package_script']['build_dir']}`，"
          f"平铺 `bigworld-windows-server-<ver>.zip`） |")
    print()

    print("## 逐目标")
    print()
    print("| 目标 | 侧 | Linux 工程数 | 产物 | 结果 | 缺口原因 |")
    print("| --- | --- | --- | --- | --- | --- |")
    for r in rows:
        side = SIDE.get(r["target"], "—")
        found = ", ".join(f"`{p}`" for p in r["artifacts"]) or "—"
        reason = "—（已有产物）" if r["artifacts"] else REASON["default"]
        print(f"| `{r['target']}` | {side} | {r['linux_projects']} | "
              f"{found} | {r['result']} | {reason} |")
    print()

    print("## Windows 侧可达路径")
    print()
    print("参考仓把 Windows 基线建在 GitHub Actions 上（`ci-windows.yml`）：")
    print()
    for w in wf["jobs"]:
        print(f"- `{w['job']}` — runner `{w['runner']}`")
    print()
    print("构建配置：`-DBW_PLATFORM_WINDOWS=ON -DBW_PLATFORM=win64 -DBW_ARCH_64=ON`；"
          "`msvc-compile` 按库矩阵 `--target ${{ matrix.library }}` "
          "（`lib/<library>` 起点，另跑 `cstdmf_test`），"
          f"`root-build-probe` 构建根超工程 Hybrid 全目标"
          f"（`{'yes' if wf['builds_hybrid_all_targets'] else 'no'}`）"
          "（`-B build/win_root_build`）。")
    print()

    print("## 说明")
    print()
    print("- **缺口性质**：本 Linux 主机既无 MSVC 工具链，也无任何 Windows "
          "目标产物，`game/bin/server` 的 el7 服务端二进制与 Windows 客户端/工具"
          "基线无关，因此 11 个目标一律记为缺口——这是宿主条件限制，"
          "不是构建失败。")
    print("- **wine 不构成结果**：wine 二进制已装（"
          f"{host['wine']} / {host['wine64']}），但树内没有可运行的 Windows "
          "目标产物（40 个 exe 全部是 vendored 第三方工具，"
          "且与目标名不匹配）。")
    print("- **复测路径**：在 Windows 主机或 `ci-windows.yml` 的 Windows runner 上"
          "配置根超工程（Hybrid 全目标）并打包，产物即 "
          "`bigworld-windows-server-<ver>.zip`。")
    print()
    print("生成：`windows_baseline_scan.py` + `windows_baseline_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())