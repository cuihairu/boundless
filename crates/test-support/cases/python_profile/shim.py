# Boundless Python call capture shim (BW-23-01-02).
#
# The pure-Python analog of the reference's `_bw_profile` module.  It installs
# a `sys.setprofile` hook that accounts every Python function call in monotonic
# nanoseconds (the same clock the Rust profiler records), then writes
# per-function rows -- `name\tfile\tcalls\ttotal_nanos`, one per line -- to the
# output path when `_bwp_stop` runs.  The hook is installed only after the shim
# itself has loaded, so the shim's own imports are never attributed to the
# captured script.
#
# `_OUTPUT` carries the path the capture driver substitutes in place of the
# `__OUTPUT__` placeholder below.

import sys
import time

_OUTPUT = "__OUTPUT__"

# code -> [calls, total_nanos]
_STAT = {}
# The open 'call' events, one (code, start_ns) per nesting level.
_OPEN = []


def _bwp_event(frame, event, arg):
    if event == "call":
        code = frame.f_code
        _OPEN.append((code, time.perf_counter_ns()))
        entry = _STAT.get(code)
        if entry is None:
            _STAT[code] = [1, 0]
        else:
            entry[0] += 1
    elif event == "return":
        if _OPEN:
            code, start = _OPEN.pop()
            entry = _STAT.get(code)
            if entry is not None:
                entry[1] += time.perf_counter_ns() - start


def _bwp_start():
    sys.setprofile(_bwp_event)


def _bwp_stop(output=_OUTPUT):
    sys.setprofile(None)
    _OPEN.clear()
    rows = sorted(
        _STAT.items(),
        key=lambda kv: (-kv[1][1], kv[0].co_name, kv[0].co_filename),
    )
    with open(output, "w") as handle:
        for code, (calls, total) in rows:
            handle.write("%s\t%s\t%d\t%d\n" % (code.co_name, code.co_filename, calls, total))
