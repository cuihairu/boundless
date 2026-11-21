//! The test scene (BW-11-01-02's "测试场景").

use super::framebuffer::Framebuffer;

/// Number of cells the scene sweeps across the width.
pub const CELLS: u32 = 16;

/// A minimal animated test scene.
///
/// Each step advances `phase`, and each draw paints a colour band plus a
/// moving marker whose position depends on `phase`, so consecutive frames
/// produce *different* pixels. BW-11-01-02's acceptance is that this scene
/// keeps producing frames after the device is rebuilt — the frame content
/// continuing to change is the evidence, not merely a counter incrementing.
#[derive(Debug, Clone, Copy)]
pub struct TestScene {
    /// Animation phase in `[0, 1)`.
    pub phase: f32,
    /// Steps taken so far (monotonic across device resets).
    pub steps: u32,
    /// Steps per full phase wrap.
    pub period: u32,
}

impl Default for TestScene {
    fn default() -> Self {
        Self {
            phase: 0.0,
            steps: 0,
            period: CELLS,
        }
    }
}

impl TestScene {
    /// A scene at rest, one full sweep per `period` steps.
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the animation.
    pub fn step(&mut self) {
        self.steps = self.steps.wrapping_add(1);
        self.phase = (self.phase + 1.0 / self.period as f32).fract();
    }

    /// The cell (of [`CELLS`]) the marker currently sits in.
    pub fn cell(&self) -> u32 {
        (self.phase * CELLS as f32).floor() as u32
    }

    /// Draw this frame into the back buffer.
    pub fn draw(&self, fb: &mut Framebuffer) {
        if fb.width == 0 || fb.height == 0 {
            return;
        }
        let cell = self.cell();
        let cell_width = fb.width / CELLS;
        let bg = 0x20_28_38_ff;
        for x in 0..fb.width {
            let x_cell = (x / cell_width.max(1)).min(CELLS - 1);
            // Dimmer each step so the frame differs even when the marker
            // does not move.
            let shade = 0x10u32.saturating_mul(self.steps % 8);
            let r = (0x30u32 + shade + x_cell * 3) & 0xff;
            let g = (0x40u32 + shade) & 0xff;
            let b = (0x90u32 + shade) & 0xff;
            let base = (b << 16) | (g << 8) | r | 0xff00_0000;
            for y in 0..fb.height {
                fb.set_pixel(x, y, if x_cell == cell { base } else { bg });
            }
        }
    }
}
