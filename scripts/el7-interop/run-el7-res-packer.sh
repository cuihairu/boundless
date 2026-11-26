#!/bin/sh
# BW-15-02-05: pack the fixture server/bw.xml with the reference EL7
# res_packer binary. The output is committed as
# docs/review/fixtures/el7_res_packer/bw.packed and parsed against its XML
# source by the `an_el7_res_packer_file_reads_back_identical_to_its_xml_source`
# test in crates/resources/src/packed.rs.
set -e
REPO=$(cd "$(dirname "$0")/../.." && pwd)
REF=${BIGWORLD_REF:-/home/cui/workspaces/BigWorld}
TOOL="$REF/game/bin/server/el7/tools/res_packer"
SRC="$REPO/docs/review/fixtures/fantasydemo_def/res/server/bw.xml"
OUT=${1:-"$REPO/docs/review/fixtures/el7_res_packer/bw.packed"}
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
cp "$SRC" "$TMP/bw.xml"
(cd "$TMP" && "$TOOL" -r . bw.xml bw.packed)
cp "$TMP/bw.packed" "$OUT"
echo "packed written: $OUT"
