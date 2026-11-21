//! BW-11 client application: window creation, presentation-mode switching and
//! the device lifecycle that must not leak across mode changes.
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
//! surface, never re-creates the device. `window_mode` carries the mode model
//! and the transition policy; `app` binds them to a real window through winit
//! (feature `winit-backend`).

pub mod device;
pub mod window_mode;

#[cfg(feature = "winit-backend")]
pub mod app;
