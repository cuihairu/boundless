"""The Boundless engine Python module.

The build entry for the Python side of the tree (BW-00-03-03): installing
this package puts a ``boundless`` module in front of an interpreter that
has no engine attached, which is the same top-level name the embedded
native types carry (``boundless.Sequence``, ``boundless.wrapper`` in
``crates/python-host``). The engine-facing script surface -- the modules
the reference exposes to game scripts -- is enumerated in the export
manifest (``crates/python-host/src/manifest.rs``) and lands here module by
module as its porting item delivers it.
"""

"""Boundless Python module.

The installable surface the engine's Python half shares. The embedded
interpreter builds the ``BigWorld`` module at init (``crates/python-host``);
this package carries the constants tooling and tests can depend on without a
running server, and is the module the repo's Python build entry installs.

Reference contract: the role names mirror ``BigWorld.component`` as the
reference sets it in ``Script::init`` (``lib/pyscript/script.cpp``).
"""

__version__ = "0.1.0"

# The engine role names: cell, base, client, database, bot, editor.
COMPONENTS = ("cell", "base", "client", "database", "bot", "editor")

__all__ = ["__version__", "COMPONENTS"]
