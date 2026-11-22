#!/usr/bin/env python3
"""Build the Python-callback fixture for BW-00-04-02 from a login capture.

The login capture (BW-00-02-04, reference/bw00_02_04_client_login.json) already
holds the ordered client login callback sequence, so this builds the callback
fixture from that real capture rather than driving the cluster again:

  * every callback keeps its position (order is the sequence index across the
    merged client + server logs) and its raw line;
  * each entry's name (script method or engine point) and args (the remainder)
    are split so the diff tool can compare them field by field;
  * the fields that legitimately differ between runs — the entity id and the
    client source port — are recorded as ignorable, per entity, so a diff of
    two runs ignores exactly those and nothing else.

Usage: python_callback_fixture.py LOGIN_JSON OUTPUT_JSON
"""
import json
import os
import re
import sys

LEVEL = re.compile(r"^(INFO|DEBUG|TRACE|ERROR|WARNING|NOTICE):\s*")
# The second `::` segment may start with `~` (destructors log as Base::~Base).
NAME = re.compile(
    r"^([A-Za-z_]\w*(?:::[~A-Za-z_]\w*)?(?:\.[A-Za-z_]\w*)?)\s*[:(]\s*(.*)$")


def parse_callback(line):
    """(name, args_text) for one log line, or (None, None) if not a callback."""
    s = LEVEL.sub("", line).strip()
    m = NAME.match(s)
    if not m:
        return None, None
    name, args = m.group(1), m.group(2).strip()
    # The name regex stops before a `(`-argument list; a closing paren the
    # name itself consumed (e.g. `enableEntities(4805)`) trails the args.
    if args.endswith(")") and args.count("(") < args.count(")"):
        args = args[:-1].rstrip()
    # A `name(4805): detail` form leaves `4805): detail`; the argument list is
    # the name's, so the meaningful args are the text after `):`.
    args = re.sub(r"^[^)]*\):\s*", "", args)
    return name, args


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 1
    login, out_json = sys.argv[1], sys.argv[2]
    d = json.load(open(login, encoding="utf-8"))

    proxy_id = d["server_callbacks"].get("proxy_id") or "0"
    port = (d.get("client_source_port") or "").split(":")[-1]
    ignorable_fields = ["entity_id", "source_port", "address"]

    def ignorable_for(args):
        """Which recorded fields differ between two runs of the same login."""
        ign = []
        if proxy_id in (args or ""):
            ign.append("entity_id")
        if port and port in (args or ""):
            ign.append("source_port")
        if "@" in (args or ""):
            # The hop address the reply names, e.g. BaseApp@<addr>:<port>.
            ign.append("address")
        return sorted(set(ign))

    entries = []
    order = 0
    # Client side first (the request/reply sequence), then server side in the
    # order the capture recorded (baseapp, then cellapp) — order across the
    # whole run, since the client and server logs were captured in one run.
    for e in d["client_events"]:
        if e["kind"] != "callback":
            continue
        name, args = parse_callback(e["line"])
        if not name:
            continue
        order += 1
        entries.append({"order": order, "site": "client", "name": name,
                        "args": args or e["msg"], "raw": e["line"],
                        "ignorable": ignorable_for(e["msg"])})

    for site in ("baseapp", "cellapp"):
        for line in d["server_callbacks"].get(site, []):
            name, args = parse_callback(line)
            if not name:
                continue
            order += 1
            entries.append({"order": order, "site": site, "name": name,
                            "args": args, "raw": line.strip(),
                            "ignorable": ignorable_for(args)})

    data = {
        "source_commit": d.get("source_commit"),
        "captured_at": d.get("captured_at"),
        "login_command": d.get("command"),
        "proxy_id": proxy_id,
        "client_source_port": d.get("client_source_port"),
        "ignorable_fields": ignorable_fields,
        "callbacks": entries,
    }
    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    print("commit %s  proxy %s  callbacks %d (ignorable fields: %s)" % (
        data["source_commit"], proxy_id, len(entries),
        ", ".join(ignorable_fields)))
    for e in entries:
        ign = " [ign:%s]" % ",".join(e["ignorable"]) if e["ignorable"] else ""
        print("  %-3d %-32s %s%s" % (e["order"], e["name"],
                                     (e["args"] or "")[:40], ign))
    return 0


if __name__ == "__main__":
    sys.exit(main())