//! A software back buffer of RGBA8 pixels.
//!
//! The reference's back buffer is the D3D `IDirect3DBackBuffer` queried in
//! `updateDeviceInfo` (`lib/moo/render_context.cpp:811`) — `GetBackBuffer` /
//! `GetDesc` give its width and height, which the client turns into the
//! `halfScreenWidth_` / `halfScreenHeight_` it renders with. This is that
//! buffer in a form that needs no GPU: `width * height` pixels of `u32`
//! (blue, green, red, alpha).

/// A software back buffer.
///
/// Pixels are `0xAABBGGRR` (red in the low byte, alpha in the high byte),
/// matching a D3D `D3DFMT_A8R8G8B8` back buffer.
#[derive(Debug, Default)]
pub struct Framebuffer {
    /// Buffer width in pixels.
    pub width: u32,
    /// Buffer height in pixels.
    pub height: u32,
    /// Pixel data, `width * height` RGBA8 values.
    pub pixels: Vec<u32>,
}

impl Framebuffer {
    /// An empty buffer of the given size.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width.saturating_mul(height) as usize],
        }
    }

    /// Reallocate to a new size, clearing the pixels. This is the software
    /// half of a device reset changing presentation parameters.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width != self.width || height != self.height {
            *self = Self::new(width, height);
        }
    }

    /// Fill the whole buffer with one colour.
    pub fn fill(&mut self, rgba: u32) {
        self.pixels.fill(rgba);
    }

    /// Write a pixel, ignoring coordinates outside the buffer.
    pub fn set_pixel(&mut self, x: u32, y: u32, rgba: u32) {
        if x < self.width && y < self.height {
            self.pixels[(y * self.width + x) as usize] = rgba;
        }
    }

    /// Read a pixel, or black outside the buffer.
    pub fn get_pixel(&self, x: u32, y: u32) -> u32 {
        if x < self.width && y < self.height {
            self.pixels[(y * self.width + x) as usize]
        } else {
            0
        }
    }

    /// A digest of the buffer contents. Two different frames have different
    /// digests, so it is the test's evidence that a frame actually rendered.
    pub fn checksum(&self) -> u64 {
        // FNV-1a over the pixels: cheap, deterministic, no dependencies.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for px in &self.pixels {
            h ^= u64::from(*px).rotate_left(13);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }
}
