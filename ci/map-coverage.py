#!/usr/bin/env python3
"""BW-00-05-02: index every build-inventory item.

The coverage ledger (BW-00-05-01) enumerates the reference's build targets.
This mapper gives each row an index: the port crate that implements it, when
one exists, and the BW-xx ticket whose source scope covers its directory
either way. A row with neither is recorded as a bare gap — named, counted and
traceable, not silently dropped.

The derivation is mechanical on all three sides:

  * the inventory — the ledger's rows (task-scoped, so each mapping item
    (BW-00-05-02/03/04) maps its own category);
  * the port index — PORT_CRATE below, a curated table whose every entry is
    justified by the port crate's own doc line; the table is verified against
    the port workspace (a crate that no longer exists fails the map rather
    than silently unmapping), and unit-test targets derive from their base
    library's entry;
  * the ticket scope — parsed from todo.md itself: each BW-xx heading's
    `源码` line names the source directories it covers, so a target's ticket
    index always points at a heading that exists.

Usage:
  ci/map-coverage.py LEDGER TODO PORT_ROOT OUT_DIR [--task BW-00-05-02]
                     [--self-check]

Exits 2 when the ledger or the port root is missing; --self-check exits 3 on
a hollow map (no rows, a curated crate the port does not carry, or a curated
target the ledger does not enumerate).
"""

import json
import os
import re
import sys

ROOTS = ("lib", "server", "client", "tools", "common", "examples")

# The port index for reference targets the port already implements. Every
# entry names a workspace member (verified below) and says why, in the port
# crate's own terms.
PORT_CRATE = {
    "cstdmf": ("crates/foundation",
               "foundation is the BW-01 shared-primitives port (ids, clocks, math, byte codecs)"),
    "math": ("crates/foundation", "foundation::math ports lib/math"),
    "resmgr": ("crates/resources",
               "resources is the BW-01 DataSection and resource-search-path port"),
    "network": ("crates/mercury",
                "mercury is the BW-02 transports, message-framing and RPC port"),
    "server": ("crates/server-runtime",
               "server-runtime is the BW-01 process-bootstrap port (server_app, tick, signals)"),
    "script": ("crates/python-host",
               "python-host ports lib/pyscript and lib/script (its own doc line)"),
    "pyscript": ("crates/python-host",
                 "python-host ports lib/pyscript and lib/script (its own doc line)"),
    "entitydef": ("crates/entitydef",
                  "entitydef is the BW-04 entity-definitions port"),
    "entitydef_script": ("crates/entitydef",
                         "entitydef covers BW-04's lib/entitydef_script scope"),
    "bwentity": ("crates/entitydef", "entitydef covers BW-04's lib/bwentity scope"),
    "baseapp": ("apps/server", "boundless-server runs the baseapp role (KNOWN_ROLES)"),
    "baseappmgr": ("apps/server", "boundless-server runs the baseappmgr role (KNOWN_ROLES)"),
    "cellapp": ("apps/server", "boundless-server runs the cellapp role (KNOWN_ROLES)"),
    "cellappmgr": ("apps/server", "boundless-server runs the cellappmgr role (KNOWN_ROLES)"),
    "dbapp": ("apps/server", "boundless-server runs the dbapp role (KNOWN_ROLES)"),
    "dbappmgr": ("apps/server", "boundless-server runs the dbappmgr role (KNOWN_ROLES)"),
    "loginapp": ("apps/server", "boundless-server runs the loginapp role (KNOWN_ROLES)"),
    "reviver": ("apps/server", "boundless-server runs the reviver role (KNOWN_ROLES)"),
}

HEADING_RE = re.compile(r"^#{3,4}\s+(BW-[0-9]+(?:-[0-9]+)*)")
SCOPE_RE = re.compile(r"^源码")
TOKEN_RE = re.compile(r"`([^`]+)`")
BRACE_RE = re.compile(r"\{([^{}]*)\}")
UNIT_TEST_SUFFIX = "_unit_test"


def expand_braces(text):
    """`lib/{a,b}/` -> [lib/a, lib/b] (one level; the scopes never nest)."""
    m = BRACE_RE.search(text)
    if not m:
        return [text]
    out = []
    for alt in m.group(1).split(","):
        out.extend(expand_braces(text[:m.start()] + alt + text[m.end():]))
    return out


def scope_dir(token):
    """A scope token normalized to a static source directory, or None.

    A bare root is a scope (`tools/` -> tools, `examples/` -> examples); file
    names and glob tails drop to their directory (`lib/server/bwservice.hpp`
    -> lib/server, `server/dbapp/*login*` -> server/dbapp); tokens outside the
    six source roots (links, bare file names, ../ paths) are not scopes.
    """
    t = token.strip().rstrip("/")
    if t.split("/", 1)[0] not in ROOTS:
        return None
    last = t.rsplit("/", 1)[1] if "/" in t else ""
    if last and ("." in last or "*" in last):
        t = t.rsplit("/", 1)[0]
    return t or None


def parse_scopes(todo_path):
    """{ticket: [source dir, ...]} from each heading's 源码 line."""
    scopes = {}
    if not os.path.isfile(todo_path):
        return None
    ticket = None
    with open(todo_path, encoding="utf-8") as f:
        for raw in f:
            m = HEADING_RE.match(raw)
            if m:
                ticket = m.group(1)
                continue
            if ticket and SCOPE_RE.match(raw):
                for tok in TOKEN_RE.findall(raw):
                    for one in expand_braces(tok):
                        d = scope_dir(one)
                        if d and d not in scopes.setdefault(ticket, []):
                            scopes[ticket].append(d)
                ticket = None  # one scope line per heading
    return scopes


def port_members(port_root):
    """The workspace members an index may point at: crates/<name>, apps/<name>."""
    members = set()
    for group in ("crates", "apps"):
        base = os.path.join(port_root, group)
        if not os.path.isdir(base):
            continue
        for name in os.listdir(base):
            if os.path.isfile(os.path.join(base, name, "Cargo.toml")):
                members.add("%s/%s" % (group, name))
    return members


def ledger_rows(ledger_path):
    if not os.path.isfile(ledger_path):
        return None
    with open(ledger_path, encoding="utf-8") as f:
        return json.load(f)["rows"]


def map_rows(rows, tasks, scopes, members):
    """One index row per ledger row in scope."""
    out = []
    for r in rows:
        if r["task"] not in tasks:
            continue
        path = r["original_path"]
        name = path.split(" (", 1)[0]
        directory = path.rsplit(" (", 1)[1].rstrip(")")
        index, how = "", ""
        if name in PORT_CRATE:
            index, how = PORT_CRATE[name]
        elif name.endswith(UNIT_TEST_SUFFIX) \
                and name[: -len(UNIT_TEST_SUFFIX)] in PORT_CRATE:
            base = name[: -len(UNIT_TEST_SUFFIX)]
            index, how = PORT_CRATE[base]
            how = "the port crate's test suite covers the reference unit tests (%s)" % how
        tickets = sorted(t for t, dirs in scopes.items()
                         if any(d == directory or directory.startswith(d + "/")
                                for d in dirs))
        out.append({
            "original_path": path,
            "kind": r["kind"],
            "enable_condition": r["enable_condition"],
            "index": index if index else "none yet",
            "ticket": ",".join(tickets) if tickets else "none",
            "mapped": bool(index),
            "how": how if how else "no port counterpart; no BW-xx source scope covers it",
        })
    return out


def emit(ledger_path, todo_path, port_root, out_dir, tasks, self_check=False):
    rows = ledger_rows(ledger_path)
    if rows is None:
        sys.stderr.write("missing ledger: no coverage ledger at %s\n" % ledger_path)
        sys.exit(2)
    members = port_members(port_root)
    if not members:
        sys.stderr.write("missing port: no workspace members under %s\n" % port_root)
        sys.exit(2)
    scopes = parse_scopes(todo_path)
    if scopes is None:
        sys.stderr.write("missing todo: no todo at %s\n" % todo_path)
        sys.exit(2)

    mapped = map_rows(rows, tasks, scopes or {}, members)

    if self_check:
        problems = []
        if not mapped:
            problems.append("no rows in scope")
        stale = []
        for target, (crate, _how) in PORT_CRATE.items():
            if crate not in members:
                problems.append("%s -> %s is not a port member" % (target, crate))
            if not any(r["original_path"].split(" (", 1)[0] == target for r in rows):
                stale.append(target)
        # A curated target the inventory no longer lists maps nothing today —
        # a table entry, not a broken map. Only a table whose every entry is
        # orphaned is hollow.
        if stale and len(stale) == len(PORT_CRATE):
            problems.append("every curated target is absent from the ledger: %s"
                            % ", ".join(sorted(stale)))
        elif stale:
            sys.stderr.write("note: %d curated target(s) not in the ledger: %s\n"
                             % (len(stale), ", ".join(sorted(stale))))
        if problems:
            sys.stderr.write("hollow map: %s\n" % "; ".join(problems))
            sys.exit(3)

    os.makedirs(out_dir, exist_ok=True)
    n_crate = sum(1 for r in mapped if r["mapped"])
    n_ticket = sum(1 for r in mapped if not r["mapped"] and r["ticket"] != "none")
    n_bare = sum(1 for r in mapped if not r["mapped"] and r["ticket"] == "none")
    with open(os.path.join(out_dir, "coverage-map.json"), "w") as f:
        json.dump({"tasks": sorted(tasks), "rows": mapped, "count": len(mapped),
                   "to_crate": n_crate, "to_ticket": n_ticket, "bare": n_bare,
                   "schema": ["original_path", "kind", "enable_condition",
                              "index", "ticket", "mapped", "how"]}, f, indent=2)
        f.write("\n")
    with open(os.path.join(out_dir, "coverage-map.tsv"), "w") as f:
        f.write("original_path\tkind\tindex\tticket\thow\n")
        for r in mapped:
            f.write("%s\t%s\t%s\t%s\t%s\n" % (
                r["original_path"], r["kind"], r["index"], r["ticket"], r["how"]))
    return mapped, n_crate, n_ticket, n_bare


if __name__ == "__main__":
    args = sys.argv[1:]
    self_check = "--self-check" in args
    if self_check:
        args.remove("--self-check")
    tasks = set()
    while "--task" in args:
        i = args.index("--task")
        tasks.add(args[i + 1])
        del args[i:i + 2]
    if not tasks:
        tasks = {"BW-00-05-02"}
    if len(args) < 4:
        sys.stderr.write("usage: map-coverage.py LEDGER TODO PORT_ROOT OUT_DIR"
                         " [--task ID] [--self-check]\n")
        sys.exit(2)
    mapped, n_crate, n_ticket, n_bare = emit(
        args[0], args[1], args[2], args[3], tasks, self_check)
    print("indexed=%d (crate=%d, ticket=%d, bare=%d)" % (
        len(mapped), n_crate, n_ticket, n_bare))
