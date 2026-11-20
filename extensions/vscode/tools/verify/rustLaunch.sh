#!/bin/sh
# BW-20-02-06 evidence: the Rust launch configuration really starts CellApp
# under a debugger and hits a breakpoint inside the role entry point.
#
# The launch configuration VS Code uses points at target/debug/boundless-server
# with the role as argv[1]; this harness runs the same command line under gdb
# batch mode, which is what the lldb/cppvsdbg adapters drive.
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
BIN="$REPO/target/debug/boundless-server"
REFERENCE="${BW_REFERENCE:-/home/cui/workspaces/BigWorld}"
RES="${BW_EXAMPLE_RES:-$REFERENCE/programming/bigworld/examples/client_integration/python/simple/res}"

if [ ! -x "$BIN" ]; then
  echo "building boundless-server..."
  (cd "$REPO" && cargo build -p boundless-server) || exit 1
fi

RES="$(cd "$RES" 2>/dev/null && pwd)" || RES=""
echo "binary: $BIN"
echo "BW_RES_PATH: $RES"

OUT="$(mktemp -d)"
BW_RES_PATH="$RES" gdb -q -batch \
  -ex "break server_runtime::app::run" \
  -ex run \
  -ex "bt 3" \
  --args "$BIN" cellapp > "$OUT/gdb.log" 2>&1
STATUS=$?

cat "$OUT/gdb.log"
echo "---"

HIT=$(grep -c "Breakpoint 1, server_runtime::app::run" "$OUT/gdb.log")
FRAME=$(grep -c "boundless_server::main" "$OUT/gdb.log")
rm -rf "$OUT"

if [ "$HIT" -ge 1 ] && [ "$FRAME" -ge 1 ]; then
  echo "PASS: breakpoint hit in the role entry point, called from main"
else
  echo "FAIL: breakpoint did not hit as expected"
  exit 1
fi
