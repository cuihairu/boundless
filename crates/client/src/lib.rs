//! BW-11 client application: window creation, presentation-mode switching,
//! device lifecycle and the render device itself.
//!
//! The reference client toggles a single `windowed_` flag on the render
//! context (`lib/moo/render_context.hpp:66`) through
//! `RenderContext::changeMode` (`lib/moo/render_context.cpp:581`), whose three
//! paths — going fullscreen, going windowed, and changing the mode index while
//! staying fullscreen — are the three mode transitions this port models.
//! `App::desktopLocked` (`client/app.cpp:407`) is a Windows input-desktop
//! probe, not a presentation mode, so this port does not treat it as one.
//!
//! BW-11-01-01's acceptance (three-mode switching without device leak) is the
//! property the `device` module enforces: the graphics device is created once
//! for the app lifetime and a presentation-mode switch only reconfigures the
//! surface, never re-creates the device. BW-11-01-02 adds the device itself:
//! it is initialised once (`Device::create`), rebuilt in place on a mode
//! switch, a resize or a device loss (`Device::rebuild`, a reset of the *same*
//! object) and presents frames from a test scene (`render`), so frames keep
//! coming after a rebuild. `window_mode` carries the mode model and the
//! transition policy; `app` binds them to a real window through winit
//! (feature `winit-backend`).
//!
//! BW-11-01-05 adds the display settings and the object LOD setting: the
//! "OBJECT_LOD" graphics setting (`lib/moo/lod_settings.cpp`) selects an LOD
//! option whose factor biases the object distance (`applyLodBias`) and the
//! deterministic `calculateLod` value a model compares against its fixed
//! thresholds. `settings` carries both, so the same thresholds always produce
//! the same switch.
//!
//! BW-11-02-01/02/03 are the client entity layer: `player` carries the Player
//! entity switch and the fixed order of its script callbacks (`entity.cpp`
//! `onEnterWorld`/`onLeaveWorld`/`onChangeSpace`), `local_entity` the
//! client-only entity and `UserDataObject` construction
//! (`entity_udo_factory.cpp`), and `picker` the entity selection
//! (`entity_picker.cpp`) — a hit focuses the target, a release blurs it, and
//! the hidden grace period holds a line-of-sight-blocked target before it is
//! released. `movement_filter` carries movement filtering
//! (`lib/connection/movement_filter.hpp`, `client/dumb_filter.cpp`): the
//! filter framework (`reset`/`input`/`output`/`getLastInput`) and the dumb
//! filter, whose error-box trust logic turns a fixed input-sample trajectory
//! into the same output trajectory.
//!
//! BW-11-03-01 is the replay file's head: `replay` parses the header block
//! (`lib/connection/replay_header.cpp`) and the metadata block
//! (`lib/connection/replay_metadata.cpp`) — the version, digest, frequency,
//! timestamp, nonce, signature and tick count, and the metadata key/value
//! collection — so a known recording reads out the same fields.

pub mod camera;
pub mod device;
pub mod focus;
pub mod frame;
pub mod local_entity;
pub mod movement_filter;
pub mod picker;
pub mod player;
pub mod render;
pub mod replay;
pub mod settings;
pub mod window_mode;

#[cfg(feature = "winit-backend")]
pub mod app;
