#!/bin/sh
# BW-15-02-05: run the reference EL7 process_defs binary against the in-repo
# fantasydemo defs fixture, closing the "complete defs dump" gap in the EL7
# baseline record (docs/review/bw15-02-05-el7-baseline.md section 3 item 3).
#
# The reference binary imports its personality module (default name
# ProcessDefs, callback ProcessDefs.process) from scripts/common on the res
# path; main.cpp builds the description dict {entityTypes, constants} and
# hands it to the callback. The fixture root carries the dumper, the bigworld
# res root carries bw_site.py, so one BW_RES_PATH serves both.
set -e
REPO=$(cd "$(dirname "$0")/../.." && pwd)
REF=${BIGWORLD_REF:-/home/cui/workspaces/BigWorld}
TOOL="$REF/game/bin/server/el7/tools/process_defs"
FIXTURE="$REPO/docs/review/fixtures/fantasydemo_def/res"
OUT=${1:-"$TMPDIR/process_defs_dump.json"}
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/res/scripts/common"
cp -r "$FIXTURE/." "$TMP/res/"
cat > "$TMP/res/scripts/common/ProcessDefs.py" <<'PY'
"""ProcessDefs personality: dump the entity description map to JSON.

The reference process_defs binary calls ProcessDefs.process(description)
with {"entityTypes": [...], "constants": {"digest": ..., "max...": ...}}.
Everything arrives as plain dict/list/str/int, so json handles it.
"""
import json
import os


def process(description):
    path = os.environ.get("PROCESS_DEFS_DUMP", "process_defs_dump.json")
    with open(path, "w") as handle:
        json.dump(description, handle, indent=1, default=str)
    constants = description["constants"]
    print("digest:", constants["digest"])
    for entity in description["entityTypes"]:
        faces = "".join(
            tag
            for tag, flag in (("C", entity["canBeOnClient"]),
                              ("B", entity["canBeOnBase"]),
                              ("S", entity["canBeOnCell"]))
            if flag)
        print("entity: %2d %-12s %s" % (entity["index"], entity["name"],
                                        faces))
    return True
PY

BW_RES_PATH="$TMP/res:$REF/game/res/bigworld" \
PROCESS_DEFS_DUMP="$OUT" "$TOOL" -m ProcessDefs
echo "dump written: $OUT"
