#!/usr/bin/env python3
"""BW-00-05-04: index the common code and the FantasyDemo entries.

The coverage ledger enumerates build targets; the common code and the
FantasyDemo scripts and resources are not build targets — the common code
compiles into other targets' sources, and the demo ships its own script and
resource trees. This mapper gives each entry a traceable index the same way
the target mapper does: the BW-xx ticket whose source scope names it, parsed
from todo.md itself.

  * common code — every file under the reference's common/, indexed by the
    scopes that name its directory (BW-03) and, where a scope names the file
    itself (common/py_network.cpp -> BW-02), both tickets;
  * FantasyDemo — the demo's res/scripts/ script entries, its res/ resource
    entries and its res/server/ server-resource entries, indexed by the
    ticket whose scope token names the demo tree (BW-14). A checkout without
    the demo tree records the absence, not a silent gap.

Usage:
  ci/map-entry.py REFERENCE_SOURCE_ROOT TODO OUT_DIR [--self-check]

Exits 2 when the reference source root is missing; --self-check exits 3 on a
hollow map (no rows, or a row without an index).
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from coverage_scope import parse_scopes, scope_tokens, tickets_for_token  # noqa: E402

COMMON_SUFFIXES = (".cpp", ".hpp", ".h")
DEMO_NAME = "fantasydemo"


def common_rows(src_root, tokens):
    """One row per common-code file, indexed by the scopes that name it."""
    rows = []
    common_dir = os.path.join(src_root, "common")
    if not os.path.isdir(common_dir):
        return rows
    for name in sorted(os.listdir(common_dir)):
        if not name.endswith(COMMON_SUFFIXES):
            continue
        entry = "common/%s" % name
        tickets = tickets_for_token(entry, tokens)
        rows.append({
            "entry": entry,
            "kind": "code",
            "source": entry,
            "index": ",".join(tickets) if tickets else "none",
            "how": "the common code compiles into other targets; the tickets'"
                   " scopes name it" if tickets else
                  "no BW-xx source scope names it",
        })
    return rows


def demo_root(src_root):
    """The FantasyDemo programming tree (programming/fantasydemo)."""
    return os.path.normpath(os.path.join(src_root, "..", DEMO_NAME))


def demo_tickets(tokens):
    """The tickets whose raw scope token names the demo tree."""
    return sorted(t for t, toks in tokens.items()
                  if any(DEMO_NAME in tok for tok in toks))


def demo_rows(src_root, tokens):
    """The demo's script and resource entries, each with its source path."""
    rows = []
    demo = demo_root(src_root)
    tickets = demo_tickets(tokens)
    if not os.path.isdir(demo):
        return rows, ["demo tree absent at %s — %s covers it when the tree is present"
                      % (demo, ",".join(tickets) if tickets else "no ticket")]
    res = os.path.join(demo, "res")
    res_entries = []
    if os.path.isdir(res):
        res_entries = sorted(os.listdir(res))
    for name in res_entries:
        kind = "script" if name == "scripts" else "resource"
        rows.append(_demo_row(demo, "res/%s" % name, kind, tickets))
        if name == "scripts":
            for sub in sorted(os.listdir(os.path.join(res, "scripts"))):
                rows.append(_demo_row(demo, "res/scripts/%s" % sub, "script",
                                      tickets))
        if name == "server" and os.path.isdir(os.path.join(res, "server")):
            for sub in sorted(os.listdir(os.path.join(res, "server"))):
                rows.append(_demo_row(demo, "res/server/%s" % sub, "resource",
                                      tickets))
    if os.path.isdir(os.path.join(demo, "web")):
        rows.append(_demo_row(demo, "web", "resource", tickets))
    return rows, []


def _demo_row(demo, rel, kind, tickets):
    return {
        "entry": rel,
        "kind": kind,
        "source": "%s/%s" % (DEMO_NAME, rel),
        "index": ",".join(tickets) if tickets else "none",
        "how": "the FantasyDemo tree; %s's scope names it"
               % (",".join(tickets) if tickets else "no ticket"),
    }


def emit(src_root, todo_path, out_dir, self_check=False):
    if not os.path.isdir(src_root):
        sys.stderr.write("missing source: no reference source root at %s\n" % src_root)
        sys.exit(2)
    scopes = parse_scopes(todo_path)
    tokens = scope_tokens(todo_path)
    if scopes is None or tokens is None:
        sys.stderr.write("missing todo: no todo at %s\n" % todo_path)
        sys.exit(2)

    rows = common_rows(src_root, tokens)
    demo, demo_notes = demo_rows(src_root, tokens)
    rows.extend(demo)

    if self_check:
        problems = []
        if not rows:
            problems.append("no entries in scope")
        for r in rows:
            for field in ("entry", "kind", "source", "index", "how"):
                if not r[field]:
                    problems.append("a row missing %s" % field)
        if problems:
            sys.stderr.write("hollow map: %s\n" % "; ".join(problems))
            sys.exit(3)

    os.makedirs(out_dir, exist_ok=True)
    n_code = sum(1 for r in rows if r["kind"] == "code")
    n_script = sum(1 for r in rows if r["kind"] == "script")
    n_resource = sum(1 for r in rows if r["kind"] == "resource")
    with open(os.path.join(out_dir, "coverage-entry.json"), "w") as f:
        json.dump({"rows": rows, "count": len(rows),
                   "code": n_code, "script": n_script, "resource": n_resource,
                   "notes": demo_notes,
                   "schema": ["entry", "kind", "source", "index", "how"]}, f, indent=2)
        f.write("\n")
    with open(os.path.join(out_dir, "coverage-entry.tsv"), "w") as f:
        f.write("entry\tkind\tsource\tindex\thow\n")
        for r in rows:
            f.write("%s\t%s\t%s\t%s\t%s\n" % (
                r["entry"], r["kind"], r["source"], r["index"], r["how"]))
    return rows, n_code, n_script, n_resource, demo_notes


if __name__ == "__main__":
    args = sys.argv[1:]
    self_check = "--self-check" in args
    if self_check:
        args.remove("--self-check")
    if len(args) < 3:
        sys.stderr.write("usage: map-entry.py REFERENCE_SOURCE_ROOT TODO OUT_DIR"
                         " [--self-check]\n")
        sys.exit(2)
    rows, n_code, n_script, n_resource, notes = emit(
        args[0], args[1], args[2], self_check)
    for n in notes:
        sys.stderr.write("note: %s\n" % n)
    print("entries=%d (code=%d, script=%d, resource=%d)" % (
        len(rows), n_code, n_script, n_resource))
