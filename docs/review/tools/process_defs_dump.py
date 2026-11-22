"""Reference-side capture harness for the BigWorld `process_defs` tool.

Run with the *stock* reference binary, e.g.

    process_defs -m process_defs_dump -f dump \
        -p <dir containing this file> -r <res roots...> --use-stdout

`process_defs` builds a Python dict of every parsed entity description plus
`constants.digest`; this module turns that dict into deterministic JSON so a
Rust re-implementation can be compared field for field against the reference
(BW-04-01-01/03/04/05, BW-04-04-03).

The destination file comes from `sys.argv[1]`; the tool passes its remaining
arguments straight through to the callback's caller, so
`process_defs ... --use-stdout /path/out.json` writes there.
"""

import json
import os
import sys


def _convert(value):
    """Map a BigWorld script object onto JSON-shaped Python data."""
    if value is None:
        return None
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float, str)):
        return value
    if isinstance(value, (tuple, list)):
        return [_convert(item) for item in value]
    if isinstance(value, dict):
        return dict((key, _convert(item)) for key, item in value.items())
    return repr(value)


def dump(description, path=None):
    """Callback invoked by `process_defs -f dump`."""
    if path is None:
        path = sys.argv[1]
    payload = _convert(description)
    if not os.path.isdir(os.path.dirname(path) or "."):
        os.makedirs(os.path.dirname(path))
    with open(path, "w") as handle:
        json.dump(payload, handle, sort_keys=True, indent=1, default=repr)
        handle.write("\n")
    return True


def help():
    """`-h` branch of `process_defs`: the reference prints this instead."""
    print("process_defs_dump.help")
    return True