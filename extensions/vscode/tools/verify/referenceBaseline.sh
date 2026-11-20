#!/bin/sh
# The reference half of the EntityDef diagnostic evidence (BW-20-01-05):
# install one .def into a probe res tree and run the original `process_defs`
# against it, printing only the entity-definition messages.
#
# usage: referenceBaseline.sh            # use the checked-in fixtures
#        referenceBaseline.sh /path/Foo.def
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REFERENCE="${BW_REFERENCE:-/home/cui/workspaces/BigWorld}"
PROBE="${BW_PROBE:-/tmp/bwprobe}/res"
BIN="$REFERENCE/game/bin/server/el7/tools/process_defs"
DEF_DIR="$PROBE/scripts/entity_defs"

install_one() {
  mkdir -p "$DEF_DIR"
  cp "$REFERENCE/programming/bigworld/examples/client_integration/python/simple/res/scripts/entity_defs/alias.xml" \
    "$DEF_DIR/alias.xml" 2>/dev/null || true
  rm -f "$DEF_DIR"/*.def
  cp "$1" "$DEF_DIR/Fixture.def"
  cat > "$PROBE/scripts/entities.xml" <<'XML'
<root>
	<ClientServerEntities>
		<Fixture/>
	</ClientServerEntities>
	<ServerOnlyEntities/>
</root>
XML
  # The fixture needs a client script so canBeOnClient() is true.
  mkdir -p "$PROBE/scripts/client" "$PROBE/scripts/base" "$PROBE/scripts/cell"
  echo "# fixture client script" > "$PROBE/scripts/client/Fixture.py"
  echo "# fixture base script" > "$PROBE/scripts/base/Fixture.py"
  echo "# fixture cell script" > "$PROBE/scripts/cell/Fixture.py"
}

install_one "$1"

export BW_RES_PATH="$PROBE:$REFERENCE/game/res/bigworld"
cd /tmp
timeout 60 "$BIN" -v -p "$PROBE/scripts" 2>&1 \
  | grep -E "\[(DataDescription|EntityDescriptionMap|DataType|VolatileInfo|MethodDescription|SequenceMetaDataType|UserMetaDataType|DataLoDLevels)\]|Error in|error in|Unable to|Invalid|Failed to parse" \
  | head -40
