#!/usr/bin/env python3
"""BW-00-04-06 — the difference-report engine.

Compares two evidences of the same input and reports every difference as
`(kind, path, ref, new)` where the path localizes the finding and the kind
says what happened:

  injected  — a field present in the new evidence and absent from the
              reference (the path names the field that appeared);
  omitted   — a field present in the reference and absent from the new
              (the mirror case; reported so nothing silently vanishes);
  timeout   — one side produced no evidence at all (the side is named —
              a run that never finished is not a value mismatch);
  order     — the same elements in a different sequence (the path names
              the array and the report carries both sequences);
  value     — same field, different content.

IGNORABLE — the callback fixture marks runtime-varying leaves with the
`<float>` placeholder and the `-IGNORABLE` event suffix (BW-00-04-02). The
report keeps those leaves out of the comparison: a `<float>` on either side
matches anything, and inside a `-IGNORABLE` event every numeric token is
folded to the placeholder first, so a concrete runtime value compares equal
to the sample's placeholder and to any other runtime value.

Exit code: 0 when the evidences agree, 1 when there are findings, 2 when a
side is missing (timeout) — the caller records which.
"""

import json
import re
import sys

TIMEOUT = "timeout"
INJECTED = "injected"
OMITTED = "omitted"
ORDER = "order"
VALUE = "value"

IGNORABLE_SUFFIX = "-IGNORABLE"
PLACEHOLDER = "<float>"
NUMERIC = re.compile(r"^-?\d+(\.\d+)?$")

# A scalar containing the placeholder matches anything: the sample pinned the
# shape, not the runtime value.
def scalar_equal(ref, new):
    if ref == new:
        return True
    if isinstance(ref, str) and isinstance(new, str):
        return _tokens_equal(ref.split(), new.split())
    return PLACEHOLDER in str(ref) or PLACEHOLDER in str(new)

def _tokens_equal(ref, new):
    if len(ref) != len(new):
        return False
    return all(
        PLACEHOLDER in x or PLACEHOLDER in y or x == y
        for x, y in zip(ref, new)
    )

# Inside a -IGNORABLE event every numeric token is runtime, so it folds to
# the placeholder before comparing; elsewhere it is content and compares.
def fold_ignorable(event_name, value):
    if not event_name.endswith(IGNORABLE_SUFFIX):
        return value
    if isinstance(value, list):
        return [fold_ignorable(event_name, item) for item in value]
    if isinstance(value, str):
        return " ".join(
            PLACEHOLDER if (NUMERIC.match(t) or PLACEHOLDER in t) else t
            for t in value.split()
        )
    return value

def diff(ref, new, path="$"):
    """Yield findings between two evidence trees, localized by path."""
    if isinstance(ref, dict) and isinstance(new, dict):
        for key in sorted(ref.keys() | new.keys()):
            here = f"{path}.{key}"
            if key not in new:
                yield OMITTED, here, _show(ref[key]), None
            elif key not in ref:
                yield INJECTED, here, None, _show(new[key])
            else:
                yield from diff(ref[key], new[key], here)
    elif isinstance(ref, list) and isinstance(new, list):
        if _same_multiset(ref, new):
            if ref != new:
                yield ORDER, path, _show(ref), _show(new)
            return
        if len(ref) != len(new):
            yield VALUE, f"{path} (length {len(ref)} vs {len(new)})", \
                _show(ref), _show(new)
            return
        for i, (x, y) in enumerate(zip(ref, new)):
            yield from diff(x, y, f"{path}[{i}]")
    elif isinstance(ref, (dict, list)) or isinstance(new, (dict, list)):
        yield VALUE, path, _show(ref), _show(new)
    elif not scalar_equal(ref, new):
        yield VALUE, path, _show(ref), _show(new)

def _same_multiset(ref, new):
    if len(ref) != len(new):
        return False
    return sorted(ref, key=repr) == sorted(new, key=repr)

def _show(value):
    if value is None:
        return "-"
    if isinstance(value, (dict, list)):
        return json.dumps(value, sort_keys=True)
    return str(value)

# --- the callback-trace format (BW-00-04-02) ---------------------------------

def parse_trace(text):
    """callback-trace.txt -> {"header": [...], "events": [event, ...]}.

    Each `event <name>` line starts a block; its indented `key value` lines
    are fields; order/args/results split on spaces (they are sequences, and
    order is the kind of thing the report must localize).
    """
    header, events, current = [], [], None
    for line in text.splitlines():
        if line.startswith("event "):
            current = {"name": line[len("event "):].strip()}
            events.append(current)
        elif line.startswith("  ") and current is not None:
            key, _, value = line.strip().partition(" ")
            current[key] = value.split() if key in ("order", "args", "results") \
                else value
        elif line.strip() and current is None:
            header.append(line.strip())
    return {"header": header, "events": events}

def diff_trace(ref_text, new_text):
    """Diff two callback traces. Events match by name (not position), so an
    injected event is named, not offset-shifted; the -IGNORABLE suffix scopes
    which event's leaves are runtime."""
    ref, new = parse_trace(ref_text), parse_trace(new_text)
    if not ref["events"] and not new["events"]:
        yield TIMEOUT, "both traces empty", "-", "-"
        return
    if not ref["events"]:
        yield TIMEOUT, "reference trace empty", "-", _event_names(new)
        return
    if not new["events"]:
        yield TIMEOUT, "new trace empty", _event_names(ref), "-"
        return
    if ref["events"] != new["events"] and \
            [e["name"] for e in ref["events"]] != [e["name"] for e in new["events"]]:
        ref_names, new_names = [e["name"] for e in ref["events"]], \
            [e["name"] for e in new["events"]]
        if sorted(ref_names) == sorted(new_names):
            yield ORDER, "$.events", ref_names, new_names
        else:
            yield from _event_field_diff(ref, new)
        return
    yield from _event_field_diff(ref, new)

def _event_names(events):
    return [e["name"] for e in events]

def _event_field_diff(ref, new):
    by_name = {e["name"]: e for e in new["events"]}
    seen = set()
    for event in ref["events"]:
        name = event["name"]
        seen.add(name)
        counterpart = by_name.get(name)
        if counterpart is None:
            yield OMITTED, f"$.events[{name}]", name, None
            continue
        for key in sorted(event.keys() | counterpart.keys()):
            if key == "name":
                continue
            ref_value = fold_ignorable(name, event.get(key))
            new_value = fold_ignorable(name, counterpart.get(key))
            if key not in counterpart:
                yield OMITTED, f"$.events[{name}].{key}", _show(ref_value), None
            elif key not in event:
                yield INJECTED, f"$.events[{name}].{key}", None, _show(new_value)
            elif not scalar_equal(ref_value, new_value):
                for kind, path, r, n in diff(ref_value, new_value,
                                             f"$.events[{name}].{key}"):
                    yield kind, path, r, n
    # An event present only in the new trace is injected, not offset-shifted —
    # events match by name, so a new subscriber is named, not miscounted.
    for event in new["events"]:
        if event["name"] not in seen:
            yield INJECTED, f"$.events[{event['name']}]", None, event["name"]

# --- drivers -----------------------------------------------------------------

def diff_json_files(ref_path, new_path):
    """Diff two JSON evidence files. A missing or empty side is a timeout."""
    import os
    ref_text = _read(ref_path)
    new_text = _read(new_path)
    if ref_text is None and new_text is None:
        return [(TIMEOUT, "both evidences missing", ref_path, new_path)]
    if ref_text is None:
        return [(TIMEOUT, "reference evidence missing", ref_path, _first_line(new_text))]
    if new_text is None:
        return [(TIMEOUT, "new evidence missing", _first_line(ref_text), new_path)]
    return list(diff(json.loads(ref_text), json.loads(new_text)))

def diff_trace_files(ref_path, new_path):
    ref_text = _read(ref_path)
    new_text = _read(new_path)
    if ref_text is None and new_text is None:
        return [(TIMEOUT, "both traces missing", ref_path, new_path)]
    if ref_text is None:
        return [(TIMEOUT, "reference trace missing", ref_path, _first_line(new_text))]
    if new_text is None:
        return [(TIMEOUT, "new trace missing", _first_line(ref_text), new_path)]
    return list(diff_trace(ref_text, new_text))

def _read(path):
    try:
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        return text if text.strip() else None
    except OSError:
        return None

def _first_line(text):
    return text.splitlines()[0] if text else "-"

def main():
    usage = "usage: diff_report.py json <ref.json> <new.json> | trace <ref> <new>"
    if len(sys.argv) != 4:
        print(usage, file=sys.stderr)
        return 2
    mode, ref_path, new_path = sys.argv[1], sys.argv[2], sys.argv[3]
    if mode == "json":
        findings = diff_json_files(ref_path, new_path)
    elif mode == "trace":
        findings = diff_trace_files(ref_path, new_path)
    else:
        print(usage, file=sys.stderr)
        return 2
    for kind, path, ref, new in findings:
        print(f"{kind}\t{path}\t{_show(ref)}\t{_show(new)}")
    kinds = {kind for kind, *_ in findings}
    if TIMEOUT in kinds:
        return 2
    return 1 if findings else 0

if __name__ == "__main__":
    sys.exit(main())
