#!/usr/bin/env python3
"""BW-00-05-05: record the reference distribution, SDK and resource gaps.

The coverage ledger and the target/entry mappers index build targets; the
reference also ships artifacts that are not targets of this source tree: a
prebuilt server distribution (game/), a client SDK (client_sdk/, on the
reference's own lane/client-sdk), and runtime resources the servers need that
neither the distribution nor this port carries. This mapper records each with
its source (the reference path, or the loader that needs it) and, for every
gap, the reason it blocks and the ticket that turns it on — the record the
acceptance asks for ("对第三方 SDK、发行物中的独有内容和缺失资源单独记录来源
与验证缺口").

  * distribution — the reference's prebuilt runtime (game/bin/server/el7):
    server processes, tools, examples, unit tests, the bundled Python and the
    runtime scripts tree;
  * sdk — the reference's client SDK parts (unity, unreal, typescript,
    generator) and the port-side gap (this port carries no sdk/ tree yet,
    planned by BW-21-01-01);
  * missing — the runtime resources the port does not carry: the bundled
    scripts tree and the game-project res (server config, entity defs, game
    scripts), each with the loader that needs it and the ticket that turns it
    on.

Usage:
  ci/coverage-artifacts.py REFERENCE_ROOT PORT_ROOT TODO OUT_DIR
      [--sdk-root SDK_ROOT] [--self-check]

Exits 2 when the reference or port root is missing; --self-check exits 3 on a
hollow map (no rows, or a gap row without a source or a blocking reason).
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from coverage_scope import parse_scopes, scope_dir  # noqa: E402

DIST_TICKET = "BW-14"     # 仓库根 scripts/package-daily* 和发行物
SDK_TICKET = "BW-21"      # 多引擎客户端 SDK
SCRIPT_TICKET = "BW-03"   # CPython 嵌入和脚本 API (the bundled scripts tree)
GAME_RES_TICKET = "BW-14"  # FantasyDemo 的游戏项目 res (server config, entity defs)

# The port-side gap rows are curated: the resource, the port-side probe (a path
# that exists only when the resource is present), the loader that needs it, the
# ticket that turns it on, and the reason it blocks today. The port carries
# none of them now — verified by the tree checks below, not assumed.
PORT_RES_GAPS = [
    ("res/server", "game-project server config",
     "the server processes load bw.xml from the game project's res/server",
     GAME_RES_TICKET, "the port ships no game-project res; the reference keeps"
                      " it only in fantasydemo's res/server"),
    ("res/scripts/entity_defs", "entity definitions",
     "the EntityDef loader reads entity_defs from the game project's res",
     GAME_RES_TICKET, "the port ships no game-project res; the reference keeps"
                      " it only in fantasydemo's res/scripts"),
    ("res/scripts", "game scripts",
     "the server processes run the game scripts from the game project's res",
     GAME_RES_TICKET, "the port ships no game-project res; the reference keeps"
                      " it only in fantasydemo's res/scripts"),
]


def distribution_rows(ref_root):
    """The reference's prebuilt runtime pieces, each with its source path."""
    rows = []
    dist = os.path.join(ref_root, "game", "bin", "server", "el7")
    groups = {
        "server": ("server process", "server/"),
        "tools": ("tool", "tools/"),
        "examples": ("example", "examples/"),
        "unit_tests": ("unit test", "unit_tests/"),
    }
    for name, (kind, rel) in groups.items():
        base = os.path.join(dist, rel)
        if not os.path.isdir(base):
            continue
        for item in sorted(os.listdir(base)):
            if name == "unit_tests" and not item.endswith("_test"):
                continue
            path = os.path.join(base, item)
            if os.path.isfile(path):
                rows.append(_row("distribution/%s" % item, kind,
                                 os.path.join("game/bin/server/el7", rel, item),
                                 DIST_TICKET,
                                 "the reference's prebuilt distribution; %s's"
                                 " scope names the 发行物" % DIST_TICKET))
            elif os.path.isdir(path):
                # A shipped directory (cellapp-extensions, dbapp-extensions,
                # commands) — record its files so nothing in the distribution
                # is silently dropped.
                sub_kind = ("server command" if item == "commands"
                            else "server extension")
                for sub in sorted(os.listdir(path)):
                    if os.path.isfile(os.path.join(path, sub)):
                        rows.append(_row(
                            "distribution/%s/%s" % (item, sub), sub_kind,
                            os.path.join("game/bin/server/el7", rel, item, sub),
                            DIST_TICKET,
                            "the reference's prebuilt distribution; %s's scope"
                            " names the 发行物" % DIST_TICKET))
    py = os.path.join(dist, "third_party", "python")
    if os.path.isdir(py):
        rows.append(_row("distribution/python", "bundled python",
                         "game/bin/server/el7/third_party/python", DIST_TICKET,
                         "the reference bundles its Python runtime; %s's scope"
                         " names the 发行物" % DIST_TICKET))
    scripts = os.path.join(ref_root, "game", "res", "bigworld", "scripts")
    if os.path.isdir(scripts):
        rows.append(_row("distribution/scripts", "runtime scripts tree",
                         "game/res/bigworld/scripts", DIST_TICKET,
                         "the reference ships the runtime scripts tree (common/"
                         " Lib, site-packages, server_common) as 发行物; %s's"
                         " scope names it" % DIST_TICKET))
    return rows


def sdk_rows(sdk_root, port_root):
    """The reference SDK parts, and the port-side gap (no sdk/ tree yet)."""
    rows = []
    parts = ["unity", "unreal", "typescript", "generator"]
    present = []
    if sdk_root:
        for p in parts:
            if os.path.isdir(os.path.join(sdk_root, "client_sdk", p)):
                present.append(p)
                rows.append(_row("sdk/%s" % p, "sdk part",
                                 os.path.join("client_sdk", p), SDK_TICKET,
                                 "the reference's client SDK (%s); %s owns the"
                                 " SDK" % (p, SDK_TICKET)))
    how = ("the port's sdk/ tree is planned by BW-21-01-01; the reference ships"
           " %d SDK part(s) on its own lane/client-sdk branch" % len(present))
    rows.append(_row("sdk/port", "port sdk gap", "the reference client_sdk",
                     "none yet", how))
    return rows


def missing_rows(port_root):
    """The runtime resources the port does not carry, with source and blocker."""
    rows = []
    scripts = os.path.join(port_root, "game", "res", "bigworld", "scripts")
    if not os.path.isdir(scripts):
        rows.append(_row("missing/bundled-scripts", "missing resource",
                         "the server bootstrap reads the runtime scripts from"
                         " game/res/bigworld/scripts", "none yet",
                         "the port ships no bundled runtime scripts tree; %s"
                         " (python-host) embeds CPython but no scripts tree"
                         % SCRIPT_TICKET))
    for probe, label, source, ticket, blocked in PORT_RES_GAPS:
        if os.path.isdir(os.path.join(port_root, probe)):
            continue
        rows.append(_row("missing/%s" % label.replace(" ", "-"), "missing resource",
                         source, "none yet",
                         "%s (turned on by %s)" % (blocked, ticket)))
    return rows


def _row(artifact, kind, source, index, how):
    return {"artifact": artifact, "kind": kind, "source": source,
            "index": index if index else "none yet", "how": how}


def emit(ref_root, port_root, todo_path, out_dir, sdk_root=None, self_check=False):
    if not os.path.isdir(ref_root):
        sys.stderr.write("missing reference: no reference root at %s\n" % ref_root)
        sys.exit(2)
    if not os.path.isdir(port_root):
        sys.stderr.write("missing port: no port root at %s\n" % port_root)
        sys.exit(2)
    scopes = parse_scopes(todo_path)
    if scopes is None:
        sys.stderr.write("missing todo: no todo at %s\n" % todo_path)
        sys.exit(2)

    rows = distribution_rows(ref_root)
    rows.extend(sdk_rows(sdk_root, port_root))
    rows.extend(missing_rows(port_root))

    if self_check:
        problems = []
        n_dist = sum(1 for r in rows if r["artifact"].startswith("distribution/"))
        n_missing = sum(1 for r in rows if r["artifact"].startswith("missing/"))
        # The sdk/port row is always present, so a record that captured nothing
        # about the reference's distribution or the port's resource gaps is
        # hollow even when it is not empty.
        if not rows or (n_dist == 0 and n_missing == 0):
            problems.append("no artifacts in scope")
        for r in rows:
            if not r["source"]:
                problems.append("%s without a source" % r["artifact"])
            if r["index"] == "none yet" and not r["how"]:
                problems.append("%s is a gap without a blocking reason" % r["artifact"])
        if problems:
            sys.stderr.write("hollow map: %s\n" % "; ".join(problems))
            sys.exit(3)

    os.makedirs(out_dir, exist_ok=True)
    n_dist = sum(1 for r in rows if r["artifact"].startswith("distribution/"))
    n_sdk = sum(1 for r in rows if r["artifact"].startswith("sdk/"))
    n_missing = sum(1 for r in rows if r["artifact"].startswith("missing/"))
    with open(os.path.join(out_dir, "coverage-artifacts.json"), "w") as f:
        json.dump({"rows": rows, "count": len(rows),
                   "distribution": n_dist, "sdk": n_sdk, "missing": n_missing,
                   "schema": ["artifact", "kind", "source", "index", "how"]},
                  f, indent=2)
        f.write("\n")
    with open(os.path.join(out_dir, "coverage-artifacts.tsv"), "w") as f:
        f.write("artifact\tkind\tsource\tindex\thow\n")
        for r in rows:
            f.write("%s\t%s\t%s\t%s\t%s\n" % (
                r["artifact"], r["kind"], r["source"], r["index"], r["how"]))
    return rows, n_dist, n_sdk, n_missing


if __name__ == "__main__":
    args = sys.argv[1:]
    self_check = "--self-check" in args
    if self_check:
        args.remove("--self-check")
    sdk_root = None
    while "--sdk-root" in args:
        i = args.index("--sdk-root")
        sdk_root = args[i + 1]
        del args[i:i + 2]
    if len(args) < 4:
        sys.stderr.write("usage: coverage-artifacts.py REFERENCE_ROOT PORT_ROOT"
                         " TODO OUT_DIR [--sdk-root SDK_ROOT] [--self-check]\n")
        sys.exit(2)
    rows, n_dist, n_sdk, n_missing = emit(
        args[0], args[1], args[2], args[3], sdk_root, self_check)
    print("artifacts=%d (distribution=%d, sdk=%d, missing=%d)" % (
        len(rows), n_dist, n_sdk, n_missing))
