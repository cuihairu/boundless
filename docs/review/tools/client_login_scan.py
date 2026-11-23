#!/usr/bin/env python3
"""Drive a minimal client login against the running cluster (BW-00-02-04).

Runs the reference example client (examples/client_integration/python/simple ->
game/bin/server/el7/examples/simple_python_client) with the account seeded in
the example's db.xml (MyAccount / password a) and captures, in order:

  * the client-side login request sequence (logOnBegin, LoginHandler request
    attempts, replies, LOGGED_ON, enableEntities, "Connected to");
  * the client-side entity callback prints (the Account/ClientAvatar client
    scripts print on __init__/fromCell/createLocalEntity when they run);
  * the server-side log lines that belong to this exact attempt, matched by the
    client's ephemeral source port so earlier runs in the same log files are
    not mixed in.

Usage: client_login_scan.py BIGWORLD_ROOT OUTPUT_JSON
"""
import json
import os
import re
import subprocess
import sys
import time

# Client-side request/callback lines, in the order the client emits them.
CLIENT_PATTERNS = [
    ("request", re.compile(
        r"^\[?\]?\s*ServerConnection::logOnBegin: (?P<msg>.+)$")),
    ("request", re.compile(
        r"^(TRACE|DEBUG|INFO):\s*LoginHandler::start: (?P<msg>.+)$")),
    ("request", re.compile(
        r"^TRACE:\s*LoginRequest::onTransportConnect: (?P<msg>.+)$")),
    ("request", re.compile(
        r"^TRACE:\s*LoginHandler::sendNextRequest: (?P<msg>.+)$")),
    ("reply", re.compile(
        r"^TRACE:\s*LoginRequest::handleMessage: (?P<msg>.+)$")),
    ("callback", re.compile(
        r"^DEBUG:\s*LoginHandler::onLoginAppLoginSuccess: (?P<msg>.+)$")),
    ("callback", re.compile(
        r"^DEBUG:\s*LoginHandler::onBaseAppLoginSuccess: (?P<msg>.+)$")),
    ("callback", re.compile(
        r"^DEBUG:\s*\[Connection\] ServerConnection::logOn: (?P<msg>.+)$")),
    ("callback", re.compile(
        r"^DEBUG:\s*\[Connection\] ServerConnection::enableEntities: (?P<msg>.+)$")),
    ("callback", re.compile(
        r"^DEBUG:\s*SmartServerConnection::logOnTo: (?P<msg>.+)$")),
    ("callback", re.compile(
        r"^Connected to (?P<msg>.+)$")),
    ("callback", re.compile(r"^Login failed: (?P<msg>.+)$")),
]

# The client-side entity scripts print their own callbacks (Account.py,
# ClientAvatar.py); the connection log uses the same prefixes as above.
SCRIPT_PATTERNS = [
    ("callback", re.compile(r"^Client: Account\.__init__.*$")),
    ("callback", re.compile(r"^ClientAvatar\.__init__.*$")),
    ("callback", re.compile(r"^ClientAvatar\.fromCell:.*$")),
    ("callback", re.compile(r"^ClientAvatar\.createLocalEntity:.*$")),
    ("callback", re.compile(r"^Account\.chatMessage:.*$")),
]

# Server-side script callbacks for this run, matched by the client's source port.
SERVER_LOGS = {
    "loginapp": "loginapp.log",
    "baseapp": "baseapp.log",
    "baseapp(2)": "baseapp2.log",
    "cellapp": "cellapp.log",
}


def parse_client_events(text):
    events = []
    for line in text.splitlines():
        for kind, pat in list(CLIENT_PATTERNS) + list(SCRIPT_PATTERNS):
            m = pat.match(line)
            if m:
                events.append({
                    "kind": kind,
                    "line": line.strip(),
                    "msg": m.group("msg").strip() if "msg" in m.groupdict()
                    else line.strip(),
                })
                break
    return events


def source_port(text):
    """Client ephemeral source port, e.g. 192.168.5.188:47409."""
    m = re.search(r"sendNextRequest: From (\S+?), attempt", text)
    if m:
        return m.group(1)
    m = re.search(r"recreateListeningSocket: address (\S+)", text)
    if m:
        return m.group(1)
    return None


def server_events(log_dir, port, username):
    """Server-side lines mentioning this client's port or the login username."""
    if not port:
        return []
    hits = []
    for name, fname in SERVER_LOGS.items():
        path = os.path.join(log_dir, fname)
        try:
            with open(path, errors="replace") as fh:
                for line in fh:
                    if port in line or (":" + port.split(":")[-1]) in line:
                        hits.append({"server": name, "line": line.rstrip("\n")})
        except Exception:
            pass
    return hits


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 1
    root, out_json = sys.argv[1], sys.argv[2]

    bin_dir = os.path.join(root, "game", "bin", "server", "el7", "examples")
    client = os.path.join(bin_dir, "simple_python_client")
    ex_res = os.path.join(root, "programming", "bigworld", "examples",
                          "client_integration", "python", "simple", "res")
    smoke_res = os.path.join(root, "scripts", "cluster-smoke", "res")
    res_path = os.pathsep.join(
        [ex_res, os.path.join(root, "game", "res", "bigworld"), smoke_res])
    log_dir = os.environ.get("TMPDIR", "/tmp") + "/cluster-smoke"

    # -autoDisconnect (main.cpp printUsage) makes the client exit right after the
    # connection comes up, so the run is bounded instead of ticking forever
    # until Ctrl-C.
    cmd = [client, "-server", "127.0.0.1", "-user", "MyAccount",
           "-password", "a", "-autoDisconnect"]
    env = dict(os.environ, BW_RES_PATH=res_path)
    start = time.time()
    try:
        # Merge stderr into stdout: the engine logs DEBUG/TRACE to stderr while
        # the client's own prints go to stdout, so keeping two buffers loses the
        # chronological request/callback order.
        proc = subprocess.run(
            cmd, cwd=root, env=env, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, text=True, timeout=60)
        rc, out = proc.returncode, proc.stdout
    except subprocess.TimeoutExpired as exc:
        def decoded(buf):
            if isinstance(buf, bytes):
                return buf.decode(errors="replace")
            return buf or ""
        rc, out = 124, decoded(exc.stdout) + decoded(exc.stderr)

    events = parse_client_events(out)
    port = source_port(out)
    # Server-side entity callbacks for THIS run. The append-only logs reuse
    # entity ids, so a run is delimited by its own proxy add / entity create
    # marker (the run is the latest activity in each log): capture the
    # ClientAvatar lines after the last such marker.
    # The client auto-disconnects almost instantly; the baseapp still has to
    # flush its proxy / callback lines, so give it a moment before reading.
    time.sleep(2)
    server_events_list = server_events(log_dir, port, "MyAccount")
    proxy_id = None
    for ev in server_events_list:
        m = re.search(r"Adding proxy (\d+) at", ev["line"])
        if m:
            proxy_id = m.group(1)
    def tail_after_last(fnames, marker, needle="ClientAvatar"):
        hits = []
        for fname in fnames:
            path = os.path.join(log_dir, fname)
            try:
                with open(path, errors="replace") as fh:
                    lines = fh.readlines()
            except Exception:
                continue
            idx = max((i for i, l in enumerate(lines)
                       if marker in l), default=-1)
            if idx < 0:
                continue
            if needle:
                hits += [l.rstrip("\n") for l in lines[idx + 1:]
                         if needle in l]
            else:
                hits += [l.rstrip("\n") for l in lines[idx + 1:]]
        return hits[-10:]

    data = {
        "source_commit": subprocess.run(
            ["git", "-C", root, "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True).stdout.strip(),
        "captured_at": time.strftime("%Y-%m-%d %H:%M:%S %z"),
        "command": " ".join(cmd),
        "res_path": res_path,
        "log_dir": log_dir,
        "returncode": rc,
        "seconds": round(time.time() - start, 3),
        "client_source_port": port,
        "client_events": events,
        "server_events": server_events_list,
        "server_callbacks": {
            "proxy_id": proxy_id,
            "baseapp": tail_after_last(
                ["baseapp.log", "baseapp2.log"],
                "Base::Base(%s): ClientAvatar" % proxy_id,
                needle=None) if proxy_id else [],
            "cellapp": tail_after_last(
                ["cellapp.log"],
                "createEntityInternal: New ClientAvatar"),
        },
    }
    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    print("commit %s  rc=%s  %ss  port=%s" % (
        data["source_commit"], rc, data["seconds"], port))
    print("client events: %d   server lines: %d" % (
        len(events), len(data["server_events"])))
    for e in events:
        print("  %-9s %s" % (e["kind"], e["msg"][:90]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
