#!/usr/bin/env python3
"""BW-00-04-05: the single-component replacement run, per server role.

The substitution path replaces exactly one original role with the port's
equivalent and holds every other component constant. This parser derives, from
the port's own source and the reference's role binaries, the per-role
replacement inventory — which role, what it is replaced by, and whether the
substitution can actually run. The report names the replaced role on every row,
which is the acceptance: the report notes the replaced component.

The run gate is per substitution: the original full stack must be launchable
(server/bw.xml at the resource root + a machined configuration) and the port's
role must be runnable (the server runtime wires the Mercury network interface).
Until either holds, the lane records the closed gate naming the role and the
missing piece — an honest record, not a claim.

Usage:
  ci/replace-role.py REPO_ROOT OUT_DIR [REFERENCE_ROOT] [--self-check]

Exits 2 when the port source the inventory needs is missing; --self-check exits
3 on a hollow inventory (no roles, or a row without a role or gate).
"""

import json
import os
import re
import sys

PORT_APP = os.path.join("crates", "server-runtime", "src", "app.rs")
REF_BIN = os.path.join("game", "bin", "server", "el7", "server")


def port_roles(repo):
    """The port's canonical server role set, from KNOWN_ROLES in app.rs."""
    path = os.path.join(repo, PORT_APP)
    if not os.path.isfile(path):
        return None
    src = open(path, encoding="utf-8", errors="replace").read()
    m = re.search(r"pub const KNOWN_ROLES:\s*&\[&str\]\s*=\s*&\[(.*?)\];", src, re.S)
    if not m:
        return None
    roles = re.findall(r'"(\w+)"', m.group(1))
    return roles or None


def reference_presence(ref):
    """Which reference role binaries are present, as {name: bool}. A reference
    tree absent or without a server bin root records nothing — presence is
    reported as absent, not as a fatal gap, on a runner with no reference tree."""
    if not ref:
        return None
    root = os.path.join(ref, REF_BIN)
    if not os.path.isdir(root):
        return {}
    out = {}
    for name in os.listdir(root):
        if name in ("cellapp-extensions", "dbapp-extensions", "commands"):
            continue
        out[name] = os.path.isfile(os.path.join(root, name))
    return out


def port_gate(repo):
    """Can a port role run at all? The gate opens only when the server runtime
    actually consumes the Mercury crate — the wiring that a live role needs."""
    runtime = os.path.join(repo, "crates", "server-runtime")
    if not os.path.isdir(runtime):
        return {"gate": "closed",
                "reason": "no server-runtime source — the port has no role entry"}
    for dirpath, _dirs, files in os.walk(runtime):
        for name in files:
            if not name.endswith(".rs"):
                continue
            src = open(os.path.join(dirpath, name), encoding="utf-8",
                       errors="replace").read()
            if re.search(r"\buse\s+mercury|mercury::", src):
                return {"gate": "open",
                        "reason": "the port role wires the Mercury network interface"}
    return {"gate": "closed",
            "reason": "the port role validates the role name only — no network, no login path (Mercury joins at BW-02)"}


def inventory(repo, ref=None):
    """The per-role replacement inventory, one row per port role."""
    roles = port_roles(repo)
    if roles is None:
        return None
    gate = port_gate(repo)
    present = reference_presence(ref) if ref else {}
    rows = []
    for role in roles:
        rows.append({
            "role": role,
            "reference_binary": "%s/%s" % (REF_BIN, role),
            "reference_present": bool(present.get(role)),
            "port_entry": "boundless-server %s" % role,
            "gate": gate["gate"],
            "gate_reason": gate["reason"],
        })
    return {
        "roles": rows,
        "count": len(rows),
        "substitution": "one role replaced, the rest held constant",
        "source": "crates/server-runtime/src/app.rs + %s" % REF_BIN,
    }


def emit(repo, out_dir, ref=None, self_check=False):
    app = os.path.join(repo, PORT_APP)
    if not os.path.isfile(app):
        sys.stderr.write("missing source(s): %s\n" % PORT_APP)
        sys.exit(2)
    inv = inventory(repo, ref)
    if not inv or not inv["roles"]:
        sys.stderr.write("could not derive a per-role replacement inventory\n")
        sys.exit(2)

    if self_check:
        problems = []
        if not inv["roles"]:
            problems.append("no roles")
        for row in inv["roles"]:
            if not row["role"]:
                problems.append("a row with no role")
            if not row["gate"]:
                problems.append("a row with no gate")
        if problems:
            sys.stderr.write("hollow inventory: %s\n" % "; ".join(problems))
            sys.exit(3)

    os.makedirs(out_dir, exist_ok=True)
    doc = {
        "substitution": inv["substitution"],
        "gate": inv["roles"][0]["gate"] if inv["roles"] else "unknown",
        "gate_reason": inv["roles"][0]["gate_reason"] if inv["roles"] else "",
        "roles": inv["roles"],
        "source": inv["source"],
    }
    with open(os.path.join(out_dir, "replace-roles.json"), "w") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")
    with open(os.path.join(out_dir, "replace-roles.tsv"), "w") as f:
        f.write("role\treference_binary\treference_present\tport_entry\tgate\tgate_reason\n")
        for row in inv["roles"]:
            f.write("%s\t%s\t%s\t%s\t%s\t%s\n" % (
                row["role"], row["reference_binary"],
                "yes" if row["reference_present"] else "no",
                row["port_entry"], row["gate"], row["gate_reason"]))
    return inv


if __name__ == "__main__":
    args = sys.argv[1:]
    self_check = "--self-check" in args
    if self_check:
        args.remove("--self-check")
    repo = args[0] if len(args) > 0 else os.getcwd()
    out_dir = args[1] if len(args) > 1 else "."
    ref = args[2] if len(args) > 2 else None
    inv = emit(repo, out_dir, ref, self_check)
    print("roles=%d (gate %s, commit n/a)" % (inv["count"], inv["roles"][0]["gate"]))
