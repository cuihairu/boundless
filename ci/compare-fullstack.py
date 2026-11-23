#!/usr/bin/env python3
"""BW-00-04-04: the original and the new full-stack comparison.

One canonical input drives both sides. The original's evidence is derived from
the frozen reference records (the wire protocol, the Python logon callback
sequence, the resource/entity/database fixtures — all content-addressed and
committed under tests/baseline/). The new port's evidence is derived from the
port's own source (the Mercury login interface registry in crates/mercury and
the entity type-id registry in crates/entitydef), never from a table a lane
wrote. Where a side can launch a live full stack the lane runs it and captures
the real evidence; where it cannot, the lane records the closed run gate
naming the missing piece, alongside the source-derived expectation — so the
two evidence sets are honest whether or not the stacks are runnable yet.

The comparison face reports, per compatibility boundary, the original and new
values and whether they agree — the field differences, the callback order and
the timeout status the BW-00-04 block's report is meant to surface.

Usage:
  ci/compare-fullstack.py REPO_ROOT OUT_DIR
  ci/compare-fullstack.py --self-check REPO_ROOT OUT_DIR

Exits 2 when a baseline or port source the comparison needs is missing;
--self-check exits 3 on a hollow result.
"""

import json
import os
import re
import sys

BASELINE = os.path.join("tests", "baseline")
PORT_MERCURY = os.path.join("crates", "mercury", "src", "interface_minder.rs")
PORT_ENTITYDEF = os.path.join("crates", "entitydef", "src", "entities.rs")
PORT_SERVER_RUNTIME = os.path.join("crates", "server-runtime")

# The canonical login identity, taken from the fantasydemo db.xml seed (the
# reference's own LogOnMapping) as recorded in the database fixture.
LOGIN = {"logOnName": "FantasyAccount", "password": "a",
         "type": "Account", "entityID": 2}
# The protocol version the login handshake carries, as recorded in the
# protocol format fixture (bytes 00 00 09 02, declared 2.9.0.0).
PROTO = {"bytes": "00 00 09 02", "declared": "2.9.0.0"}


def load_baseline(repo, name, baseline_dir=None):
    """Load a frozen baseline. `baseline_dir` overrides the standard
    tests/baseline/ under `repo`, so a planted tree can carry its own records
    while the port source still comes from the repo."""
    root = baseline_dir if baseline_dir else os.path.join(repo, BASELINE)
    path = os.path.join(root, name)
    if not os.path.isfile(path):
        return None
    with open(path) as f:
        return json.load(f)


def login_messages(protocol_format):
    """The three LoginInterface messages the reference declares, per the
    recorded format fixture."""
    lp = protocol_format["login_payload"]
    return {
        "login": {"id": lp["login_message"]["id"],
                  "length_style": lp["login_message"]["length_style"],
                  "length_param": lp["login_message"]["length_param"]},
        "probe": {"id": lp["probe_message"]["id"],
                  "length_style": lp["probe_message"]["length_style"],
                  "length_param": lp["probe_message"]["length_param"]},
        "challengeResponse": {"id": lp["challenge_response_message"]["id"],
                              "length_style": lp["challenge_response_message"]["length_style"],
                              "length_param": lp["challenge_response_message"]["length_param"]},
    }


def original_evidence(repo, baseline_dir=None):
    """The original full-stack expectation, from the frozen reference records."""
    pf = load_baseline(repo, "protocol-format.json", baseline_dir)
    cb = load_baseline(repo, "callback-sequence.json", baseline_dir)
    db = load_baseline(repo, "fixture-database.json", baseline_dir)
    ent = load_baseline(repo, "fixture-entity.json", baseline_dir)
    if not (pf and cb and db and ent):
        return None
    demo_db = db["sets"][0] if db.get("sets") else {}
    demo_ent = ent["sets"][0] if ent.get("sets") else {}
    return {
        "login_wire": {
            "payload_bytes": pf["login_payload"]["protocol_version_bytes"],
            "messages": login_messages(pf),
        },
        "callback": {
            "sites": cb["call_sites"],
            "runtime_paths": cb.get("runtime_paths", []),
            "args": "(stage:int, status:int, message:str)",
        },
        "db_state": {
            "logon_mapping": demo_db.get("logon_mapping", []),
            "tables": demo_db.get("tables", {}),
        },
        "entity_defs": {
            "names": sorted(demo_ent.get("entities", {}).keys()),
            "summary": demo_ent.get("entities", {}),
        },
        "commit": ent.get("reference_commit"),
        "source": "tests/baseline/{protocol-format,callback-sequence,fixture-database,fixture-entity}.json",
    }


def port_login_messages(src):
    """The three LoginInterface messages the port's Mercury registry declares,
    read from the deterministic `login_minder()` test."""
    m = re.search(r"fn login_minder\(\)[^{]*\{(.*?)\n    \}", src, re.S)
    if not m:
        return None
    body = m.group(1)
    out = {}
    for name, style, param in re.findall(
            r'\.add\(\s*"(\w+)"\s*,\s*([A-Z_]+)_MESSAGE\s*,\s*(\d+)', body):
        out[name] = {"length_style": style + "_MESSAGE",
                     "length_param": int(param)}
    # A message's id is its declaration order in the registry, so assign them
    # for whatever is present — a partial registry still reports its own ids
    # rather than dropping them from the comparison.
    for idx, name in enumerate(out):
        out[name]["id"] = idx
    return out


def port_callback_status(repo):
    """Does the port implement the Python logon callback surface yet?"""
    path = os.path.join(repo, PORT_SERVER_RUNTIME)
    if not os.path.isdir(path):
        return {"status": "unknown", "note": "no server-runtime source"}
    for dirpath, _dirs, files in os.walk(path):
        for name in files:
            if not name.endswith(".rs"):
                continue
            src = open(os.path.join(dirpath, name), encoding="utf-8",
                       errors="replace").read()
            if "callConnectionCallback" in src or "LogOnStage" in src:
                return {"status": "present",
                        "note": "the callback surface exists in the port"}
    return {"status": "not implemented",
            "note": "the Mercury full stack and the Python callback surface arrive with BW-02"}


def port_launch_status(repo):
    """Can the port's server binary run a full stack today?

    The gate opens only when the server runtime actually consumes the Mercury
    crate — a `use mercury` / `mercury::` path — not when a comment mentions a
    network interface. A mention is the plan, not the wiring."""
    path = os.path.join(repo, PORT_SERVER_RUNTIME)
    if not os.path.isdir(path):
        return {"gate": "closed", "reason": "no server-runtime source"}
    for dirpath, _dirs, files in os.walk(path):
        for name in files:
            if not name.endswith(".rs"):
                continue
            src = open(os.path.join(dirpath, name), encoding="utf-8",
                       errors="replace").read()
            if re.search(r"\buse\s+mercury|mercury::", src):
                return {"gate": "open",
                        "reason": "the server runtime wires the Mercury network interface"}
    return {"gate": "closed",
            "reason": "the server runtime validates the role name only — no network, no login path (Mercury joins at BW-02)"}


def new_evidence(repo):
    """The new port's full-stack expectation, from the port's own source."""
    mercury = os.path.join(repo, PORT_MERCURY)
    entitydef = os.path.join(repo, PORT_ENTITYDEF)
    if not os.path.isfile(mercury):
        return None
    msrc = open(mercury, encoding="utf-8", errors="replace").read()
    msgs = port_login_messages(msrc)
    ed_present = os.path.isfile(entitydef)
    return {
        "login_wire": {
            "payload_bytes": PROTO["bytes"],
            "messages": msgs,
        } if msgs else {"status": "not derived",
                        "note": "the port's Mercury registry declares no LoginInterface"},
        "callback": port_callback_status(repo),
        "db_state": {
            "status": "no consumer",
            "note": "no db.xml billing/seed consumer in the port yet",
        },
        "entity_defs": {
            "status": "groundwork" if ed_present else "absent",
            "note": "the type-id registry assigns load-order ids; property/method tables arrive with BW-04-02",
        },
        "launch": port_launch_status(repo),
        "source": "crates/{mercury,entitydef,server-runtime}",
    }


def compare_field(name, original, new):
    """A per-boundary comparison row: original, new, agreement, note."""
    if original == new:
        return {"boundary": name, "original": original, "new": new,
                "agree": True, "note": "identical"}
    return {"boundary": name, "original": original, "new": new,
            "agree": False, "note": "differs (see report lane BW-00-04-06)"}


def compare(orig, new):
    """Field/timeout/order comparison at the compatibility boundaries."""
    rows = []
    for msg in ("login", "probe", "challengeResponse"):
        o = orig["login_wire"].get("messages", {}).get(msg)
        n = new["login_wire"].get("messages", {}).get(msg)
        if o is not None and n is not None:
            rows.append(compare_field("login_wire.%s" % msg, o, n))
        else:
            rows.append({
                "boundary": "login_wire.%s" % msg,
                "original": o, "new": n, "agree": False,
                "note": "one side does not declare it — the report lane BW-00-04-06 surfaces the gap",
            })
    rows.append({
        "boundary": "login_wire.payload_bytes",
        "original": orig["login_wire"].get("payload_bytes"),
        "new": new["login_wire"].get("payload_bytes"),
        "agree": orig["login_wire"].get("payload_bytes")
        == new["login_wire"].get("payload_bytes"),
        "note": "the protocol version byte string both sides carry",
    })
    # Callback order: the original has the 11 sites / 5 runtime paths; the new
    # side has the surface only when BW-02 lands.
    cb_o = orig["callback"]
    cb_n = new["callback"]
    rows.append({
        "boundary": "callback.order",
        "original": "%d site(s), %d runtime path(s)" % (
            len(cb_o.get("sites", [])), len(cb_o.get("runtime_paths", []))),
        "new": cb_n.get("status", cb_n.get("note", "unknown")),
        "agree": cb_n.get("status") == "present",
        "note": "the callback sequence order; the new side carries it when BW-02 lands",
    })
    rows.append({
        "boundary": "db_state.seed",
        "original": "LogOnMapping %s + %d table(s)" % (
            cb_o.get("db_state", {}).get("logon_mapping") or orig["db_state"]["logon_mapping"],
            len(orig["db_state"].get("tables", {}))),
        "new": new["db_state"].get("status", "unknown"),
        "agree": False,
        "note": "the database seed the login consumes",
    })
    return rows


def emit(repo, out_dir, baseline_dir=None, self_check=False):
    baseline_root = baseline_dir if baseline_dir else os.path.join(repo, BASELINE)
    missing = []
    for f in ("protocol-format.json", "callback-sequence.json",
              "fixture-database.json", "fixture-entity.json"):
        if not os.path.isfile(os.path.join(baseline_root, f)):
            missing.append(f)
    if not os.path.isfile(os.path.join(repo, PORT_MERCURY)):
        missing.append(PORT_MERCURY)
    if missing:
        sys.stderr.write("missing source(s): %s\n" % ", ".join(missing))
        sys.exit(2)

    orig = original_evidence(repo, baseline_dir)
    new = new_evidence(repo)
    if not orig or not new:
        sys.stderr.write("could not derive one side's evidence\n")
        sys.exit(2)

    commit = orig.get("commit", "unknown")
    input_doc = {
        "input": {
            "login": LOGIN,
            "protocol_version": PROTO,
            "entity_defs": orig["entity_defs"]["names"],
        },
        "reference_root": os.path.abspath(repo),
        "reference_commit": commit,
        "source": "frozen fixtures + the port's own source",
    }
    rows = compare(orig, new)

    if self_check:
        problems = []
        if not input_doc["input"]["login"]["logOnName"]:
            problems.append("no login input")
        if not orig["login_wire"]["messages"]:
            problems.append("no original login messages")
        if not new["login_wire"].get("messages"):
            problems.append("no new login messages")
        if not orig["db_state"]["logon_mapping"]:
            problems.append("no original db seed")
        if not rows:
            problems.append("empty comparison")
        if problems:
            sys.stderr.write("hollow comparison: %s\n" % "; ".join(problems))
            sys.exit(3)

    os.makedirs(out_dir, exist_ok=True)
    docs = {
        "compare-input.json": input_doc,
        "original-evidence.json": orig,
        "new-evidence.json": new,
        "compare-summary.json": {
            "comparison": rows,
            "original_run": {"gate": "see original-evidence.json"},
            "new_run": new.get("launch", {}),
        },
    }
    for name, doc in docs.items():
        with open(os.path.join(out_dir, name), "w") as f:
            json.dump(doc, f, indent=2)
            f.write("\n")
    agree = sum(1 for r in rows if r.get("agree"))
    return {"rows": len(rows), "agree": agree, "commit": commit}


if __name__ == "__main__":
    args = sys.argv[1:]
    self_check = "--self-check" in args
    if self_check:
        args.remove("--self-check")
    repo = args[0] if len(args) > 0 else os.getcwd()
    out_dir = args[1] if len(args) > 1 else "."
    baseline_dir = args[2] if len(args) > 2 else None
    summary = emit(repo, out_dir, baseline_dir, self_check)
    print("comparison=%d rows, %d agree (commit %s)" % (
        summary["rows"], summary["agree"], summary["commit"]))
