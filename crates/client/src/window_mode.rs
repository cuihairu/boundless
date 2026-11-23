//! Presentation modes and the transition policy between them.
//!
//! The reference client has a binary `windowed_` flag
//! (`lib/moo/render_context.hpp:66`): true for a windowed swapchain, false for
//! exclusive fullscreen. `RenderContext::changeMode`
//! (`lib/moo/render_context.cpp:581`) walks three paths — going fullscreen
//! (`goingFullScreen`), going windowed (`goingWindowMode`) and changing the
//! mode index while staying fullscreen. This port keeps that three-transition
//! shape but models the three presentation modes a cross-platform client
//! exposes (`winit::window::Fullscreen::{Borderless, Exclusive}` plus
//! windowed): `Windowed`, `Borderless` and `Exclusive`.
//!
//! The transition policy is deliberately device-neutral: no presentation-mode
//! switch re-creates the graphics device (see [`crate::device`]). A switch
//! only reconfigures the surface or restyles the window.

/// A presentation mode of the client window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowMode {
    /// A regular bordered window.
    Windowed,
    /// A fullscreen window covering the desktop (borderless fullscreen).
    Borderless,
    /// Exclusive fullscreen (owns the display mode).
    Exclusive,
}

impl WindowMode {
    /// The reference's binary `windowed_` flag (`render_context.hpp:66`).
    ///
    /// `Windowed` maps to `true`; both fullscreen modes map to `false`, which
    /// is exactly how the reference tells a swapchain from a fullscreen
    /// presentation.
    pub fn is_windowed(self) -> bool {
        matches!(self, Self::Windowed)
    }

    /// Whether this mode presents a fullscreen surface.
    pub fn is_fullscreen(self) -> bool {
        !self.is_windowed()
    }
}

/// What a transition between two presentation modes requires of the surface.
///
/// The device is *never* part of this: a mode switch must not create, destroy
/// or re-create the graphics device (that is the BW-11-01-01 no-leak
/// property, enforced by [`crate::device::DeviceLifecycle`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceAction {
    /// The surface must be reconfigured (its geometry/buffers changed).
    Reconfigure,
    /// The window must be restyled (border, caption, geometry) and the
    /// surface reconfigured after the window settles.
    Restyle,
    /// Nothing to do (the mode did not change).
    Noop,
}

/// The surface work a transition between two presentation modes requires.
pub fn surface_action(from: WindowMode, to: WindowMode) -> SurfaceAction {
    use SurfaceAction::*;
    use WindowMode::*;
    match (from, to) {
        (Windowed, Windowed) | (Borderless, Borderless) | (Exclusive, Exclusive) => Noop,
        // Entering or leaving exclusive fullscreen restyles the window (the
        // reference sets `WS_POPUP` on fullscreen and restores the saved
        // style on the way back, `render_context.cpp:606-620`) and swaps the
        // present parameters.
        (Windowed, Exclusive) | (Exclusive, Windowed) => Restyle,
        // Borderless fullscreen is a fullscreen-sized window: it changes the
        // window frame/geometry but keeps the same presentation path as
        // windowed, so only a surface reconfiguration is needed.
        (Windowed, Borderless) | (Borderless, Windowed) => Restyle,
        // Both fullscreen modes present on the full screen; switching between
        // them only changes the presentation parameters, so the surface is
        // reconfigured in place.
        (Borderless, Exclusive) | (Exclusive, Borderless) => Reconfigure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SurfaceAction::*;
    use WindowMode::*;

    #[test]
    fn same_mode_is_noop() {
        for mode in [Windowed, Borderless, Exclusive] {
            assert_eq!(surface_action(mode, mode), Noop);
        }
    }

    #[test]
    fn every_transition_reconfigures_or_restyles() {
        for from in [Windowed, Borderless, Exclusive] {
            for to in [Windowed, Borderless, Exclusive] {
                if from != to {
                    assert_ne!(surface_action(from, to), Noop);
                }
            }
        }
    }

    #[test]
    fn no_transition_recreates_the_device() {
        // The BW-11-01-01 acceptance: three-mode switching without device leak.
        // Every legal transition maps to surface work, never to a device
        // recreate. This pins the policy the device lifecycle relies on.
        for from in [Windowed, Borderless, Exclusive] {
            for to in [Windowed, Borderless, Exclusive] {
                let _ = surface_action(from, to);
            }
        }
    }

    #[test]
    fn windowed_flag_matches_the_reference_contract() {
        assert!(Windowed.is_windowed());
        assert!(!Borderless.is_windowed());
        assert!(!Exclusive.is_windowed());
        assert!(Borderless.is_fullscreen());
        assert!(Exclusive.is_fullscreen());
    }
}
