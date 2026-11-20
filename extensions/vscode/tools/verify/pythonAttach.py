"""BW-20-02-05 evidence: attach a Python debugger to a running script host and
hit a breakpoint inside a BaseApp entity callback.

The callback is the reference example's real BaseApp script
(`examples/client_integration/python/simple/res/scripts/base/ClientAvatar.py`);
only the `BigWorld` binding is stubbed, because that binding is the server
itself. The DAP frames are exchanged by hand: recent debugpy wheels ship no
bundled client, so this uses the same wire protocol VS Code uses.

usage: python3 tools/verify/pythonAttach.py
"""

import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REFERENCE = os.environ.get(
    "BW_REFERENCE", "/home/cui/workspaces/BigWorld"
)
CLIENT_AVATAR = os.path.join(
    REFERENCE,
    "programming",
    "bigworld",
    "examples",
    "client_integration",
    "python",
    "simple",
    "res",
    "scripts",
    "base",
    "ClientAvatar.py",
)

PORT = 5678


class Dap:
    """The smallest DAP client: framed JSON over TCP."""

    def __init__(self, host, port):
        self.sock = socket.create_connection((host, port), timeout=60)
        self.seq = 0
        self.pending = {}
        self.events = []
        self.outputs = []
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        while True:
            header = self._read_line()
            if header is None:
                return
            if not header.startswith("Content-Length:"):
                continue
            length = int(header.split(":", 1)[1])
            while True:  # remaining header lines, up to the blank one
                line = self._read_line()
                if line is None or line == "":
                    break
            body = b""
            while len(body) < length:
                chunk = self.sock.recv(length - len(body))
                if not chunk:
                    return
                body += chunk
            message = json.loads(body.decode("utf-8"))
            if message.get("type") == "event":
                self.events.append(message)
                if message["event"] == "output":
                    self.outputs.append(message.get("body", {}).get("output", ""))
            elif message.get("type") == "response":
                self.pending[message.get("request_seq", message.get("seq"))] = message

    def _read_line(self):
        data = b""
        while not data.endswith(b"\n"):
            byte = self.sock.recv(1)
            if not byte:
                return None
            data += byte
        return data.decode("utf-8", "replace").strip()

    def send(self, command, **body):
        self.seq += 1
        payload = json.dumps(
            {"seq": self.seq, "type": "request", "command": command, "arguments": body}
        ).encode("utf-8")
        self.sock.sendall(f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload)
        return self.seq

    def call(self, command, timeout=60, **body):
        seq = self.send(command, **body)
        deadline = time.time() + timeout
        while time.time() < deadline:
            if seq in self.pending:
                return self.pending.pop(seq)
            time.sleep(0.05)
        raise TimeoutError(command)

    def wait_event(self, name, timeout=60):
        deadline = time.time() + timeout
        while time.time() < deadline:
            for event in self.events:
                if event["event"] == name:
                    return event
            time.sleep(0.05)
        raise TimeoutError(name)

    def has_event(self, name):
        return any(e["event"] == name for e in self.events)


def logoff_line():
    with open(CLIENT_AVATAR, encoding="utf-8") as handle:
        for number, text in enumerate(handle, start=1):
            if text.strip().startswith("def logOff("):
                return number
    raise AssertionError("no logOff in the reference base script")


def main():
    line = logoff_line()
    report = []

    def step(name, ok, detail=""):
        report.append(ok)
        print(f"{'PASS' if ok else 'FAIL'}  {name}  {detail}".rstrip())

    workdir = tempfile.mkdtemp(prefix="bwpyattach-")
    shutil.copy(CLIENT_AVATAR, os.path.join(workdir, "ClientAvatar.py"))
    with open(os.path.join(workdir, "BigWorld.py"), "w", encoding="utf-8") as fh:
        fh.write(
            "# Stub of the server binding: the entity script needs Proxy.\n"
            "class Proxy:\n"
            "    def __init__(self, id=1):\n"
            "        self.id = id\n"
            "        self.databaseID = id\n"
            "    def destroy(self):\n"
            "        print('BigWorld.Proxy.destroy (stub)', flush=True)\n"
            "    def destroyCellEntity(self):\n"
            "        pass\n"
        )
    with open(os.path.join(workdir, "run.py"), "w", encoding="utf-8") as fh:
        fh.write(
            "import os\n"
            "print('host up, pid', os.getpid(), flush=True)\n"
            "import ClientAvatar\n"
            "entity = ClientAvatar.ClientAvatar()\n"
            "entity.logOff()\n"
            "print('callback returned', flush=True)\n"
        )

    log_path = os.path.join(workdir, "target.out")
    child = subprocess.Popen(
        [
            sys.executable, "-Xfrozen_modules=off", "-m", "debugpy",
            "--listen", f"127.0.0.1:{PORT}", "--wait-for-client", "run.py",
        ],
        cwd=workdir,
        stdout=open(log_path, "w"),
        stderr=subprocess.STDOUT,
        preexec_fn=os.setsid,
    )
    pgid = None
    try:
        step("the reference base script carries the callback", line == 36,
             f"logOff at line {line}")

        dap = None
        for _ in range(240):
            try:
                dap = Dap("127.0.0.1", PORT)
                break
            except OSError:
                time.sleep(0.1)
        if dap is None:
            print(open(log_path, encoding="utf-8").read())
            raise TimeoutError(f"debugpy never bound 127.0.0.1:{PORT}")

        dap.call("initialize", adapterID="debugpy", clientID="boundless-vscode",
                 clientName="boundless-vscode", pathFormat="path",
                 linesStartAt1=True, columnsStartAt1=True,
                 supportsVariableType=True)
        step("the debugger attached to the running script host", True)

        # A `debugpy --listen` target is a server session: the client attaches
        # to it. The attach response only arrives after configurationDone, so
        # it is sent without waiting.
        dap.send("attach", connect={"host": "127.0.0.1", "port": PORT},
                 justMyCode=False)
        dap.wait_event("initialized", timeout=30)

        verified = dap.call("setBreakpoints",
                            source={"path": os.path.join(workdir, "ClientAvatar.py")},
                            breakpoints=[{"line": line}])
        got = verified.get("body", {}).get("breakpoints", [])
        step("the breakpoint was verified at the callback line",
             bool(got) and got[0].get("verified") is True,
             f"line {got[0].get('line') if got else None}")

        dap.call("configurationDone")

        stopped = dap.wait_event("stopped")
        thread_id = stopped["body"].get("threadId")
        top = dap.call("stackTrace", threadId=thread_id, startFrame=0,
                       levels=1)["body"]["stackFrames"][0]
        hit = (os.path.basename(top["source"]["path"]), top["line"])
        step("the breakpoint hit inside the BaseApp entity callback",
             stopped["body"].get("reason") == "breakpoint"
             and hit[0] == "ClientAvatar.py" and hit[1] == line,
             f"reason={stopped['body'].get('reason')} frame {hit[0]}:{hit[1]}")

        dap.call("continue", threadId=thread_id)

        deadline = time.time() + 30
        text = ""
        while time.time() < deadline:
            text = open(log_path, encoding="utf-8").read()
            if "callback returned" in text:
                break
            time.sleep(0.2)
        step("the callback ran to completion after resume", "callback returned" in text)
        print("\n--- script host output ---")
        print(text.strip())
    finally:
        if child.poll() is None:
            try:
                pgid = os.getpgid(child.pid)
                os.killpg(pgid, signal.SIGTERM)
            except Exception:
                child.terminate()
        try:
            child.wait(timeout=15)
        except Exception:
            child.kill()
        step("left no script host behind", child.poll() is not None,
             f"exit={child.returncode}")

    leftover = ""
    try:
        if pgid:
            leftover = subprocess.run(["ps", "-o", "pid=", "-g", str(pgid)],
                                      capture_output=True, text=True).stdout.strip()
    except FileNotFoundError:
        pass
    step("the script host's process group is gone", leftover == "", leftover)

    failed = report.count(False)
    print(f"\n{len(report) - failed}/{len(report)} checks passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
