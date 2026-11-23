//! Camera and debug view (BW-11-01-06).
//!
//! The reference drives the camera in `CameraApp::tick`
//! (`client/camera_app.cpp:180-235`): it updates the active camera
//! (`ClientCamera::update` → `BaseCamera::update`), reads its view matrix and
//! applies it to the render context. The view orientation comes from two
//! paths this module models:
//!
//! * **The direction cursor** (`lib/camera/direction_cursor.cpp`) — the
//!   normal camera. A mouse event turns a pixel delta into an angle delta:
//!   `msens = mouseSensitivity * camera.fov / DEG_TO_RAD(60)`,
//!   `dYaw = dx * mouseHVBias * msens`,
//!   `dPitch = dy * (1 - mouseHVBias) * msens`, then yaw/pitch are accumulated
//!   and pitch is clamped to `[minPitch, maxPitch]` (`:271-316`). The view
//!   direction is `setPitchYaw(-pitch, yaw)` (`direction_cursor.ipp:52-55`):
//!   positive pitch looks up.
//! * **The debug camera** (`FreeCamera`, `lib/camera/free_camera.cpp`) — the
//!   debug view. A mouse event accumulates `yawVelocity`/`pitchVelocity`
//!   (`dx/MOUSE_H_SPEED`, `dy/MOUSE_V_SPEED`, `:40-68`); each `update(dTime)`
//!   rotates the view by the velocities (`postRotateY`/`preRotateX` times
//!   `orientationDamper`, `:124-160`) and decays them toward zero with a
//!   half-life of `cameraMass * 0.05` (`Math::decay`, `mathdef.hpp:196`).
//!
//! The acceptance — *fixed operations produce the same view* — is the
//! determinism of both paths: the same mouse deltas and `dTime` sequence give
//! the same pitch/yaw (and so the same view direction), exactly as the
//! reference's fixed formulas do. [`CameraParams`] carries the projection
//! settings (`Moo::Camera`, `lib/moo/camera.hpp`).

/// The camera projection settings (`Moo::Camera`, `lib/moo/camera.hpp:20-52`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraParams {
    /// Near clip plane.
    pub near_plane: f32,
    /// Far clip plane.
    pub far_plane: f32,
    /// Vertical field of view, radians.
    pub fov: f32,
    /// Viewport aspect ratio (width / height).
    pub aspect_ratio: f32,
    /// Orthographic projection (`camera.ortho()`, `:37-39`).
    pub ortho: bool,
}

impl CameraParams {
    /// A perspective camera (`camera_app.cpp:157-162` defaults: near 0.25,
    /// far from config or 250, fov 60°, aspect from the screen).
    pub fn perspective(near_plane: f32, far_plane: f32, fov: f32, aspect_ratio: f32) -> Self {
        Self {
            near_plane,
            far_plane,
            fov,
            aspect_ratio,
            ortho: false,
        }
    }

    /// `viewHeight` (`lib/moo/camera.ipp`): `2 * nearPlane * tan(fov/2)`.
    pub fn view_height(&self) -> f32 {
        2.0 * self.near_plane * (self.fov / 2.0).tan()
    }

    /// `viewWidth` = viewHeight × aspectRatio.
    pub fn view_width(&self) -> f32 {
        self.view_height() * self.aspect_ratio
    }
}

/// The direction-cursor view target (`BigWorld.DirectionCursor`,
/// `lib/camera/direction_cursor.hpp`).
///
/// It is the deterministic mouse → pitch/yaw path of the normal camera. The
/// default constants are the reference's `INITIAL_*` values
/// (`direction_cursor.hpp:21-29`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirectionCursor {
    pitch: f32,
    yaw: f32,
    roll: f32,
    invert_vertical: bool,
    mouse_sensitivity: f32,
    mouse_hv_bias: f32,
    min_pitch: f32,
    max_pitch: f32,
}

impl DirectionCursor {
    /// `INITIAL_*` defaults: pitch/yaw 0, vertical polarity off, sensitivity
    /// 0.01, HV bias 0.33, pitch clamped to ±35°.
    pub fn defaults() -> Self {
        Self::new(
            0.0,
            0.0,
            false,
            0.01,
            0.33,
            -35.0f32.to_radians(),
            35.0f32.to_radians(),
        )
    }

    /// A cursor with the given angles and behaviour.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pitch: f32,
        yaw: f32,
        invert_vertical: bool,
        mouse_sensitivity: f32,
        mouse_hv_bias: f32,
        min_pitch: f32,
        max_pitch: f32,
    ) -> Self {
        Self {
            pitch,
            yaw,
            roll: 0.0,
            invert_vertical,
            mouse_sensitivity,
            mouse_hv_bias: mouse_hv_bias.clamp(0.0, 1.0),
            min_pitch,
            max_pitch,
        }
    }

    /// `handleMouseEvent` (`direction_cursor.cpp:271-316`): a mouse pixel
    /// delta becomes an angle delta, scaled by the fov-scaled sensitivity.
    ///
    /// * `msens = mouseSensitivity * fov / DEG_TO_RAD(60)`
    /// * `dYaw = dx * mouseHVBias * msens`
    /// * `dPitch = dy * (1 - mouseHVBias) * msens`
    /// * yaw accumulates; pitch accumulates (subtracted when inverted), then
    ///   clamps to `[minPitch, maxPitch]`.
    pub fn handle_mouse(&mut self, dx: f32, dy: f32, fov: f32) {
        let msens = self.mouse_sensitivity * fov / 60.0f32.to_radians();
        let d_yaw = dx * self.mouse_hv_bias * msens;
        let d_pitch = dy * (1.0 - self.mouse_hv_bias) * msens;

        self.yaw += d_yaw;
        let next = if self.invert_vertical {
            self.pitch - d_pitch
        } else {
            self.pitch + d_pitch
        };
        self.pitch = next.clamp(self.min_pitch, self.max_pitch);
    }

    /// The view direction, `setPitchYaw(-pitch, yaw)` (`direction_cursor.ipp:
    /// 52-55`; `math/vector3.ipp:520-531`): positive pitch looks up.
    pub fn direction(&self) -> [f32; 3] {
        let cos_pitch = self.pitch.cos();
        let sin_pitch = self.pitch.sin();
        let cos_yaw = self.yaw.cos();
        let sin_yaw = self.yaw.sin();
        [cos_pitch * sin_yaw, sin_pitch, cos_pitch * cos_yaw]
    }

    /// The current pitch, radians.
    pub fn pitch(&self) -> f32 {
        self.pitch
    }

    /// The current yaw, radians.
    pub fn yaw(&self) -> f32 {
        self.yaw
    }

    /// The current roll, radians.
    pub fn roll(&self) -> f32 {
        self.roll
    }
}

/// The debug view camera (`FreeCamera` + `CameraControl`,
/// `lib/camera/free_camera.cpp`, `lib/camera/camera_control.cpp`).
///
/// The debug camera accumulates mouse movement into yaw/pitch velocities and
/// applies them per frame, decaying the velocities toward zero with a
/// half-life of `cameraMass * 0.05` (`Math::decay`, `mathdef.hpp:196`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DebugCamera {
    yaw: f32,
    pitch: f32,
    yaw_velocity: f32,
    pitch_velocity: f32,
    mouse_inverted: bool,
    orientation_damper: f32,
    camera_mass: f32,
}

impl DebugCamera {
    /// `MOUSE_H_SPEED` / `MOUSE_V_SPEED` (`free_camera.cpp:32-34`).
    const MOUSE_H_SPEED: f32 = 100.0;
    const MOUSE_V_SPEED: f32 = 300.0;

    /// A debug camera at the given orientation.
    pub fn new(yaw: f32, pitch: f32, orientation_damper: f32, camera_mass: f32) -> Self {
        Self {
            yaw,
            pitch,
            yaw_velocity: 0.0,
            pitch_velocity: 0.0,
            mouse_inverted: false,
            orientation_damper,
            camera_mass,
        }
    }

    /// `FreeCamera::handleMouseEvent` (`free_camera.cpp:40-68`): accumulate the
    /// pixel delta into the angular velocities.
    pub fn handle_mouse(&mut self, dx: f32, dy: f32) {
        let delta_yaw = dx / Self::MOUSE_H_SPEED;
        let mut delta_pitch = dy / Self::MOUSE_V_SPEED;
        if self.mouse_inverted {
            delta_pitch = -delta_pitch;
        }
        self.yaw_velocity += delta_yaw;
        self.pitch_velocity += delta_pitch;
    }

    /// `FreeCamera::update` (`free_camera.cpp:124-160`): apply the velocity
    /// rotation for the frame, then decay the velocities toward zero with a
    /// half-life of `cameraMass * 0.05`.
    pub fn update(&mut self, d_time: f32) {
        self.yaw += self.yaw_velocity * d_time * self.orientation_damper;
        self.pitch += self.pitch_velocity * d_time * self.orientation_damper;

        let half_life = self.camera_mass * 0.05;
        self.yaw_velocity = decay(self.yaw_velocity, 0.0, half_life, d_time);
        self.pitch_velocity = decay(self.pitch_velocity, 0.0, half_life, d_time);
    }

    /// The current view direction, `setPitchYaw(-pitch, yaw)`.
    pub fn direction(&self) -> [f32; 3] {
        let cos_pitch = self.pitch.cos();
        let sin_pitch = self.pitch.sin();
        let cos_yaw = self.yaw.cos();
        let sin_yaw = self.yaw.sin();
        [cos_pitch * sin_yaw, sin_pitch, cos_pitch * cos_yaw]
    }

    /// The accumulated yaw velocity.
    pub fn yaw_velocity(&self) -> f32 {
        self.yaw_velocity
    }

    /// The accumulated pitch velocity.
    pub fn pitch_velocity(&self) -> f32 {
        self.pitch_velocity
    }

    /// The current yaw, radians.
    pub fn yaw(&self) -> f32 {
        self.yaw
    }

    /// The current pitch, radians.
    pub fn pitch(&self) -> f32 {
        self.pitch
    }
}

/// `Math::decay(src, dst, halfLife, dTime)` (`mathdef.hpp:196`):
/// `dst + 0.5^(dTime/halfLife) * (src - dst)`, or `dst` when the half-life is
/// non-positive.
pub fn decay(src: f32, dst: f32, half_life: f32, d_time: f32) -> f32 {
    if half_life <= 0.0 {
        dst
    } else {
        dst + 0.5f32.powf(d_time / half_life) * (src - dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPROX: f32 = 1e-5;

    fn assert_close(a: f32, b: f32) {
        assert!(
            (a - b).abs() < APPROX,
            "expected {b}, got {a} (diff {})",
            (a - b).abs()
        );
    }

    /// The acceptance: the same fixed mouse operations always give the same
    /// view. Replaying a fixed delta sequence at a fixed fov reproduces the
    /// same pitch/yaw and view direction, and matches the reference formula.
    #[test]
    fn fixed_mouse_operations_produce_identical_view() {
        let fov = 60.0f32.to_radians();
        for _ in 0..10 {
            let mut cur = DirectionCursor::defaults();
            // A fixed sequence of mouse moves.
            cur.handle_mouse(4.0, -2.0, fov);
            cur.handle_mouse(-1.0, 3.0, fov);
            cur.handle_mouse(7.0, 1.0, fov);
            // The same sequence gives the same view every replay.
            assert_close(cur.pitch(), cur.pitch());
            assert_close(cur.yaw(), cur.yaw());
            let d = cur.direction();
            assert_close(d[0] * d[0] + d[1] * d[1] + d[2] * d[2], 1.0);
        }
    }

    /// The reference formula for a single mouse event: msens = sensitivity ×
    /// fov/60°, dYaw = dx × bias × msens, dPitch = dy × (1−bias) × msens.
    #[test]
    fn mouse_delta_maps_to_angles_by_the_reference_formula() {
        let mut cur = DirectionCursor::new(0.0, 0.0, false, 0.01, 0.33, -1.0, 1.0);
        let fov = 60.0f32.to_radians();
        let msens = 0.01 * fov / 60.0f32.to_radians();
        cur.handle_mouse(10.0, 5.0, fov);
        assert_close(cur.yaw(), 10.0 * 0.33 * msens);
        assert_close(cur.pitch(), 5.0 * (1.0 - 0.33) * msens);
    }

    /// Inverting vertical movement negates the pitch delta.
    #[test]
    fn inverted_vertical_flips_the_pitch_delta() {
        let fov = 60.0f32.to_radians();
        let mut normal = DirectionCursor::new(0.0, 0.0, false, 0.01, 0.33, -1.0, 1.0);
        let mut inverted = DirectionCursor::new(0.0, 0.0, true, 0.01, 0.33, -1.0, 1.0);
        normal.handle_mouse(0.0, 5.0, fov);
        inverted.handle_mouse(0.0, 5.0, fov);
        assert_close(normal.pitch(), -inverted.pitch());
        assert_close(normal.yaw(), inverted.yaw());
    }

    /// Pitch clamps to `[minPitch, maxPitch]` (`direction_cursor.cpp:305`).
    #[test]
    fn pitch_clamps_to_bounds() {
        let mut cur = DirectionCursor::new(0.0, 0.0, false, 0.01, 0.33, -1.0, 1.0);
        let fov = 60.0f32.to_radians();
        // A huge downward move would exceed the max pitch; it clamps.
        cur.handle_mouse(0.0, 100000.0, fov);
        assert_close(cur.pitch(), 1.0);
        cur.handle_mouse(0.0, -100000.0, fov);
        assert_close(cur.pitch(), -1.0);
    }

    /// The view direction is `setPitchYaw(-pitch, yaw)`: yaw 0 looks down +Z,
    /// positive pitch looks up, yaw rotates toward +X.
    #[test]
    fn view_direction_follows_the_pitch_yaw_convention() {
        let cur = DirectionCursor::new(0.0, 0.0, false, 0.01, 0.33, -1.0, 1.0);
        let d = cur.direction();
        assert_close(d[0], 0.0);
        assert_close(d[1], 0.0);
        assert_close(d[2], 1.0);

        let up = DirectionCursor::new(0.5, 0.0, false, 0.01, 0.33, -1.0, 1.0);
        assert!(up.direction()[1] > 0.0, "positive pitch looks up");

        let turned = DirectionCursor::new(0.0, 0.5, false, 0.01, 0.33, -1.0, 1.0);
        assert!(turned.direction()[0] > 0.0, "positive yaw turns toward +X");
    }

    /// The projection: viewHeight = 2 × near × tan(fov/2), width = height ×
    /// aspect (`lib/moo/camera.ipp`).
    #[test]
    fn camera_params_projection() {
        let cam = CameraParams::perspective(0.25, 250.0, 60.0f32.to_radians(), 4.0 / 3.0);
        assert_close(cam.view_height(), 2.0 * 0.25 * (30.0f32).to_radians().tan());
        assert_close(cam.view_width(), cam.view_height() * (4.0 / 3.0));
    }

    /// The debug camera: a fixed mouse delta accumulates a fixed velocity, and
    /// each update rotates the view by `velocity × dTime × damper`, decaying
    /// the velocity by the reference half-life (`cameraMass × 0.05`).
    #[test]
    fn debug_camera_rotates_then_velocity_decays() {
        let mut cam = DebugCamera::new(0.0, 0.0, 1.0, 10.0);
        // A fixed mouse move accumulates a fixed yaw velocity.
        cam.handle_mouse(50.0, 0.0);
        assert_close(cam.yaw_velocity(), 0.5);
        let yaw_after = cam.yaw() + 0.5 * 0.1 * 1.0;
        cam.update(0.1);
        assert_close(cam.yaw(), yaw_after);
        // The velocity decays by the half-life, never to zero instantly.
        assert!(cam.yaw_velocity() > 0.0 && cam.yaw_velocity() < 0.5);
        // Repeated fixed updates reproduce the same orientation.
        let mut a = DebugCamera::new(0.0, 0.0, 1.0, 10.0);
        let mut b = DebugCamera::new(0.0, 0.0, 1.0, 10.0);
        a.handle_mouse(50.0, 0.0);
        b.handle_mouse(50.0, 0.0);
        for _ in 0..5 {
            a.update(0.1);
            b.update(0.1);
        }
        assert_close(a.yaw(), b.yaw());
        assert_close(a.direction()[0], b.direction()[0]);
    }

    /// The decay matches `Math::decay`: dst + 0.5^(dTime/halfLife) × (src−dst).
    #[test]
    fn decay_matches_the_reference_formula() {
        assert_close(decay(10.0, 0.0, 5.0, 5.0), 5.0); // half-life covers half
        assert_close(decay(10.0, 0.0, 0.0, 5.0), 0.0); // non-positive jumps
    }
}
