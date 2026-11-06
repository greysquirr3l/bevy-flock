//! A camera you can fly: deterministic, clamped, and independent of any window.
//!
//! The renderer takes a [`Camera`] — an eye, a target and a lens — because that is all a rasteriser
//! needs. Exploring a scene needs something with state: a heading that accumulates over a second of
//! held key, a pitch that cannot roll past vertical, and a speed measured in metres per second
//! rather than metres per frame. That is this module, and it is pure arithmetic so the same
//! keystrokes produce the same camera on every machine.

use bevy_math::Vec3;

use crate::raster::Camera;

/// How far the pitch may tilt, radians: just short of straight up or down.
///
/// Stopping short of the poles keeps the world's up vector out of the look direction, so a right
/// vector can always be computed and the view matrix never degenerates.
pub const MAX_PITCH: f32 = 1.553_343; // 89 degrees

/// The most time a single step may advance, seconds.
///
/// A stalled frame (a breakpoint, a scheduling hiccup, a laptop lid) would otherwise teleport the
/// camera through a wall of ruins. Clamping the step trades a skipped metre for a camera that never
/// tunnels.
pub const MAX_STEP: f32 = 0.1;

/// A first-person rig: where it stands, where it looks, and how wide the lens is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraRig {
    /// Eye position, metres.
    pub position: Vec3,
    /// Heading, radians: zero looks along `+Z`, positive turns towards `+X`.
    pub yaw: f32,
    /// Elevation, radians: positive looks up, clamped to [`MAX_PITCH`].
    pub pitch: f32,
    /// Vertical field of view, radians.
    pub fov_y: f32,
    /// Near plane, metres.
    pub near: f32,
    /// Far plane, metres.
    pub far: f32,
}

/// What the controls asked for this frame.
///
/// Every axis is analogue and in `-1.0..=1.0`, which is what a stick or a key pair produces after
/// it has been normalised: a digital key is a stick that only ever reads full deflection. Keeping
/// the rig in terms of axes is what lets a keyboard and a gamepad drive the same camera.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CameraInput {
    /// Push along the lens.
    pub forward: f32,
    /// Slide to the right.
    pub strafe: f32,
    /// Climb relative to the horizon, not to the lens.
    pub rise: f32,
    /// Turn, radians per second at full deflection.
    pub look_yaw: f32,
    /// Tilt, radians per second at full deflection.
    pub look_pitch: f32,
    /// Narrow or widen the lens, radians of field of view per second at full deflection.
    pub zoom: f32,
}

impl CameraInput {
    /// Whether anything at all is being asked for, which is how a caller decides to re-render.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.forward == 0.0
            && self.strafe == 0.0
            && self.rise == 0.0
            && self.look_yaw == 0.0
            && self.look_pitch == 0.0
            && self.zoom == 0.0
    }
}

impl Default for CameraRig {
    fn default() -> Self {
        Self {
            position: Vec3::new(0.0, 1.7, -12.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y: 0.9,
            near: 0.1,
            far: 4000.0,
        }
    }
}

impl CameraRig {
    /// A rig standing at `position` and looking along `yaw`/`pitch`.
    #[must_use]
    pub fn new(position: Vec3, yaw: f32, pitch: f32) -> Self {
        Self {
            position,
            yaw,
            pitch: pitch.clamp(-MAX_PITCH, MAX_PITCH),
            ..Self::default()
        }
    }

    /// Recover a rig from a camera, so a shot's framed view can be flown on from.
    ///
    /// The camera's lens is kept; its eye and its look direction become the rig's state. A camera
    /// with no distance between eye and target has no direction to recover, so it faces `+Z`.
    #[must_use]
    pub fn from_camera(camera: &Camera) -> Self {
        let direction = (camera.target - camera.eye).normalize_or_zero();
        let direction = if direction == Vec3::ZERO {
            Vec3::Z
        } else {
            direction
        };
        Self {
            position: camera.eye,
            yaw: direction.x.atan2(direction.z),
            pitch: direction
                .y
                .clamp(-1.0, 1.0)
                .asin()
                .clamp(-MAX_PITCH, MAX_PITCH),
            fov_y: camera.fov_y,
            near: camera.near,
            far: camera.far,
        }
    }

    /// The unit vector the lens points along.
    #[must_use]
    pub fn forward(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        Vec3::new(sin_yaw * cos_pitch, sin_pitch, cos_yaw * cos_pitch)
    }

    /// The unit vector pointing to the rig's right, level with the horizon.
    #[must_use]
    pub fn right(&self) -> Vec3 {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        Vec3::new(cos_yaw, 0.0, -sin_yaw)
    }

    /// A camera looking where the rig looks.
    #[must_use]
    pub fn camera(&self) -> Camera {
        Camera {
            eye: self.position,
            target: self.position + self.forward(),
            fov_y: self.fov_y,
            near: self.near,
            far: self.far,
        }
    }

    /// Advance the rig by `step` seconds at `speed` metres per second.
    ///
    /// A move vector longer than one unit is normalised, so holding two keys is not faster than
    /// holding one; the step is clamped to [`MAX_STEP`]; and the result is frame-rate independent
    /// by construction, because every term is per second.
    pub fn fly(&mut self, input: &CameraInput, step: f32, speed: f32) {
        let step = step.clamp(0.0, MAX_STEP);
        if step == 0.0 {
            return;
        }
        let mut move_vector =
            self.forward() * input.forward + self.right() * input.strafe + Vec3::Y * input.rise;
        if move_vector.length_squared() > 1.0 {
            move_vector = move_vector.normalize_or_zero();
        }
        self.position += move_vector * speed * step;
    }

    /// Turn and tilt the rig, `sensitivity` being radians per second at full deflection.
    pub fn look(&mut self, input: &CameraInput, step: f32, sensitivity: f32) {
        let step = step.clamp(0.0, MAX_STEP);
        if step == 0.0 {
            return;
        }
        self.yaw += input.look_yaw * sensitivity * step;
        // Keep the heading in one turn, so a long session cannot accumulate a float large enough to
        // lose precision in the sine.
        let turn = std::f32::consts::TAU;
        self.yaw = (self.yaw + turn) % turn;
        self.pitch =
            (self.pitch + input.look_pitch * sensitivity * step).clamp(-MAX_PITCH, MAX_PITCH);
        self.fov_y = (self.fov_y + input.zoom * step).clamp(0.25, 2.4);
    }

    /// Point the rig at `pivot` from `radius` metres away along `yaw`, `pitch`.
    ///
    /// This is the orbit an inspection pass wants: the subject stays framed while the camera swings
    /// around it.
    pub fn orbit(&mut self, pivot: Vec3, yaw: f32, pitch: f32, radius: f32) {
        let pitch = pitch.clamp(-MAX_PITCH, MAX_PITCH);
        let (sin_yaw, cos_yaw) = yaw.sin_cos();
        let (sin_pitch, cos_pitch) = pitch.sin_cos();
        let offset = Vec3::new(sin_yaw * cos_pitch, sin_pitch, cos_yaw * cos_pitch) * radius;
        self.position = pivot + offset;
        // Face the pivot: the rig looks back down the offset it was pushed out along.
        self.yaw = (-offset.x).atan2(-offset.z);
        self.pitch = (-offset.y)
            .atan2(offset.x.hypot(offset.z))
            .clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Stand `distance` metres back from `pivot` at a three-quarter angle, at eye height.
    pub fn frame(&mut self, pivot: Vec3, distance: f32) {
        let yaw = 0.6_f32;
        let pitch = 0.18_f32;
        self.orbit(pivot + Vec3::Y * 1.6, yaw, pitch, distance.max(1.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1.0e-4
    }

    #[test]
    fn forward_follows_the_heading() {
        // Ten steps of a tenth of a second at ten metres a second is one metre of travel: the step
        // is clamped to `MAX_STEP`, so a test that asked for a whole second at once would be
        // testing the clamp instead of the heading.
        let mut rig = CameraRig::new(Vec3::ZERO, 0.0, 0.0);
        for _ in 0..10 {
            rig.fly(
                &CameraInput {
                    forward: 1.0,
                    ..CameraInput::default()
                },
                0.1,
                10.0,
            );
        }
        assert!(close(rig.position.z, 10.0), "{:?}", rig.position);
        assert!(close(rig.position.x, 0.0), "{:?}", rig.position);

        // A quarter turn to the right and the same push goes to +X.
        let mut turned = CameraRig::new(Vec3::ZERO, std::f32::consts::FRAC_PI_2, 0.0);
        for _ in 0..10 {
            turned.fly(
                &CameraInput {
                    forward: 1.0,
                    ..CameraInput::default()
                },
                0.1,
                10.0,
            );
        }
        assert!(close(turned.position.x, 10.0), "{:?}", turned.position);
        assert!(turned.position.z.abs() < 1.0e-3, "{:?}", turned.position);
    }

    #[test]
    fn strafe_is_level_and_perpendicular() {
        let rig = CameraRig::new(Vec3::ZERO, 0.7, 0.4);
        assert!(close(rig.right().y, 0.0));
        assert!(rig.forward().dot(rig.right()).abs() < 1.0e-5);
    }

    #[test]
    fn two_keys_are_not_faster_than_one() {
        let mut one = CameraRig::new(Vec3::ZERO, 0.0, 0.0);
        let mut two = one;
        one.fly(
            &CameraInput {
                forward: 1.0,
                ..CameraInput::default()
            },
            1.0,
            8.0,
        );
        two.fly(
            &CameraInput {
                forward: 1.0,
                strafe: 1.0,
                ..CameraInput::default()
            },
            1.0,
            8.0,
        );
        assert!(
            close(one.position.length(), two.position.length()),
            "{} vs {}",
            one.position.length(),
            two.position.length()
        );
    }

    #[test]
    fn pitch_cannot_pass_vertical() {
        let mut rig = CameraRig::new(Vec3::ZERO, 0.0, 0.0);
        for _ in 0..100 {
            rig.look(
                &CameraInput {
                    look_pitch: 1.0,
                    ..CameraInput::default()
                },
                0.1,
                3.0,
            );
        }
        assert!(close(rig.pitch, MAX_PITCH), "{}", rig.pitch);
        // And the right vector still exists, which is the reason for the clamp.
        assert!(rig.right().length() > 0.99);
    }

    #[test]
    fn a_step_is_frame_rate_independent() {
        let input = CameraInput {
            forward: 1.0,
            ..CameraInput::default()
        };
        let mut coarse = CameraRig::new(Vec3::ZERO, 0.3, 0.0);
        coarse.fly(&input, 0.1, 6.0);
        let mut fine = CameraRig::new(Vec3::ZERO, 0.3, 0.0);
        for _ in 0..10 {
            fine.fly(&input, 0.01, 6.0);
        }
        assert!(
            coarse.position.distance(fine.position) < 1.0e-3,
            "{} vs {}",
            coarse.position,
            fine.position
        );
    }

    #[test]
    fn a_stalled_frame_does_not_tunnel() {
        let mut rig = CameraRig::new(Vec3::ZERO, 0.0, 0.0);
        rig.fly(
            &CameraInput {
                forward: 1.0,
                ..CameraInput::default()
            },
            30.0,
            10.0,
        );
        assert!(close(rig.position.z, 1.0), "{:?}", rig.position);
    }

    #[test]
    fn a_rig_round_trips_through_a_camera() {
        let rig = CameraRig::new(Vec3::new(4.0, 2.0, -9.0), 1.1, -0.3);
        let recovered = CameraRig::from_camera(&rig.camera());
        assert!(close(recovered.yaw, rig.yaw), "{}", recovered.yaw);
        assert!(close(recovered.pitch, rig.pitch), "{}", recovered.pitch);
        assert!(recovered.position.distance(rig.position) < 1.0e-4);
        assert!(close(recovered.fov_y, rig.fov_y));
    }

    #[test]
    fn orbit_keeps_the_radius_and_faces_the_pivot() {
        let pivot = Vec3::new(3.0, 0.0, 5.0);
        let mut rig = CameraRig::default();
        rig.orbit(pivot, 0.9, 0.3, 12.0);
        assert!(close(rig.position.distance(pivot), 12.0));
        let to_pivot = (pivot - rig.position).normalize_or_zero();
        assert!(
            rig.forward().dot(to_pivot) > 0.999,
            "{}",
            rig.forward().dot(to_pivot)
        );
    }
}
