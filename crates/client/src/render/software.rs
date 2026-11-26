//! The software device backend: the concrete pixel-producing half of the
//! client's render device.
//!
//! The reference device is an `IDirect3DDevice9` (`render_context.cpp:1164`).
//! Its observable contract — take presentation parameters, reinitialise
//! itself in place on a reset, and present a back buffer each frame — is what
//! this backend implements against a [`Framebuffer`], so the device lifecycle
//! can be exercised on a machine with no GPU and no display server.

use super::FrameResult;
use super::framebuffer::Framebuffer;
use super::scene::TestScene;

/// A device that renders into an in-memory back buffer.
#[derive(Debug)]
pub struct SoftwareDevice {
    framebuffer: Framebuffer,
    /// Frames presented by this device object. A reset reinitialises the same
    /// object, so this keeps counting rather than restarting.
    frames: u64,
    /// Digest of the last frame presented.
    last_checksum: u64,
}

impl SoftwareDevice {
    /// Initialise the backend with the first presentation parameters.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            framebuffer: Framebuffer::new(width, height),
            frames: 0,
            last_checksum: 0,
        }
    }

    /// Reinitialise the device in place with (possibly new) presentation
    /// parameters — the software half of `device_->Reset(&presentParameters_)`
    /// (`render_context.cpp:731`). Returns the new back-buffer area.
    pub fn reset(&mut self, width: u32, height: u32) -> u32 {
        self.framebuffer.resize(width, height);
        self.framebuffer
            .width
            .saturating_mul(self.framebuffer.height)
    }

    /// Present one frame of the scene.
    pub fn present(&mut self, scene: &TestScene) -> FrameResult {
        scene.draw(&mut self.framebuffer);
        self.frames += 1;
        self.last_checksum = self.framebuffer.checksum();
        FrameResult {
            frame_index: self.frames,
            checksum: self.last_checksum,
        }
    }

    /// Frames presented by this device object.
    pub fn frame_count(&self) -> u64 {
        self.frames
    }

    /// Digest of the last presented frame.
    pub fn last_checksum(&self) -> u64 {
        self.last_checksum
    }

    /// Back-buffer size, as `updateDeviceInfo` reads it from the back buffer.
    pub fn back_buffer(&self) -> (u32, u32) {
        (self.framebuffer.width, self.framebuffer.height)
    }

    /// The back buffer itself (`GetBackBuffer`), for the screenshot face.
    pub fn framebuffer(&self) -> &Framebuffer {
        &self.framebuffer
    }
}
