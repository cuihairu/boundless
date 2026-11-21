//! The winit-backed client application (BW-11-01-01: window creation,
//! presentation-mode switching, clean exit).
//!
//! This is the real window binding for the pure `window_mode` / `device`
//! modules. It owns one window, cycles the presentation mode, and releases
//! the device exactly once on exit. The acceptance — three-mode switching
//! without device leak — is exercised by the `#[ignore]` integration test at
//! the bottom, run under a display (locally via `xvfb-run`, or on a real
//! machine).

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::{Fullscreen, Window, WindowId};

use crate::device::{DeviceLifecycle, DeviceStats};
use crate::window_mode::{SurfaceAction, WindowMode, surface_action};

/// How the app terminates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The window's close button was pressed.
    CloseRequested,
    /// The frame budget was reached.
    FrameBudget,
}

/// What a run of the client app produced.
#[derive(Debug, Clone, Copy)]
pub struct AppOutcome {
    /// Device lifetime counters at exit (after the device was released).
    pub stats: DeviceStats,
    /// Orphaned device handles (zero means no leak).
    pub leaked: u64,
    /// The device id observed after every mode switch. A single distinct id
    /// proves the device was reused, not re-created.
    pub device_ids: [u64; 3],
    /// Presentation-mode switches performed.
    pub mode_switches: u32,
    /// Why the loop stopped.
    pub exit_reason: ExitReason,
}

/// Tuning for a run.
#[derive(Debug, Clone, Copy)]
pub struct RunConfig {
    /// Stop after this many redraws.
    pub max_frames: u32,
    /// Advance the presentation mode every N redraws.
    pub cycle_every: u32,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_frames: 30,
            cycle_every: 5,
        }
    }
}

/// The application state. Owns the window and the device lifecycle.
pub struct ClientApp {
    window: Option<Window>,
    device: DeviceLifecycle,
    mode: WindowMode,
    frames: u32,
    mode_switches: u32,
    device_ids: [u64; 3],
    ids_filled: usize,
    config: RunConfig,
    outcome: Option<AppOutcome>,
}

impl ClientApp {
    fn new(config: RunConfig) -> Self {
        Self {
            window: None,
            device: DeviceLifecycle::new(),
            mode: WindowMode::Windowed,
            frames: 0,
            mode_switches: 0,
            device_ids: [0; 3],
            ids_filled: 0,
            config,
            outcome: None,
        }
    }

    fn record_device(&mut self) {
        if let Some(id) = self.device.device_id() {
            let i = self.ids_filled.min(2);
            self.device_ids[i] = id;
            self.ids_filled = i + 1;
        }
    }

    /// Advance to the next presentation mode in the cycle windowed →
    /// borderless → exclusive → windowed.
    fn advance_mode(&mut self) {
        let next = match self.mode {
            WindowMode::Windowed => WindowMode::Borderless,
            WindowMode::Borderless => WindowMode::Exclusive,
            WindowMode::Exclusive => WindowMode::Windowed,
        };
        if let Some(w) = &self.window {
            match surface_action(self.mode, next) {
                SurfaceAction::Noop => {}
                SurfaceAction::Restyle | SurfaceAction::Reconfigure => {
                    match next {
                        WindowMode::Windowed => {
                            w.set_fullscreen(None);
                        }
                        WindowMode::Borderless => {
                            w.set_fullscreen(Some(Fullscreen::Borderless(None)));
                        }
                        WindowMode::Exclusive => {
                            // Exclusive fullscreen needs a real video mode.
                            if let Some(mode) =
                                w.primary_monitor().and_then(|m| m.video_modes().next())
                            {
                                w.set_fullscreen(Some(Fullscreen::Exclusive(mode)));
                            }
                        }
                    }
                }
            }
        }
        // The device is untouched by a mode switch (the no-leak property).
        self.device.on_mode_switch();
        self.mode = next;
        self.mode_switches += 1;
        self.record_device();
        // Redraw in the new mode.
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn finish(&mut self, reason: ExitReason, el: &ActiveEventLoop) {
        self.device.release();
        self.outcome = Some(AppOutcome {
            stats: self.device.stats(),
            leaked: self.device.leaked(),
            device_ids: self.device_ids,
            mode_switches: self.mode_switches,
            exit_reason: reason,
        });
        el.exit();
    }
}

impl ApplicationHandler for ClientApp {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none() {
            let attrs = Window::default_attributes()
                .with_title("boundless client (BW-11-01-01)")
                .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
            match el.create_window(attrs) {
                Ok(w) => {
                    self.window = Some(w);
                    let _ = self.device.ensure_device();
                    self.record_device();
                }
                Err(_) => {
                    // No display: fail the run cleanly so callers can tell a
                    // headless machine from a real failure.
                    self.finish(ExitReason::CloseRequested, el);
                }
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.finish(ExitReason::CloseRequested, el),
            WindowEvent::RedrawRequested => {
                self.frames += 1;
                if self.config.cycle_every > 0
                    && self.frames.is_multiple_of(self.config.cycle_every)
                {
                    self.advance_mode();
                }
                if self.frames >= self.config.max_frames {
                    self.finish(ExitReason::FrameBudget, el);
                } else if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

/// Run the client application and return its outcome.
///
/// Requires a display (real machine, or `xvfb-run` on a headless Linux box).
///
/// On Linux the event loop is built with `any_thread`, so the acceptance can
/// be driven from a `cargo test` worker thread (the winit default requires
/// the main thread). Windows/macOS keep the default main-thread requirement,
/// which is how the real client runs.
pub fn run(config: RunConfig) -> Result<AppOutcome, Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    let event_loop = {
        use winit::event_loop::EventLoop;
        use winit::platform::wayland::EventLoopBuilderExtWayland;
        use winit::platform::x11::EventLoopBuilderExtX11;
        let mut builder = EventLoop::builder();
        EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
        EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
        builder.build()?
    };
    #[cfg(not(target_os = "linux"))]
    let event_loop = winit::event_loop::EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = ClientApp::new(config);
    event_loop.run_app(&mut app)?;
    app.outcome
        .ok_or_else(|| "client app exited without an outcome".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The BW-11-01-01 acceptance: window creation, three-mode switching and
    /// clean exit with no device leak. `#[ignore]`: needs a display — run with
    /// `xvfb-run cargo test --features winit-backend -p client -- --ignored`.
    #[test]
    #[ignore = "needs a display; run under xvfb-run or on a real machine"]
    fn three_mode_switching_does_not_leak_the_device() {
        let cfg = RunConfig {
            max_frames: 40,
            cycle_every: 4,
        };
        let outcome = run(cfg).expect("winit should run under a display");
        assert_eq!(outcome.exit_reason, ExitReason::FrameBudget);
        assert!(outcome.mode_switches >= 3, "at least one full mode cycle");

        // The device was created once, never re-created, and released: the
        // three-mode switching leaked nothing.
        assert_eq!(outcome.stats.created, 1, "device created exactly once");
        assert_eq!(outcome.stats.live, 0, "device released on exit");
        assert_eq!(outcome.stats.dropped, 1);
        assert_eq!(outcome.leaked, 0, "no device leaked");
        assert_eq!(
            outcome.device_ids[0], outcome.device_ids[1],
            "same device across windowed→borderless"
        );
        assert_eq!(
            outcome.device_ids[1], outcome.device_ids[2],
            "same device across borderless→exclusive"
        );
    }
}
