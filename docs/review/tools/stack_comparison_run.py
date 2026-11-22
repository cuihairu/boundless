#!/usr/bin/env python3
"""Run the reference full stack and the new full stack from one input.

For BW-00-04-04 a *comparison run* means: one input (role set + resource path +
config) drives both stacks, and each produces its own evidence file. The
reference side is the real server binaries brought up as the deep-chain full
app set (observed when another session owns the cluster, mode `live`; brought
up and torn down when it is free, mode `owned`). The new side is the Rust
`boundless-server` entry for every role in the same input.

The two stacks are not at the same depth yet: the reference reaches a full
steady state (machined, managers, dbapp/cellapp/baseapp/loginapp, each logging
`---- <App> is running ----`), while the new entry reaches process startup
only (it validates the role and prints its startup line; the deeper init chain
arrives with BW-01-04). The harness records exactly what each side reached, so
the difference itself is the evidence, not an assumption.

Usage: stack_comparison_run.py BIGWORLD_ROOT NEW_BIN INPUT_JSON OUT_REF OUT_NEW
"""
import json
import os
import subprocess
import sys
import time

# reference role -> (binary name, running marker in the log, log file).
REFERENCE = {
    "dbappmgr": ("dbappmgr", "DBAppMgr"),
    "cellappmgr": ("cellappmgr", "CellAppMgr"),
    "baseappmgr": ("baseappmgr", "BaseAppMgr"),
    "dbapp": ("dbapp", "DBApp"),
    "cellapp": ("cellapp", "CellApp"),
    "baseapp": ("baseapp", "BaseApp"),
    "loginapp": ("loginapp", "LoginApp"),
    "serviceapp": ("serviceapp", "ServiceApp"),
}
# The new entry's KNOWN_ROLES (crates/server-runtime/src/app.rs).
NEW_ROLES = {"baseapp", "baseappmgr", "cellapp", "cellappmgr", "dbapp",
             "dbappmgr", "loginapp", "reviver"}


def run(cmd, timeout=60, env=None, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout,
                          env=env, **kw)


def observe_once(input_data):
    """One read-only per-role observation of the reference stack."""
    log_dir = input_data["log_dir"]
    out = []
    for role in sorted(REFERENCE):
        binary, marker = REFERENCE[role]
        bpath = os.path.join(input_data["reference_bin_dir"], binary)
        present = bool(run(["pgrep", "-x", binary]).stdout.strip())
        logf = os.path.join(log_dir, role + ".log")
        found = False
        if os.path.exists(logf):
            with open(logf, errors="replace") as fh:
                found = any("---- %s is running ----" % marker in l
                            for l in fh)
        out.append({
            "role": role, "binary": bpath, "process_present": present,
            "log_file": logf, "marker": "---- %s is running ----" % marker,
            "marker_found": found,
        })
    # The second BaseApp (backup partner) deep-chain always starts.
    baseapp2 = os.path.join(log_dir, "baseapp2.log")
    out.append({
        "role": "baseapp(2)", "binary": os.path.join(
            input_data["reference_bin_dir"], "baseapp"),
        "process_present": len(run(["pgrep", "-x", "baseapp"]).stdout.split()) > 1,
        "log_file": baseapp2,
        "marker": "---- BaseApp is running ----",
        "marker_found": os.path.exists(baseapp2) and any(
            "---- BaseApp is running ----" in l
            for l in open(baseapp2, errors="replace")),
    })
    return out


def is_full(obs):
    """Every role present and its running marker in the log."""
    return all(r["process_present"] and r["marker_found"] for r in obs)


def reference_observe(root, input_data, mode):
    """Reference observation, polling for a full steady snapshot (read-only).

    The cluster may be owned by another session that cycles it; poll (1s) up
    to `poll_seconds` for a snapshot where every role is present and logged
    `---- <App> is running ----`. That snapshot is the reference's reach. If
    the window never closes, keep the last observation and mark it partial.
    """
    deadline = time.time() + input_data.get("poll_seconds", 150)
    last, waited = None, 0.0
    while time.time() < deadline:
        last = observe_once(input_data)
        if is_full(last):
            return last, round(waited, 1), True
        time.sleep(1)
        waited += 1
    return last, round(waited, 1), False


def reference_run(root, input_data):
    """Bring the reference full stack up (owned) or observe it (live)."""
    smoke = os.path.join(root, "scripts", "cluster-smoke", "deep-chain.sh")
    if run(["pgrep", "-x", "bwmachined2"]).stdout.strip():
        mode = "live"
        rc = seconds = None
    else:
        mode = "owned"
        t0 = time.time()
        up = run(["bash", smoke], timeout=180)
        rc = up.returncode
        seconds = round(time.time() - t0, 1)
    return mode, rc, seconds


def new_observe(new_bin, input_data):
    """Per-role new-side observation: run the entry, capture rc + output."""
    out = []
    env = dict(os.environ, BW_RES_PATH=input_data["res_path"])
    for role in sorted(input_data["roles"]):
        try:
            p = run([new_bin, role], timeout=20, env=env)
            rc, stdout, stderr = p.returncode, p.stdout, p.stderr
            timed_out = False
        except subprocess.TimeoutExpired as exc:
            rc = 124
            stdout = exc.stdout.decode() if isinstance(exc.stdout, bytes) \
                else (exc.stdout or "")
            stderr = exc.stderr.decode() if isinstance(exc.stderr, bytes) \
                else (exc.stderr or "")
            timed_out = True
        output = (stderr or stdout).strip().splitlines()
        out.append({
            "role": role,
            "accepted": role in NEW_ROLES,
            "rc": rc,
            "timed_out": timed_out,
            "output": output[0] if output else "",
            "stdout": stdout.strip(),
            "stderr": stderr.strip(),
        })
    return out


def main():
    if len(sys.argv) < 6:
        print(__doc__)
        return 1
    root, new_bin, input_json, out_ref, out_new = sys.argv[1:6]
    with open(input_json, encoding="utf-8") as fh:
        input_data = json.load(fh)

    mode, rc, seconds = reference_run(root, input_data)
    ref_roles, poll_seconds, full = reference_observe(root, input_data, mode)

    ref_evidence = {
        "source_commit": run(["git", "-C", root, "rev-parse", "--short",
                              "HEAD"]).stdout.strip(),
        "captured_at": time.strftime("%Y-%m-%d %H:%M:%S %z"),
        "mode": mode,
        "bringup_rc": rc, "bringup_seconds": seconds,
        "poll_seconds": poll_seconds,
        "full_steady": full,
        "input": input_data,
        "roles": ref_roles,
    }
    new_evidence = {
        "source_commit": run(["git", "rev-parse", "--short", "HEAD"]).stdout.strip(),
        "captured_at": time.strftime("%Y-%m-%d %H:%M:%S %z"),
        "input": input_data,
        "binary": new_bin,
        "roles": new_observe(new_bin, input_data),
    }
    with open(out_ref, "w", encoding="utf-8") as fh:
        json.dump(ref_evidence, fh, indent=1, sort_keys=True)
    with open(out_new, "w", encoding="utf-8") as fh:
        json.dump(new_evidence, fh, indent=1, sort_keys=True)

    print("mode=%s  ref bringup rc=%s (%ss)  new bin=%s" % (
        mode, rc, seconds, new_bin))
    print("reference snapshot: full_steady=%s after %.1fs polling" % (
        full, poll_seconds))
    print("== reference ==")
    for r in ref_roles:
        print("  %-14s present=%-5s marker=%-5s" % (
            r["role"], r["process_present"], r["marker_found"]))
    print("== new ==")
    for r in new_evidence["roles"]:
        print("  %-14s accepted=%-5s rc=%-3s %s" % (
            r["role"], r["accepted"], r["rc"], r["output"][:50]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
