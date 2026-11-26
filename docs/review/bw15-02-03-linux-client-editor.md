# BW-15-02-03 Linux leg: client display + editor edit of the fixed map

Date: 2026-11-26. Base: `8603191` (`lane/bw10-terrain`, after absorbing main's corrected blocker note).

## What was missing

The corrected blocker note on main (`fb34794`) records the real gap: `apps/`
carried only the bots and server entries — no client/editor executable — and
the `crates/client` test scene was a 16-cell colour-band animation rather than
the fixed map. This leg adds both entries and drives the existing fixed-map
fixture (`docs/review/fixtures/chunk_space/res/space_00010001`, 5 chunk files
plus the settings grid) through the real client and editor faces.

## Deliverables (code)

- `crates/client/src/render/mod.rs`, `scene.rs`, `software.rs`,
  `device.rs`: a small `Scene` trait; `TestScene` implements it through its
  existing inherent `draw`, and `SoftwareDevice::present` / both
  `Device::present` methods take `&impl Scene`. No existing caller changes.
- `crates/client/src/render/map_scene.rs`: `MapScene::from_space` projects
  every loaded chunk (grid cell with deterministic identifier fill) and every
  chunk item (world-space marker) into the framebuffer, and paints the loaded
  area's outline. Three tests pin the loaded-chunk/item counts, neighbour
  shading, deterministic double draw, and two device presents on the fixture.
- `apps/client` (`boundless-client`): the display entry. Loads the space via
  `GeometryMapping::from_settings` + `gather_chunks` + `load_and_bind`, renders
  N static frames through `SoftwareDevice`, writes a TGA via `ScreenCopy`, and
  prints `chunks_loaded`, `items`, `bounds`, `buffer`, `frames`,
  `frame_digest`, `shot_bytes`, `shot_digest`.
- `apps/editor` (`boundless-editor`): the edit entry. Loads the same space into
  `EditorWorld` through `SectionItemFactory`, keeps each chunk's root
  `DataSection`, moves the target item through `ChunkItemMatrix::set_matrix` +
  `commit_state` (undo op + `GIZMO/UNDO/MOVE_PROPERTY` barrier), walks
  undo/redo, writes the moved pose into the chunk section, saves via
  `UnsavedChunks` with an `EditorChunkCache::ed_save`-backed saver into a
  `MultiFileSystem`/`NativeFileSystem`, then reloads the published space and
  verifies the stored pose. Before/after TGA captures ride the same map scene.

## Linux leg run (commands from the repository root)

```
cargo build -p boundless-client -p boundless-editor
./target/debug/boundless-client \
  --res docs/review/fixtures/chunk_space/res/space_00010001 \
  --frames 8 --shot /tmp/bw15-leg/client-view.tga
./target/debug/boundless-editor \
  --res docs/review/fixtures/chunk_space/res/space_00010001 \
  --out /tmp/bw15-leg/out \
  --shot-before /tmp/bw15-leg/editor-before.tga \
  --shot-after /tmp/bw15-leg/editor-after.tga
```

Client stdout:

```
chunks_loaded	6
items	15
bounds	0.000,0.000 .. 1800.000,350.000
buffer	320x240
frames	8
frame_digest	34f7ec55e76b1325
shot_bytes	307218
shot_digest	021052b810a1f69d
```

A second run of the client produced the same `frame_digest` and
`shot_digest` (static-map determinism). The TGA is a 320×240 type-2 uncompressed
image; artifacts stay in `/tmp` and are not committed.

Editor stdout:

```
resource	tree00.model
chunk	00000000o
items_loaded	15
move_from	12.000,0.000,34.000
move_to	42.000,0.000,64.000
undo_pose	12.000,0.000,34.000
redo_pose	42.000,0.000,64.000
undo_barrier	GIZMO/UNDO/MOVE_PROPERTY model tree00.model
changed_chunks	00000000o
saved_chunks	00000000o
reload_chunks	1
reload_bound	1
reload_pose	42.000,0.000,64.000
shot_before_frame_digest	34f7ec55e76b1325
shot_before_digest	021052b810a1f69d
shot_after_frame_digest	6de0dafdc5095325
shot_after_digest	0245d59dd2a73dcf
```

The written `/tmp/bw15-leg/out/space_00010001/00000000o.chunk` carries
`<row3>42.000000 0.000000 64.000000</row3>` in the first model's `<transform>`;
the sibling items are unchanged. Reloading that published space binds
`00000000o` and reports row3 at `(42, 0, 64)`. The map scene's frame digest
changes from `34f7ec55e76b1325` to `6de0dafdc5095325`, so the edit is visible
in the display face as well as in the persisted chunk XML.

## Scope / row status

This is the **Linux leg only**. The todo row's full acceptance names three
platforms; the row stays open pending the Windows/macOS legs. The fixed map is
the existing chunk-space fixture; the editor move uses the real editor faces
rather than a synthetic XML patch. No release tag or release workflow is
touched.
