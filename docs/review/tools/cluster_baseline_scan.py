#!/usr/bin/env python3
"""Snapshot the running single-machine cluster for BW-00-02-03.

Captures the startup baseline the acceptance asks for — topology, config and
logs — from the cluster brought up by the reference repo's own
scripts/cluster-smoke/deep-chain.sh:

  * topology: the app set, dependency chain, PID, cwd, cmdline, BW_RES_PATH and
    listening sockets of every process still in steady state;
  * config: the server config overlay (bw_deep.xml -> bw_<user>.xml), the asset
    tree resources.xml, the res path and the DB connection parameters;
  * logs: per-app log file (size, the "---- X is running ----" steady marker,
    and a bounded tail) so the evidence never disagrees with the captured run.

Best-effort: every probe falls back to None rather than aborting, so a partial
cluster still yields a usable inventory.

Usage: cluster_baseline_scan.py BIGWORLD_ROOT OUTPUT_JSON
"""
import hashlib
import json
import os
import re
import subprocess
import sys
import time

SERVER_APPS = ("dbappmgr", "cellappmgr", "baseappmgr",
               "dbapp", "cellapp", "baseapp", "loginapp", "serviceapp")
LOG_DIR = "/tmp/cluster-smoke"
LOG_DIR_ENV = "TMPDIR"


def run(cmd, timeout=10):
    try:
        return subprocess.run(
            cmd, capture_output=True, text=True, timeout=timeout
        ).stdout.strip()
    except Exception:
        return None


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def pid_env(pid, key):
    """Read one environment variable from a running process."""
    try:
        with open("/proc/%d/environ" % pid, "rb") as fh:
            for kv in fh.read().split(b"\0"):
                if kv.startswith(key.encode() + b"="):
                    return kv.decode().split("=", 1)[1]
    except Exception:
        return None
    return None


def proc_info(pid):
    info = {}
    try:
        with open("/proc/%d/cmdline" % pid, "rb") as fh:
            info["cmdline"] = fh.read().decode(errors="replace") \
                .replace("\0", " ").strip()
    except Exception:
        info["cmdline"] = None
    try:
        info["cwd"] = os.readlink("/proc/%d/cwd" % pid)
    except Exception:
        info["cwd"] = None
    try:
        with open("/proc/%d/stat" % pid) as fh:
            info["started_ticks"] = fh.read().split()[21]
    except Exception:
        info["started_ticks"] = None
    info["res_path"] = pid_env(pid, "BW_RES_PATH")
    return info


def listening_ports():
    """Map pid -> sorted list of (proto, local address) it listens on."""
    by_pid = {}
    for proto, flags in (("tcp", "-tlnp"), ("udp", "-ulnp")):
        out = run(["ss", flags])
        for line in (out or "").splitlines():
            if "pid=" not in line:
                continue
            parts = line.split()
            if len(parts) < 4:
                continue
            local = parts[3]
            for m in re.finditer(r"pid=(\d+)", line):
                by_pid.setdefault(m.group(1), []).append((proto, local))
    return {p: sorted(set(v)) for p, v in by_pid.items()}


def log_tail(path, n=14):
    try:
        with open(path, errors="replace") as fh:
            return fh.readlines()[-n:]
    except Exception:
        return []


def steady_line(path):
    """Last '---- X is running ----' line, searched the whole log."""
    try:
        found = None
        with open(path, errors="replace") as fh:
            for line in fh:
                if " is running ----" in line:
                    found = line.rstrip("\n")
        return found
    except Exception:
        return None


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 1
    root, out_json = sys.argv[1], sys.argv[2]

    head = run(["git", "-C", root, "rev-parse", "--short", "HEAD"])
    log_dir = os.environ.get(LOG_DIR_ENV, "/tmp") + "/cluster-smoke"

    # Config files (read-only).
    smoke_res = os.path.join(root, "scripts", "cluster-smoke", "res")
    cfg = {
        "server_config_overlay": os.path.join(
            smoke_res, "server", "bw_deep.xml"),
        "server_config_effective": os.path.join(
            smoke_res, "server", "bw_%s.xml" % os.environ.get("USER", "user")),
        "resources_xml": os.path.join(smoke_res, "resources.xml"),
        "res_path": run(["sed", "-n", "s/^BW_RES_PATH=//p",
                         os.path.join(root, "scripts", "cluster-smoke",
                                      "deep-chain.sh")]) or None,
        "db": {"host": "127.0.0.1", "user": "bigworld",
               "pass": "bigworld", "name": "bigworld"},
    }

    ports = listening_ports()
    apps = []
    for name in ("bwmachined2",) + SERVER_APPS:
        pid = None
        try:
            pid = int(run(["pgrep", "-x", name]).splitlines()[0])
        except Exception:
            pass
        entry = {"name": name, "pid": pid}
        if pid:
            entry.update(proc_info(pid))
            entry["ports"] = ports.get(str(pid), [])
        log_file = os.path.join(log_dir, name + ".log")
        if os.path.isfile(log_file):
            entry["log_file"] = log_file
            entry["log_size"] = os.path.getsize(log_file)
            entry["steady_line"] = steady_line(log_file)
            entry["log_tail"] = log_tail(log_file)
        apps.append(entry)

    data = {
        "source_commit": head,
        "captured_at": time.strftime("%Y-%m-%d %H:%M:%S %z"),
        "smoke_script": os.path.join(root, "scripts", "cluster-smoke",
                                     "deep-chain.sh"),
        "server_apps": list(SERVER_APPS),
        "log_dir": log_dir,
        "config": cfg,
        "apps": apps,
    }
    # The smoke script starts a second BaseApp instance (baseapp2.log) as the
    # backup partner that BaseAppMgr needs to hand over a dead BaseApp's
    # entities; capture it as its own row rather than hiding it in the first.
    try:
        base_pids = [int(p) for p in run(["pgrep", "-x", "baseapp"]).split()]
    except Exception:
        base_pids = []
    if len(base_pids) > 1:
        pid = base_pids[1]
        entry = {"name": "baseapp(2)", "pid": pid}
        entry.update(proc_info(pid))
        entry["ports"] = ports.get(str(pid), [])
        log_file = os.path.join(log_dir, "baseapp2.log")
        if os.path.isfile(log_file):
            entry["log_file"] = log_file
            entry["log_size"] = os.path.getsize(log_file)
            entry["steady_line"] = steady_line(log_file)
            entry["log_tail"] = log_tail(log_file)
        apps.append(entry)

    # Add config digests after building (paths are in cfg).
    for k in ("server_config_overlay", "server_config_effective",
              "resources_xml"):
        p = cfg[k]
        if p and os.path.isfile(p):
            cfg[k + "_sha256"] = sha256(p)

    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    running = [a["name"] for a in apps if a["pid"]]
    print("commit: %s  apps up: %d/%d" % (head, len(running), len(apps)))
    print("apps: %s" % ", ".join(running))
    for a in apps:
        if a["pid"]:
            print("  %-12s pid=%-7s ports=%s log=%s" % (
                a["name"], a["pid"], ",".join("%s:%s" % p for p in a["ports"]),
                a.get("log_size", "-")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
