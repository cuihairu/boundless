//! The frame phases (BW-11-01-03): update, draw and the script callbacks in
//! between, in the reference's order.
//!
//! The reference drives one frame in `App::updateFrame`
//! (`client/app.cpp:1403-1457`): it **ticks** (update + input), then — only
//! when `Moo::rc().checkDevice()` reports the device operational — it
//! **updates animations**, then **draws**. A minimised frame runs
//! `inactiveTick` instead. The update phase itself fires script callbacks:
//! `ScriptApp::tick` (`client/script_app.cpp:143`) runs the script timers
//! (`Script::tick`), the entity ticks, and `Personality.onChangeEnvironments`
//! (`script_app.cpp:205`) when the player's environment changes.
//!
//! This module is the *order model* of those phases and callbacks: it does not
//! execute scripts or render, it records the sequence so the acceptance —
//! update, draw and script-callback order match the baseline — can be checked
//! headlessly. The concrete update action (advance the scene) and draw action
//! (present) live in `app`.

/// A phase of one frame, in the reference's `App::updateFrame` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FramePhase {
    /// `MainLoopTasks::root().tick` — update and input (`app.cpp:1425`).
    Tick,
    /// `MainLoopTasks::root().updateAnimations` (`app.cpp:1443`).
    UpdateAnimations,
    /// `MainLoopTasks::root().draw` (`app.cpp:1452`).
    Draw,
    /// `MainLoopTasks::root().inactiveTick` — a minimised frame
    /// (`app.cpp:1460`).
    InactiveTick,
}

impl FramePhase {
    /// The phase's position in an active frame, for order assertions.
    /// `InactiveTick` is not part of an active frame.
    pub fn active_index(self) -> Option<usize> {
        match self {
            Self::Tick => Some(0),
            Self::UpdateAnimations => Some(1),
            Self::Draw => Some(2),
            Self::InactiveTick => None,
        }
    }
}

/// The phases run in one frame, per `App::updateFrame`
/// (`client/app.cpp:1403-1457`).
///
/// * inactive (minimised): only `InactiveTick` — which still runs the script
///   tick (`ScriptApp::inactiveTick` → `tick`, `script_app.cpp:218`).
/// * active with a lost device: only `Tick` — the reference ticks and updates
///   even when the device is lost, but skips the draw (`app.cpp:1439`).
/// * active with an operational device: `Tick`, `UpdateAnimations`, `Draw`.
pub fn frame_phases(active: bool, device_ok: bool) -> Vec<FramePhase> {
    if !active {
        return vec![FramePhase::InactiveTick];
    }
    if !device_ok {
        return vec![FramePhase::Tick];
    }
    vec![
        FramePhase::Tick,
        FramePhase::UpdateAnimations,
        FramePhase::Draw,
    ]
}

/// A script/personality callback bound to a phase.
///
/// The reference fires these from within a phase's task — e.g.
/// `Personality.onChangeEnvironments` from `ScriptApp::tick`
/// (`script_app.cpp:205`). This is the order-verifiable handle to "script
/// callback"; the actual Python binding is a later item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseHook {
    /// The hook's name, e.g. `"Personality.onChangeEnvironments"`.
    pub name: &'static str,
    /// The phase it fires in.
    pub phase: FramePhase,
}

/// The phases and script callbacks of one frame, in the order they ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameLog {
    /// Frame index (1-based).
    pub frame: u64,
    /// The phases run, in order.
    pub phases: Vec<FramePhase>,
    /// The script hooks fired, in order.
    pub hooks: Vec<&'static str>,
}

/// Drives one frame at a time, firing each phase's hooks in order and
/// recording the sequence, so the acceptance can assert the order.
#[derive(Debug, Default)]
pub struct FrameLoop {
    frame: u64,
    hooks: Vec<PhaseHook>,
    logs: Vec<FrameLog>,
}

impl FrameLoop {
    /// An empty frame loop.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a script callback on a phase. Callbacks fire in registration
    /// order within their phase.
    pub fn add_hook(&mut self, hook: PhaseHook) {
        self.hooks.push(hook);
    }

    /// The hooks bound to a phase, in registration order.
    pub fn hooks_for(&self, phase: FramePhase) -> Vec<&'static str> {
        self.hooks
            .iter()
            .filter(|h| h.phase == phase)
            .map(|h| h.name)
            .collect()
    }

    /// Run one frame with the given app state, firing each phase's hooks in
    /// phase order and recording the sequence.
    pub fn run_frame(&mut self, active: bool, device_ok: bool) -> FrameLog {
        let phases = frame_phases(active, device_ok);
        let mut hooks = Vec::new();
        for phase in &phases {
            hooks.extend(self.hooks_for(*phase));
        }
        self.frame += 1;
        let log = FrameLog {
            frame: self.frame,
            phases,
            hooks,
        };
        self.logs.push(log.clone());
        log
    }

    /// Every frame run so far.
    pub fn logs(&self) -> &[FrameLog] {
        &self.logs
    }

    /// Frames run so far.
    pub fn frame_count(&self) -> u64 {
        self.frame
    }

    /// The distinct phases seen so far, in first-occurrence order. An active
    /// frame produces `[Tick, UpdateAnimations, Draw]`; a lost device or a
    /// minimised frame contributes only its own phases.
    pub fn phases_seen(&self) -> Vec<FramePhase> {
        let mut seen: Vec<FramePhase> = Vec::new();
        for log in &self.logs {
            for phase in &log.phases {
                if !seen.contains(phase) {
                    seen.push(*phase);
                }
            }
        }
        seen
    }
}

/// Whether a frame's phases are in canonical active-frame order: `Tick`
/// before `UpdateAnimations` before `Draw`, never repeated or reordered.
pub fn is_canonical_order(log: &FrameLog) -> bool {
    let mut last: Option<usize> = None;
    for phase in &log.phases {
        if let Some(index) = phase.active_index() {
            if let Some(prev) = last
                && index <= prev
            {
                return false;
            }
            last = Some(index);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_frame_runs_update_before_draw() {
        let log = FrameLoop::new().run_frame(true, true);
        assert_eq!(
            log.phases,
            vec![
                FramePhase::Tick,
                FramePhase::UpdateAnimations,
                FramePhase::Draw
            ]
        );
        assert!(is_canonical_order(&log));
    }

    #[test]
    fn minimised_frame_runs_only_inactive_tick() {
        let log = FrameLoop::new().run_frame(false, true);
        assert_eq!(log.phases, vec![FramePhase::InactiveTick]);
        // InactiveTick is not part of an active frame, but the single-phase
        // log is still canonical.
        assert!(is_canonical_order(&log));
    }

    #[test]
    fn lost_device_skips_draw_but_keeps_update() {
        // The reference ticks even with a lost device, but only draws when
        // `checkDevice()` is true (`app.cpp:1439`).
        let log = FrameLoop::new().run_frame(true, false);
        assert_eq!(log.phases, vec![FramePhase::Tick]);
        assert!(is_canonical_order(&log));
    }

    #[test]
    fn script_callbacks_fire_in_phase_order() {
        let mut fl = FrameLoop::new();
        // The personality callbacks the reference fires across a frame.
        fl.add_hook(PhaseHook {
            name: "onTick",
            phase: FramePhase::Tick,
        });
        fl.add_hook(PhaseHook {
            name: "onChangeEnvironments",
            phase: FramePhase::Tick,
        });
        fl.add_hook(PhaseHook {
            name: "onDrawFrame",
            phase: FramePhase::Draw,
        });
        let log = fl.run_frame(true, true);
        // Update-phase callbacks before the draw-phase callback; within a phase
        // they fire in registration order.
        assert_eq!(
            log.hooks,
            vec!["onTick", "onChangeEnvironments", "onDrawFrame"]
        );
        assert!(
            log.hooks.iter().position(|h| *h == "onTick").unwrap()
                < log.hooks.iter().position(|h| *h == "onDrawFrame").unwrap()
        );
    }

    #[test]
    fn draw_hooks_fire_after_the_draw_phase() {
        let mut fl = FrameLoop::new();
        fl.add_hook(PhaseHook {
            name: "onDrawFrame",
            phase: FramePhase::Draw,
        });
        fl.add_hook(PhaseHook {
            name: "onTick",
            phase: FramePhase::Tick,
        });
        for _ in 0..32 {
            let log = fl.run_frame(true, true);
            assert!(is_canonical_order(&log));
            // Every frame, the update hook precedes the draw hook.
            let tick_pos = log.hooks.iter().position(|h| *h == "onTick");
            let draw_pos = log.hooks.iter().position(|h| *h == "onDrawFrame");
            assert!(tick_pos.unwrap() < draw_pos.unwrap());
        }
        assert_eq!(fl.frame_count(), 32);
    }

    #[test]
    fn frame_phases_cover_reference_states() {
        // Active+ok, active+lost, inactive: the three reference frame shapes.
        assert_eq!(
            frame_phases(true, true).len(),
            3,
            "active frame: tick, animations, draw"
        );
        assert_eq!(frame_phases(true, false), vec![FramePhase::Tick]);
        assert_eq!(frame_phases(false, true), vec![FramePhase::InactiveTick]);
    }
}
