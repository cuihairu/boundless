//! Device backends and the test scene (BW-11-01-02).
//!
//! The reference client renders with an `IDirect3DDevice9` obtained from
//! `RenderContext::createDevice` (`lib/moo/render_context.cpp:1164`). The
//! reference is a Windows/D3D9 binary, so this port's device is modelled at the
//! level the reference's own lifecycle guarantees are stated at — initialise
//! once, reset (rebuild) in place, present frames, release once — and the
//! concrete pixel-producing backend is a deterministic software rasterizer
//! that needs no GPU, so the whole acceptance can run headless.

pub mod framebuffer;
pub mod map_scene;
pub mod scene;
pub mod software;

/// Something the device can present: it paints one frame into the back
/// buffer. The reference's scene graph sits behind `Moo::rc().device()`
/// drawing into the same back buffer; the port keeps the seam so the test
/// scene and the fixed-map scene drive the same device face.
pub trait Scene {
    /// Draw this frame into the back buffer.
    fn draw(&self, fb: &mut framebuffer::Framebuffer);
}

/// The result of presenting one frame.
///
/// The reference presents each frame with
/// `Moo::rc().device()->Present(&sourceRect, ...)`. `frame_index` is the
/// device's running frame count (monotonic across resets, because a reset
/// reinitialises the *same* device object) and `checksum` is a digest of the
/// presented back buffer, so a test can see a frame actually *changed*
/// rather than only counting them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameResult {
    /// Device frame count after this frame.
    pub frame_index: u64,
    /// Digest of the presented back buffer.
    pub checksum: u64,
}
