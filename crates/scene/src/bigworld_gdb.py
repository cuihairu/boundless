"""BW-13-06-04: the GDB face of the migrated debug field view.

The reference `tools/visual_studio/visualizers/bigworld.natvis` visualizes
`BW::SceneObject` as ``Object: <handle> of type: <type> (<name>)`` with a
Type/TypeName/Handle expansion. This script gives the same view to
``scene::SceneObject`` (crates/scene/src/scene_object.rs) under GDB and
GDB-based IDEs; the parenthesised type name needs the global name table
(``type_system::fetch_type_name``) and is reachable from
``SceneObject::debug_view_string`` instead.

Loaded through ``#![debugger_visualizer(gdb_script_file)]`` on the scene
crate. Every step is defensive: a formatter must never break the debug
session it decorates.
"""

import gdb
import gdb.printing


class SceneObjectView:
    """Print ``scene::SceneObject`` like the bigworld.natvis view."""

    def __init__(self, value):
        self._value = value

    def to_string(self):
        try:
            handle = self._value["handle"]
            type_id = self._value["type_id"]
            return "Object: {} of type: {}".format(handle, type_id)
        except Exception:
            return "SceneObject"

    def children(self):
        # The reference <Expand> items: Type, then Handle.
        try:
            return [
                ("Type", self._value["type_id"]),
                ("Handle", self._value["handle"]),
            ]
        except Exception:
            return []

    @staticmethod
    def display_hint():
        return "array"


def _build_printer():
    printer = gdb.printing.RegexpCollectionPrettyPrinter("boundless_scene")
    printer.add_printer("SceneObject", r"^scene::SceneObject$", SceneObjectView)
    return printer


try:
    objfile = gdb.current_objfile()
    if objfile is None:
        objfile = gdb
    gdb.printing.register_pretty_printer(objfile, _build_printer(), replace=True)
except Exception:
    # An unsuitable host (a non-GDB consumer of this script, a stripped
    # type table) must not fail the load.
    pass
