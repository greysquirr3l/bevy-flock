//! What an interactive session knows, with no terminal in sight.
//!
//! Everything the playground does between a keystroke and a frame lives here: which view is loaded,
//! where the camera has flown to, how fast it is going, whether the scripted flight has taken over.
//! The binary that owns the terminal drains a byte stream, decodes it, and calls [`Session::apply`]
//! and [`Session::advance`]; that is the whole coupling, which is why every rule below can be tested
//! without a tty and without waiting for a person to press something.

use crate::camera_rig::{CameraInput, CameraRig, MAX_STEP};
use crate::input::Command;
use crate::playback;
use crate::raster::Camera;
use crate::render3d::Shot;

/// How far a speed change moves the flying speed: double or halve, bounded at both ends.
pub const SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.5..=256.0;

/// Metres per second at the start.
pub const START_SPEED: f32 = 20.0;

/// Frames of a scripted loop, per view, when the player asks to watch instead of fly.
pub const LOOP_STEPS: usize = 24;

/// How the interactive picture is drawn: fewer bodies, shorter distances, impostors past a few
/// metres, and a cross-fade across the distance where they take over.
///
/// The shipped PNGs are read at leisure and made at full detail; a terminal picture is steered at
/// twenty frames a second and is 120 pixels wide. Measured on the district view at 120 by 64 pixels,
/// the full budget costs about 1,100 ms a frame and this one costs about 25, which is the difference
/// between a still and a camera. See `docs/performance.md`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    /// Bodies drawn, nearest first.
    pub max_bodies: usize,
    /// Bodies within this radius, metres.
    pub draw_distance: f32,
    /// Ruins and props within this radius, metres.
    pub block_distance: f32,
    /// Smallest triangle worth shading, pixels.
    pub min_area: f32,
    /// Beyond this distance a body is a billboard, metres.
    pub impostor_distance: f32,
    /// How far below that the billboard fades in, metres.
    ///
    /// Eight metres at a twenty-four metre switch is a third of the switch: a body walks through the
    /// band in well under a second at a stroll, which is the window a pop has to hide in. It costs the
    /// mesh and the quad for every body inside the band, so it is a knob and not a constant, and the
    /// five shipped views leave it at zero.
    pub impostor_band: f32,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_bodies: 420,
            draw_distance: 240.0,
            block_distance: 300.0,
            min_area: 0.75,
            impostor_distance: 24.0,
            impostor_band: 8.0,
        }
    }
}

impl Budget {
    /// Apply this budget to a shot.
    #[must_use]
    pub fn apply(&self, shot: &Shot) -> Shot {
        Shot {
            max_bodies: self.max_bodies,
            draw_distance: self.draw_distance,
            block_distance: self.block_distance,
            min_area: self.min_area,
            impostor_distance: self.impostor_distance,
            impostor_band: self.impostor_band,
            ..shot.clone()
        }
    }
}

/// An interactive session: a camera, a view, and the state of the flight.
#[derive(Debug, Clone)]
pub struct Session {
    /// The camera being flown.
    pub rig: CameraRig,
    /// Which of the shipped views the camera started from, counting from zero.
    pub view: usize,
    /// Metres per second at full deflection.
    pub speed: f32,
    /// Whether the scripted flight is driving instead of the keyboard.
    pub playback: bool,
    /// Whether the help page is showing.
    pub help: bool,
    /// Where the scripted flight is, in frames.
    pub cursor: usize,
    /// The scripted flight's cameras, built once.
    pub flight: Vec<(usize, Camera)>,
    /// How the picture is drawn.
    pub budget: Budget,
}

impl Session {
    /// Start on a view, framed exactly as the renderer's own shot frames it.
    #[must_use]
    pub fn new(shots: &[Shot], view: usize) -> Self {
        let index = view.min(shots.len().saturating_sub(1));
        let rig = shots.get(index).map_or_else(CameraRig::default, |shot| {
            CameraRig::from_camera(&shot.camera)
        });
        Self {
            rig,
            view: index,
            speed: START_SPEED,
            playback: false,
            help: false,
            cursor: 0,
            flight: playback::cameras(shots, LOOP_STEPS),
            budget: Budget::default(),
        }
    }

    /// The name of the loaded view, for the status line.
    #[must_use]
    pub fn view_name(&self, shots: &[Shot]) -> &'static str {
        shots.get(self.view).map_or("free", |shot| shot.name)
    }

    /// Apply a one-shot command.
    pub fn apply(&mut self, command: Command, shots: &[Shot]) {
        match command {
            Command::View(number) => {
                // One-based on the keyboard, zero-based here, and clamped: pressing 9 on a five-view
                // flight shows the last view rather than nothing.
                if number > 0 {
                    let index = (number - 1).min(shots.len().saturating_sub(1));
                    if let Some(shot) = shots.get(index) {
                        self.rig = CameraRig::from_camera(&shot.camera);
                        self.view = index;
                    }
                }
            }
            Command::Playback => {
                self.playback = !self.playback;
                // Joining the flight mid-way should show the flight, not a jump back to its start.
                self.cursor = if self.playback { self.cursor } else { 0 };
            }
            Command::Faster => {
                self.speed = (self.speed * 2.0).clamp(*SPEED_RANGE.start(), *SPEED_RANGE.end());
            }
            Command::Slower => {
                self.speed = (self.speed * 0.5).clamp(*SPEED_RANGE.start(), *SPEED_RANGE.end());
            }
            Command::ZoomIn => self.rig.fov_y = (self.rig.fov_y - 0.1).max(0.25),
            Command::ZoomOut => self.rig.fov_y = (self.rig.fov_y + 0.1).min(2.4),
            Command::Help => self.help = !self.help,
            Command::Quit => {}
        }
    }

    /// Advance the session by `step` seconds, given the axes the controls report.
    pub fn advance(&mut self, input: &CameraInput, step: f32) {
        if self.playback {
            self.advance_flight();
            return;
        }
        let step = step.clamp(0.0, MAX_STEP);
        if input.is_idle() {
            return;
        }
        self.rig.fly(input, step, self.speed);
        self.rig.look(input, step, crate::input::LOOK_RATE);
    }

    /// Move one frame along the scripted flight.
    ///
    /// Paced by frames rather than by the clock: a slow machine plays the loop slowly instead of
    /// skipping half of it, which is what a person watching wants.
    fn advance_flight(&mut self) {
        if self.flight.is_empty() {
            return;
        }
        self.cursor = (self.cursor + 1) % self.flight.len();
        if let Some((index, camera)) = self.flight.get(self.cursor) {
            self.view = *index;
            self.rig = CameraRig::from_camera(camera);
        }
    }

    /// The camera to render with.
    #[must_use]
    pub fn camera(&self) -> Camera {
        self.rig.camera()
    }

    /// A shot carrying the flown camera and the lighting of the nearest shipped view.
    ///
    /// The lighting travels with the view rather than with the camera: a flight out of the street
    /// fog and into the district's haze would otherwise take the fog with it and wash the ruins out.
    #[must_use]
    pub fn shot(&self, shots: &[Shot]) -> Option<Shot> {
        let base = self.nearest(shots)?;
        let mut shot = self.budget.apply(base);
        shot.camera = self.camera();
        Some(shot)
    }

    /// The shipped view whose eye is closest to the camera, which is the one whose lighting suits.
    #[must_use]
    fn nearest<'a>(&self, shots: &'a [Shot]) -> Option<&'a Shot> {
        shots.iter().min_by(|left, right| {
            let left = left.camera.eye.distance(self.rig.position);
            let right = right.camera.eye.distance(self.rig.position);
            left.total_cmp(&right)
        })
    }

    /// The line under the picture.
    #[must_use]
    pub fn status(&self, shots: &[Shot], ms: f64, fps: f64) -> String {
        let position = self.rig.position;
        let mode = if self.playback { "playback" } else { "free" };
        format!(
            " {mode} · view {}/{} {} · {ms:.0} ms {fps:.0} fps · {} m/s · x {:.0} y {:.0} z {:.0} · h help q quit ",
            self.view + 1,
            shots.len(),
            self.view_name(shots),
            self.speed,
            position.x,
            position.y,
            position.z,
        )
    }

    /// The help page, one line per key.
    #[must_use]
    pub fn help_text() -> [&'static str; 8] {
        [
            " wasd move · e climb · c descend · arrows look",
            " 1-5 jump to a view · p watch the scripted flight",
            " [ ] widen or narrow the lens · + - faster or slower",
            " h toggle this page · q quit",
            "",
            " The picture is the software rasteriser from raster.rs: no GPU,",
            " no window. Every frame is the same code that writes the PNGs,",
            " so what you fly is what the artefacts show.",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{sample_scene, sample_shots};
    use bevy_flock::testkit::OrDie;
    use bevy_math::Vec3;

    fn views() -> Vec<Shot> {
        let (city, bodies) = sample_scene();
        sample_shots(&city, &bodies)
    }

    #[test]
    fn a_session_starts_on_the_framed_view() {
        let shots = views();
        let session = Session::new(&shots, 2);
        assert_eq!(session.view, 2);
        assert!(
            session
                .rig
                .position
                .distance(shots.get(2).or_die().camera.eye)
                < 1.0e-4
        );
        assert!((session.rig.fov_y - shots.get(2).or_die().camera.fov_y).abs() < 1.0e-4);
    }

    #[test]
    fn view_keys_are_one_based_and_clamped() {
        let shots = views();
        let mut session = Session::new(&shots, 0);
        session.apply(Command::View(3), &shots);
        assert_eq!(session.view, 2);
        session.apply(Command::View(9), &shots);
        assert_eq!(session.view, shots.len() - 1);
        session.apply(Command::View(0), &shots);
        assert_eq!(session.view, shots.len() - 1, "zero is not a view");
    }

    #[test]
    fn speed_doubles_and_halves_within_bounds() {
        let shots = views();
        let mut session = Session::new(&shots, 0);
        session.apply(Command::Faster, &shots);
        assert!((session.speed - START_SPEED * 2.0).abs() < f32::EPSILON);
        for _ in 0..40 {
            session.apply(Command::Slower, &shots);
        }
        assert!((session.speed - *SPEED_RANGE.start()).abs() < f32::EPSILON);
        for _ in 0..40 {
            session.apply(Command::Faster, &shots);
        }
        assert!((session.speed - *SPEED_RANGE.end()).abs() < f32::EPSILON);
    }

    #[test]
    fn the_lens_stays_sane() {
        let shots = views();
        let mut session = Session::new(&shots, 0);
        for _ in 0..40 {
            session.apply(Command::ZoomIn, &shots);
        }
        assert!(session.rig.fov_y >= 0.25);
        for _ in 0..80 {
            session.apply(Command::ZoomOut, &shots);
        }
        assert!(session.rig.fov_y <= 2.4);
    }

    #[test]
    fn playback_walks_the_flight_and_wraps() {
        let shots = views();
        let mut session = Session::new(&shots, 0);
        session.apply(Command::Playback, &shots);
        assert!(session.playback);
        let length = session.flight.len();
        assert_eq!(length, shots.len() * LOOP_STEPS);
        for _ in 0..length {
            session.advance(&CameraInput::default(), 0.016);
        }
        assert_eq!(session.cursor, 0, "the loop must close");
        session.apply(Command::Playback, &shots);
        assert!(!session.playback);
        assert_eq!(session.cursor, 0);
    }

    #[test]
    fn free_flight_ignores_the_flight_cursor() {
        let shots = views();
        let mut session = Session::new(&shots, 0);
        let before = session.rig.position;
        for _ in 0..100 {
            session.advance(
                &CameraInput {
                    forward: 1.0,
                    ..CameraInput::default()
                },
                0.016,
            );
        }
        assert!(session.rig.position.distance(before) > 1.0);
        assert_eq!(session.cursor, 0);
    }

    #[test]
    fn a_long_session_stays_finite() {
        let shots = views();
        let mut session = Session::new(&shots, 0);
        // Ten thousand steps with every axis at full deflection: yaw wraps, pitch clamps, and the
        // position must stay a number a rasteriser can project.
        let input = CameraInput {
            forward: 1.0,
            strafe: -1.0,
            rise: 1.0,
            look_yaw: 1.0,
            look_pitch: -1.0,
            zoom: 0.0,
        };
        for _ in 0..10_000 {
            session.advance(&input, 0.016);
        }
        let position = session.rig.position;
        assert!(position.is_finite(), "{position}");
        assert!(session.rig.pitch.abs() <= crate::camera_rig::MAX_PITCH + 1.0e-5);
        assert!(session.rig.yaw >= 0.0 && session.rig.yaw < std::f32::consts::TAU);
        assert!(session.camera().target.is_finite());
    }

    #[test]
    fn the_shot_borrows_the_nearest_lighting() {
        let shots = views();
        let mut session = Session::new(&shots, 0);
        let shot = session.shot(&shots).or_die();
        assert_eq!(shot.name, shots.first().or_die().name);
        assert!(shot.camera.eye.distance(session.rig.position) < 1.0e-4);
        // Fly to the far side of the city: the lighting should follow the nearest view, not stay.
        session.rig.position = shots.get(3).or_die().camera.eye + Vec3::new(1.0, 0.0, 0.0);
        let moved = session.shot(&shots).or_die();
        assert_eq!(moved.name, shots.get(3).or_die().name);
    }

    #[test]
    fn the_status_line_names_the_mode_and_the_view() {
        let shots = views();
        let session = Session::new(&shots, 1);
        let status = session.status(&shots, 42.0, 24.0);
        assert!(status.contains("free"));
        assert!(status.contains(&format!("view 2/{}", shots.len())));
        assert!(status.contains("42 ms"));
    }
}
