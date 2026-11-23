//! Window focus and input enablement (BW-11-01-04).
//!
//! The reference handles focus in `App::handleSetFocus`
//! (`client/app.cpp:2674`), driven by `WM_ACTIVATE`
//! (`client/bw_winmain.cpp:399-411`): the window is focused only when the
//! activate state is not `WA_INACTIVE` **and** it is not minimised. On a focus
//! change the reference, in order:
//!
//! 1. tells the input devices (`InputDevices::setFocus`, `app.cpp:2678`) —
//!    input is accepted only while focused;
//! 2. tells the GUI and the mouse cursor objects (`app.cpp:2681-2691`);
//! 3. when no GUI owns the mouse, `Moo::rc().restoreCursor(!state)`
//!    (`app.cpp:2692`, `render_context.cpp:2018`) — the OS cursor is **hidden
//!    while focused** (the game captures the mouse) and **shown on blur**;
//! 4. in fullscreen (device present, not windowed, desktop not locked), a
//!    focus event drops the client back to windowed at the same mode index
//!    (`app.cpp:2699-2702`, `changeMode(modeIndex, /*windowed*/true)`).
//!
//! The frame loop's `active` flag **is** this focus state: the main loop calls
//! `app.updateFrame(g_bActive)` (`bw_winmain.cpp:261`), so an unfocused frame
//! runs `inactiveTick` instead of tick/updateAnimations/draw. That connection
//! lives here: [`FocusState::frame_active`] is what `frame_phases` consumes.

/// The client's focus-driven state, as `handleSetFocus` leaves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusState {
    /// Whether the window has focus (`WM_ACTIVATE`, not inactive, not
    /// minimised).
    pub focused: bool,
    /// `InputDevices::setFocus`: input is accepted only while focused.
    pub input_enabled: bool,
    /// `restoreCursor(!state)`: the OS cursor is hidden while focused (mouse
    /// capture) and shown on blur.
    pub cursor_visible: bool,
    /// `updateFrame(g_bActive)`: focused frames run the active path
    /// (tick → updateAnimations → draw), unfocused frames run `inactiveTick`.
    pub frame_active: bool,
    /// Whether the client is fullscreen. A focus event in fullscreen drops it
    /// to windowed (`app.cpp:2699`).
    pub fullscreen: bool,
}

impl FocusState {
    /// The baseline: focused and windowed.
    pub fn focused_windowed() -> Self {
        Self::focused(true, false)
    }

    /// The baseline: focused and fullscreen.
    pub fn focused_fullscreen() -> Self {
        Self::focused(true, true)
    }

    /// A state with the given focus and fullscreen flags, with the derived
    /// effects applied.
    pub fn focused(focused: bool, fullscreen: bool) -> Self {
        Self {
            focused,
            input_enabled: focused,
            cursor_visible: !focused,
            frame_active: focused,
            fullscreen,
        }
    }

    /// Apply a focus transition (the `WM_ACTIVATE` → `handleSetFocus` path).
    ///
    /// The effects are computed against the *current* fullscreen flag — the
    /// reference checks `!windowed()` before dropping to windowed — and the
    /// drop-to-windowed step happens last, exactly as in `handleSetFocus`.
    pub fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        self.input_enabled = focused;
        self.cursor_visible = !focused;
        self.frame_active = focused;
        if self.fullscreen {
            // changeMode(modeIndex, /*windowed*/ true) — the reference leaves
            // fullscreen on any focus event.
            self.fullscreen = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{FrameLoop, FramePhase};

    #[test]
    fn blur_disables_input_shows_cursor_and_pauses_frames() {
        let mut f = FocusState::focused_windowed();
        f.set_focus(false);
        assert!(!f.focused);
        assert!(!f.input_enabled, "input devices lose focus (app.cpp:2678)");
        assert!(
            f.cursor_visible,
            "cursor shown on blur (restoreCursor(true))"
        );
        assert!(!f.frame_active, "unfocused frames run inactiveTick");
    }

    #[test]
    fn restore_returns_to_the_focused_baseline() {
        let mut f = FocusState::focused_windowed();
        f.set_focus(false);
        f.set_focus(true);
        assert_eq!(
            f,
            FocusState::focused_windowed(),
            "state returns to baseline"
        );
    }

    #[test]
    fn blur_and_restore_are_idempotent() {
        let mut f = FocusState::focused_windowed();
        f.set_focus(false);
        let blurred = f;
        f.set_focus(false);
        assert_eq!(f, blurred, "repeated blur is stable");
        f.set_focus(true);
        let focused = f;
        f.set_focus(true);
        assert_eq!(f, focused, "repeated focus is stable");
    }

    #[test]
    fn focus_event_in_fullscreen_drops_to_windowed() {
        // app.cpp:2699-2702: fullscreen + focus event → changeMode(…, true).
        let mut f = FocusState::focused_fullscreen();
        f.set_focus(false);
        assert!(!f.fullscreen, "blur leaves fullscreen");
        assert!(!f.focused);
        f.set_focus(true);
        assert!(!f.fullscreen, "stays windowed after restore");
        assert!(f.input_enabled);
        assert!(!f.cursor_visible, "cursor hidden again once focused");
    }

    #[test]
    fn windowed_focus_events_never_change_fullscreen() {
        let mut f = FocusState::focused_windowed();
        f.set_focus(false);
        assert!(!f.fullscreen);
        f.set_focus(true);
        assert!(
            !f.fullscreen,
            "windowed stays windowed through focus events"
        );
    }

    #[test]
    fn focus_state_selects_the_reference_frame_path() {
        // bw_winmain.cpp:261: updateFrame(g_bActive) — the frame's phase path
        // is the focus state.
        let mut f = FocusState::focused_windowed();
        let mut fl = FrameLoop::new();
        let active = fl.run_frame(f.frame_active, true);
        assert_eq!(
            active.phases,
            vec![
                FramePhase::Tick,
                FramePhase::UpdateAnimations,
                FramePhase::Draw
            ]
        );
        f.set_focus(false);
        let inactive = fl.run_frame(f.frame_active, true);
        assert_eq!(inactive.phases, vec![FramePhase::InactiveTick]);
        f.set_focus(true);
        let restored = fl.run_frame(f.frame_active, true);
        assert_eq!(
            restored.phases,
            vec![
                FramePhase::Tick,
                FramePhase::UpdateAnimations,
                FramePhase::Draw
            ]
        );
    }
}
