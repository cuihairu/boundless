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

pub mod camera;
pub mod device;
pub mod focus;
pub mod frame;
pub mod local_entity;
pub mod player;
pub mod render;
pub mod settings;
pub mod window_mode;

#[cfg(feature = "winit-backend")]
pub mod app;
