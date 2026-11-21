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

__version__ = "0.1.0"

__all__ = ["__version__"]
