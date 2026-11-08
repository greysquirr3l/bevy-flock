//! Steering the eye.
//!
//! The demo ships five camera positions, and the point of a playground is to leave them: the
//! camera flies, `Tab` jumps between the shipped views, and every angle in here is a pure function
//! of two vectors so the mapping can be tested without a window.

use bevy_app::AppExit;
use bevy_ecs::message::{MessageReader, MessageWriter};
use bevy_ecs::prelude::*;
use bevy_flock_demo::render3d::Shot;
use bevy_input::ButtonInput;
use bevy_input::keyboard::KeyCode;
use bevy_input::mouse::{MouseButton, MouseMotion};
use bevy_math::{EulerRot, Quat, Vec3};
use bevy_time::Time;
use bevy_transform::components::Transform;

/// Metres a second at full tilt, before the sprint multiplier.
pub const BASE_SPEED: f32 = 24.0;

/// Radians of turn per pixel of mouse movement.
pub const LOOK_SENSITIVITY: f32 = 0.0022;

/// How far the eye may look up or down, radians: short of straight up, where the yaw axis degenerates.
pub const PITCH_LIMIT: f32 = 1.5;

/// A free-flying camera: `WASD`, `Q`/`E` for height, shift to sprint, right mouse to look.
#[derive(Component, Debug, Clone, Copy)]
pub struct FlyCamera {
    /// Turn about up, radians.
    pub yaw: f32,
    /// Look up or down, radians, inside [`PITCH_LIMIT`].
    pub pitch: f32,
    /// Metres a second.
    pub speed: f32,
}

impl FlyCamera {
    /// A camera at `eye` looking at `target`.
    pub fn looking_from(eye: Vec3, target: Vec3, speed: f32) -> Self {
        let (yaw, pitch) = angles_to(target - eye);
        Self { yaw, pitch, speed }
    }

    /// The direction the camera looks, unit length.
    pub fn forward(&self) -> Vec3 {
        self.rotation() * Vec3::NEG_Z
    }

    /// Right of the camera, unit length.
    pub fn right(&self) -> Vec3 {
        self.rotation() * Vec3::X
    }

    /// The camera's rotation: yaw about up, then pitch about its own right.
    pub fn rotation(&self) -> Quat {
        Quat::from_euler(EulerRot::YXZ, self.yaw, self.pitch, 0.0)
    }

    /// Turn the eye, clamping the pitch where the yaw axis stops being meaningful.
    pub fn turn(&mut self, delta: Vec3) {
        self.yaw -= delta.x * LOOK_SENSITIVITY;
        self.pitch = (self.pitch - delta.y * LOOK_SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }
}

/// The yaw and pitch that look along `offset`.
///
/// A camera looks down its own `-Z`, so the yaw is the angle that sends `-Z` to the offset's
/// horizontal direction, and the pitch is its elevation. A zero offset looks along `-Z` rather
/// than producing a `NaN` yaw: an eye that has arrived at its target is not an error.
pub fn angles_to(offset: Vec3) -> (f32, f32) {
    let horizontal = Vec3::new(offset.x, 0.0, offset.z);
    let yaw = if horizontal.length_squared() > f32::EPSILON {
        f32::atan2(-offset.x, -offset.z)
    } else {
        0.0
    };
    let length = offset.length();
    let pitch = if length > f32::EPSILON {
        (offset.y / length).clamp(-1.0, 1.0).asin()
    } else {
        0.0
    };
    (yaw, pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT))
}

/// The index after `current`, wrapping, for a list of `len` views.
pub const fn next_index(current: usize, len: usize) -> usize {
    if len == 0 { 0 } else { (current + 1) % len }
}

/// The views the demo ships, and which one the camera is at.
#[derive(Resource, Debug, Default)]
pub struct Views {
    /// Every view, in the order the demo lists them.
    pub shots: Vec<Shot>,
    /// The one the eye is at, or last jumped from.
    pub current: usize,
}

impl Views {
    /// The view after `current`, wrapping.
    pub const fn next(&self) -> usize {
        next_index(self.current, self.shots.len())
    }

    /// The view at `index`, if the demo ships that many.
    pub fn get(&self, index: usize) -> Option<&Shot> {
        self.shots.get(index)
    }
}

/// Put the eye at `eye`, looking at `target`.
pub fn place_at(camera: &mut Transform, fly: &mut FlyCamera, eye: Vec3, target: Vec3) {
    *fly = FlyCamera::looking_from(eye, target, fly.speed);
    camera.translation = eye;
    camera.rotation = fly.rotation();
}

/// Right mouse to look, keys to move.
pub fn fly(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    mut cameras: Query<(&mut Transform, &mut FlyCamera)>,
) {
    for (mut transform, mut fly) in cameras.iter_mut() {
        if buttons.pressed(MouseButton::Right) {
            for event in motion.read() {
                fly.turn(event.delta.extend(0.0));
            }
        } else {
            // Drain the buffer anyway: a motion read the moment the button comes up is a jump.
            for _ in motion.read() {}
        }
        transform.rotation = fly.rotation();

        let mut direction = Vec3::ZERO;
        if keys.pressed(KeyCode::KeyW) {
            direction += fly.forward();
        }
        if keys.pressed(KeyCode::KeyS) {
            direction -= fly.forward();
        }
        if keys.pressed(KeyCode::KeyD) {
            direction += fly.right();
        }
        if keys.pressed(KeyCode::KeyA) {
            direction -= fly.right();
        }
        if keys.pressed(KeyCode::KeyE) {
            direction += Vec3::Y;
        }
        if keys.pressed(KeyCode::KeyQ) {
            direction -= Vec3::Y;
        }
        if direction.length_squared() > 0.0 {
            let sprint = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
                3.0
            } else {
                1.0
            };
            let step = direction.normalize() * fly.speed * sprint * time.delta_secs();
            transform.translation += step;
        }
    }
}

/// `Tab` jumps to the next shipped view; `Escape` asks the app to close.
pub fn cycle(
    keys: Res<ButtonInput<KeyCode>>,
    mut views: ResMut<Views>,
    mut cameras: Query<(&mut Transform, &mut FlyCamera)>,
    mut exit: MessageWriter<AppExit>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        exit.write(AppExit::Success);
        return;
    }
    if !keys.just_pressed(KeyCode::Tab) {
        return;
    }
    let next = views.next();
    let Some(shot) = views
        .get(next)
        .map(|shot| (shot.camera.eye, shot.camera.target))
    else {
        return;
    };
    for (mut transform, mut fly) in cameras.iter_mut() {
        place_at(&mut transform, &mut fly, shot.0, shot.1);
    }
    views.current = next;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The angles from an offset put the camera's forward axis back on that offset.
    #[test]
    fn angles_and_forward_are_inverses() {
        for offset in [
            Vec3::new(0.0, 0.0, -10.0),
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(-3.0, 4.0, 7.0),
            Vec3::new(0.0, 9.0, -4.0),
        ] {
            let (yaw, pitch) = angles_to(offset);
            let camera = FlyCamera {
                yaw,
                pitch,
                speed: BASE_SPEED,
            };
            let forward = camera.forward();
            let expected = offset.normalize();
            assert!(
                forward.distance(expected) < 1e-4,
                "{offset} came back as {forward}"
            );
        }
    }

    /// The eye at rest looks along `-Z`, which is the yaw that makes a fresh camera sane.
    #[test]
    fn a_zero_offset_looks_forward_rather_than_nan() {
        let (yaw, pitch) = angles_to(Vec3::ZERO);
        assert_eq!((yaw, pitch), (0.0, 0.0));
        let camera = FlyCamera {
            yaw,
            pitch,
            speed: BASE_SPEED,
        };
        assert!(camera.forward().distance(Vec3::NEG_Z) < 1e-6);
    }

    /// Forward and right stay unit length and square to each other at every angle.
    #[test]
    fn the_basis_stays_orthonormal() {
        for yaw in [-3.0, -1.0, 0.0, 1.0, 3.0] {
            for pitch in [-PITCH_LIMIT, -0.5, 0.0, 0.5, PITCH_LIMIT] {
                let camera = FlyCamera {
                    yaw,
                    pitch,
                    speed: BASE_SPEED,
                };
                let forward = camera.forward();
                let right = camera.right();
                assert!((forward.length() - 1.0).abs() < 1e-5);
                assert!((right.length() - 1.0).abs() < 1e-5);
                assert!(forward.dot(right).abs() < 1e-5);
                // A camera looking level keeps its right axis level too.
                if pitch == 0.0 {
                    assert!(right.y.abs() < 1e-5);
                }
            }
        }
    }

    /// The pitch clamps rather than flipping the world upside down at the poles.
    #[test]
    fn the_pitch_clamps() {
        let mut camera = FlyCamera {
            yaw: 0.0,
            pitch: 0.0,
            speed: BASE_SPEED,
        };
        for _ in 0..100 {
            camera.turn(Vec3::new(0.0, 10.0, 0.0));
        }
        assert!((camera.pitch + PITCH_LIMIT).abs() < 1e-6);
        for _ in 0..200 {
            camera.turn(Vec3::new(0.0, -10.0, 0.0));
        }
        assert!((camera.pitch - PITCH_LIMIT).abs() < 1e-6);
        // Yaw is unbounded: turning past a full circle is a turn, not a clamp.
        for _ in 0..500 {
            camera.turn(Vec3::new(10.0, 0.0, 0.0));
        }
        assert!(camera.yaw.abs() > 6.0);
    }

    /// `Tab` walks the shipped views and wraps, which is the whole of the cycling arithmetic.
    #[test]
    fn the_views_cycle_and_wrap() {
        assert_eq!(next_index(0, 0), 0, "no views is not a division by zero");
        assert_eq!(next_index(0, 5), 1);
        assert_eq!(next_index(4, 5), 0);
        assert_eq!(next_index(0, 1), 0);
    }

    /// Placing the eye at a view puts it there and turns it at the target, keeping its speed.
    #[test]
    fn placing_the_eye_moves_and_turns_it() {
        let eye = Vec3::new(12.0, 6.0, -30.0);
        let target = Vec3::new(0.0, 2.0, 0.0);
        let mut transform = Transform::from_xyz(0.0, 0.0, 0.0);
        let mut fly = FlyCamera {
            yaw: 0.0,
            pitch: 0.0,
            speed: 77.0,
        };
        place_at(&mut transform, &mut fly, eye, target);
        assert_eq!(transform.translation, eye);
        assert!(
            (fly.speed - 77.0).abs() < 1e-6,
            "placing does not reset the speed"
        );
        let wanted = (target - eye).normalize();
        assert!(fly.forward().distance(wanted) < 1e-4);
        assert_eq!(transform.rotation, fly.rotation());
    }
}
