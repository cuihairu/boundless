#!/usr/bin/env python3
"""Derive the Python logon callback sequence the reference client runs.

`BigWorld.connect( server, loginParams, progressFn )` takes a Python callback
that the client drives through `ConnectionControl::callConnectionCallback`
(client/connection_control.cpp):

    Py_BuildValue( "(iNs)", stage, Script::getData( status.value() ),
            message.c_str() );

so a callback sees exactly three arguments — an int stage, an int status and a
string message. The stage comes from the `LogOnStage` enum in
client/connection_control.hpp and the status from `LogOnStatus` in
lib/connection/log_on_status.hpp; both are read out of the enum bodies here so
the numeric values are the ones the original compiles, not ones a table wrote.

There is no C preprocessor and no runtime to run, so the sequence is derived
from the call sites themselves: every `callConnectionCallback(...)` in
connection_control.cpp, in file order, tagged with the function it sits in.
That function is the runtime context (a success reaches onLoggedOn then
onBasePlayerCreated; a failure reaches onLogOnFailure; a disconnect reaches
disconnected), so the lanes below can order them honestly.

The status of a call site is recorded as one of three things: the enum name
when the call passes a `LogOnStatus::X` literal; `runtime` when it passes a
non-literal expression (the failure path hands over its own `status`
parameter); `NOT_SET` when it passes no status argument at all, since the
declaration's default is `LogOnStatus::NOT_SET` (connection_control.hpp).

Usage:
  ci/reference-callback.py REFERENCE_ROOT [OUT_JSON]

Emits a JSON object with the stage map, the status map and every call site.
Exits 2 when a source the derivation needs is missing.
"""

import json
import os
import re
import sys

SOURCES = (
    "client/connection_control.hpp",
    "client/connection_control.cpp",
    "lib/connection/log_on_status.hpp",
)


def strip_comments(src):
    src = re.sub(r"/\*.*?\*/", lambda m: "\n" * m.group(0).count("\n"), src,
                 flags=re.S)
    return re.sub(r"//[^\n]*", "", src)


def parse_enum(src, name):
    """name -> value for the members of `enum name { ... }`."""
    m = re.search(r"\benum\s+" + re.escape(name) + r"\s*\{(.*?)\}", src, re.S)
    if not m:
        return None
    body = m.group(1)
    out, val = {}, 0
    for member in body.split(","):
        member = member.strip()
        if not member:
            continue
        mm = re.match(r"([A-Za-z_]\w*)\s*(?:=\s*(-?\w+))?", member)
        if not mm:
            continue
        name_, rhs = mm.group(1), mm.group(2)
        if rhs is not None:
            try:
                val = int(rhs, 0)
            except ValueError:
                # An identifier (e.g. LAST_CLIENT_SIDE_VALUE = 63 is numeric,
                # but a few members alias others); keep the running value.
                val = val + 1 if not name_ else val
                continue
        out[name_] = val
        val += 1
    return out


DEF_RE = re.compile(r"^(?:\w+[\w:<>,*&\s]*)\b\w+::(\w+)\s*\(")


def definition_lines(src):
    """Sorted (line, function name) for every `Type Class::name( ... )`
    definition at column 0."""
    out = []
    for i, line in enumerate(src.split("\n"), 1):
        m = DEF_RE.match(line)
        if m and "return" not in line and "=" not in line:
            out.append((i, m.group(1)))
    return out


def enclosing(definitions, line):
    """The most recent definition above `line` — the function a call at that
    line sits in."""
    name = None
    for def_line, def_name in definitions:
        if def_line > line:
            break
        name = def_name
    return name


def split_args(body):
    """The top-level arguments of a call's argument list, split on commas at
    depth zero (nested parens stay inside their argument)."""
    args, depth, start = [], 0, 0
    for i, ch in enumerate(body):
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        elif ch == "," and depth == 0:
            args.append(body[start:i].strip())
            start = i + 1
    args.append(body[start:].strip())
    return [a for a in args if a]


def call_sites(src, definitions):
    """Every callConnectionCallback( ... ) call site, in file order.

    A definition's own signature is excluded by rejecting any match whose
    text contains a `{`: a call's argument list closes before the body opens,
    while a definition spilling past its signature has swallowed the brace."""
    sites = []
    for m in re.finditer(r"callConnectionCallback\s*\(([^;]*?)\)\s*;",
                         src, re.S):
        body = m.group(1)
        if "{" in body:
            continue
        args = split_args(body)
        # The first argument is the stage token; the status argument, if the
        # call passes one, names the status. The message and the
        # shouldCallNextFrame flag sit in the other slots and neither is
        # visible to the Python callback.
        stage = None
        status = None
        sm = re.search(r"\b(STAGE_\w+)\b", body)
        if sm:
            stage = sm.group(1)
        if len(args) >= 3:
            tm = re.search(r"LogOnStatus::(\w+)", args[2])
            if tm:
                status = tm.group(1)
            else:
                status = "runtime"
        else:
            status = "NOT_SET"
        line = src[:m.start()].count("\n") + 1
        sites.append({
            "line": line,
            "function": enclosing(definitions, line),
            "stage": stage,
            "status": status,
        })
    return sites


def derive(ref):
    """The callback record: stage map, status map and every call site."""
    missing = [s for s in SOURCES if not os.path.isfile(os.path.join(ref, s))]
    if missing:
        return None, missing
    hpp = strip_comments(open(os.path.join(ref, SOURCES[0])).read())
    cpp = strip_comments(open(os.path.join(ref, SOURCES[1])).read())
    status_hpp = strip_comments(open(os.path.join(ref, SOURCES[2])).read())

    stages = parse_enum(hpp, "LogOnStage")
    statuses = parse_enum(status_hpp, "Status")
    if not stages or not statuses:
        return None, []
    return {
        "stage_enum": stages,
        "status_enum": statuses,
        "call_sites": call_sites(cpp, definition_lines(cpp)),
    }, []


if __name__ == "__main__":
    ref = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        os.environ.get("HOME", ""), "workspaces", "BigWorld")
    data, missing = derive(ref)
    if data is None:
        sys.stderr.write("missing source(s): %s\n" % ", ".join(missing))
        sys.exit(2)
    out = sys.stdout
    if len(sys.argv) > 2:
        out = open(sys.argv[2], "w")
    json.dump(data, out, indent=2)
    out.write("\n")
    if out is not sys.stdout:
        out.close()