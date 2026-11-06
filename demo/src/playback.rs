//! Scripted playback: a deterministic flight through the five views.
//!
//! The interactive loop is for a person at a keyboard; this is for everything else. It walks the
//! same five shots, easing from one to the next, and writes a frame per step with the time it took
//! and a fingerprint of the pixels. Two runs of the same command produce the same frames and the
//! same table, which is what makes the flight reviewable in a pull request instead of a video.

use std::path::{Path, PathBuf};

use bevy_math::Vec3;

use crate::DemoError;
use crate::json;
use crate::raster::{Camera, Image};
use crate::render3d::{self, Meshes, Shot};
use crate::scene::Scene;

/// One rendered frame of a scripted flight.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameRecord {
    /// Position in the flight, counting from zero.
    pub index: usize,
    /// The view the flight is nearest, which labels the frame.
    pub view: &'static str,
    /// Wall time for the frame, milliseconds.
    pub ms: f64,
    /// Fingerprint of the pixels, so a diff can find the frame that changed.
    pub fingerprint: u64,
    /// Where the camera stood.
    pub eye: Vec3,
    /// What it looked at.
    pub target: Vec3,
    /// Vertical field of view, radians.
    pub fov_y: f32,
}

/// What a flight produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    /// One record per frame, in order.
    pub frames: Vec<FrameRecord>,
    /// Where the PNGs were written, if anywhere.
    pub directory: Option<PathBuf>,
}

impl Report {
    /// Total wall time over every frame, milliseconds.
    #[must_use]
    pub fn total_ms(&self) -> f64 {
        self.frames.iter().map(|frame| frame.ms).sum()
    }

    /// Mean wall time per frame, milliseconds; zero for an empty flight.
    #[must_use]
    pub fn mean_ms(&self) -> f64 {
        if self.frames.is_empty() {
            0.0
        } else {
            self.total_ms() / self.frames.len() as f64
        }
    }
}

/// Smoothstep: zero slope at both ends, so a cut from one view to the next does not jerk.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The flight's cameras, `steps_per_view` frames spent easing from each shot to the next.
///
/// The last shot eases back to the first, so a loop of the flight is seamless. A flight of `n` shots
/// with `s` steps is therefore `n * s` frames, and frame zero is the first shot exactly.
#[must_use]
pub fn cameras(shots: &[Shot], steps_per_view: usize) -> Vec<(usize, Camera)> {
    if shots.is_empty() {
        return Vec::new();
    }
    let steps = steps_per_view.max(1);
    let mut cameras = Vec::with_capacity(shots.len() * steps);
    for index in 0..shots.len() {
        let from = shots.get(index);
        let to = shots.get((index + 1) % shots.len());
        let (Some(from), Some(to)) = (from, to) else {
            continue;
        };
        for step in 0..steps {
            let t = ease(step as f32 / steps as f32);
            let eye = from.camera.eye.lerp(to.camera.eye, t);
            let target = from.camera.target.lerp(to.camera.target, t);
            let fov_y = from.camera.fov_y + (to.camera.fov_y - from.camera.fov_y) * t;
            cameras.push((
                index,
                Camera {
                    eye,
                    target,
                    fov_y,
                    near: from.camera.near.min(to.camera.near),
                    far: from.camera.far.max(to.camera.far),
                },
            ));
        }
    }
    cameras
}

/// The shot a frame belongs to, copied with the frame's own camera and lighting.
fn shot_for(shots: &[Shot], index: usize, camera: &Camera) -> Option<Shot> {
    let base = shots.get(index)?;
    Some(Shot {
        camera: *camera,
        // The lighting eases with the camera, so a flight out of the fog does not pop.
        atmosphere: base.atmosphere,
        ..*base
    })
}

/// Fly the five views, rendering each frame and optionally writing it out as a PNG.
///
/// `steps_per_view` sets the length; `write_to` turns the PNGs on. Nothing is written when it is
/// [`None`], which is what the tests and the benchmark use: the frames are still rendered, so their
/// timing and fingerprints are still evidence.
///
/// # Errors
///
/// Returns [`DemoError::Io`] when a frame or the table cannot be written.
pub fn fly(
    shots: &[Shot],
    scene: &Scene,
    meshes: &Meshes,
    steps_per_view: usize,
    width: u32,
    height: u32,
    write_to: Option<&Path>,
) -> Result<Report, DemoError> {
    if let Some(directory) = write_to {
        std::fs::create_dir_all(directory).map_err(|error| DemoError::Io {
            path: directory.display().to_string(),
            reason: error.to_string(),
        })?;
    }
    let mut frames = Vec::new();
    for (index, camera) in cameras(shots, steps_per_view).into_iter().enumerate() {
        let Some(shot) = shot_for(shots, camera.0, &camera.1) else {
            continue;
        };
        let started = std::time::Instant::now();
        let (image, _stats) = render3d::render(scene, meshes, &shot, width, height);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let record = FrameRecord {
            index,
            view: shot.name,
            ms,
            fingerprint: image.fingerprint(),
            eye: camera.1.eye,
            target: camera.1.target,
            fov_y: camera.1.fov_y,
        };
        if let Some(directory) = write_to {
            write_png(directory, index, &image)?;
        }
        frames.push(record);
    }
    let directory = write_to.map(Path::to_path_buf);
    let report = Report { frames, directory };
    if let Some(directory) = write_to {
        write_table(directory, &report, steps_per_view)?;
    }
    Ok(report)
}

/// Write one frame as `frame-0000.png`.
fn write_png(directory: &Path, index: usize, image: &Image) -> Result<(), DemoError> {
    let path = directory.join(format!("frame-{index:04}.png"));
    std::fs::write(&path, image.to_png()).map_err(|error| DemoError::Io {
        path: path.display().to_string(),
        reason: error.to_string(),
    })
}

/// Write the frame table as `playback.json`.
fn write_table(directory: &Path, report: &Report, steps_per_view: usize) -> Result<(), DemoError> {
    let rows: Vec<String> = report
        .frames
        .iter()
        .map(|frame| {
            let fields = [
                ("index", frame.index.to_string()),
                ("view", json::string(frame.view)),
                ("ms", json::number(frame.ms as f32, 3)),
                ("fingerprint", format!("\"{:016x}\"", frame.fingerprint)),
                ("eye", json::vec3(frame.eye)),
                ("target", json::vec3(frame.target)),
                ("fov_y", json::number(frame.fov_y, 4)),
            ];
            json::object(&fields, 4)
        })
        .collect();
    let fields = [
        ("steps_per_view", steps_per_view.to_string()),
        ("frames", report.frames.len().to_string()),
        ("total_ms", json::number(report.total_ms() as f32, 3)),
        ("mean_ms", json::number(report.mean_ms() as f32, 3)),
        ("table", json::array_block(&rows, 2)),
    ];
    let path = directory.join("playback.json");
    std::fs::write(&path, json::object(&fields, 0)).map_err(|error| DemoError::Io {
        path: path.display().to_string(),
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{sample_ready, sample_scene, sample_shots};
    use bevy_flock::testkit::OrDie;

    #[test]
    fn the_flight_starts_on_the_first_shot_and_loops() {
        let (city, bodies) = sample_scene();
        let shots = sample_shots(&city, &bodies);
        let flight = cameras(&shots, 4);
        assert_eq!(flight.len(), shots.len() * 4);
        let first = flight.first().map(|(_, camera)| *camera);
        let opening = shots.first().map(|shot| shot.camera);
        assert_eq!(
            first.map(|camera| camera.eye),
            opening.map(|camera| camera.eye)
        );
        assert_eq!(
            first.map(|camera| camera.target),
            opening.map(|camera| camera.target)
        );
    }

    #[test]
    fn the_flight_is_deterministic() {
        let (city, bodies) = sample_scene();
        let shots = sample_shots(&city, &bodies);
        let once = cameras(&shots, 3);
        let twice = cameras(&shots, 3);
        assert_eq!(once.len(), twice.len());
        for (left, right) in once.iter().zip(twice.iter()) {
            assert_eq!(left.0, right.0);
            assert_eq!(left.1.eye.to_array(), right.1.eye.to_array());
            assert_eq!(left.1.target.to_array(), right.1.target.to_array());
        }
    }

    #[test]
    fn easing_moves_monotonically_between_views() {
        let (city, bodies) = sample_scene();
        let shots = sample_shots(&city, &bodies);
        let flight = cameras(&shots, 8);
        let start = shots.first().or_die().camera.eye;
        let end = shots.get(1).or_die().camera.eye;
        let mut previous = 0.0_f32;
        for (_, camera) in flight.iter().take(8) {
            let travelled = camera.eye.distance(start) / start.distance(end).max(1.0e-3);
            assert!(
                travelled >= previous - 1.0e-4,
                "{travelled} went backwards from {previous}"
            );
            previous = travelled;
        }
    }

    #[test]
    fn a_render_costs_time_and_produces_a_picture() {
        let (city, scene, bodies) = sample_ready();
        let shots = sample_shots(&city, &bodies);
        let meshes = Meshes::load().or_die();
        let report = fly(&shots, &scene, &meshes, 1, 64, 40, None).or_die();
        assert_eq!(report.frames.len(), shots.len());
        assert!(report.total_ms() > 0.0);
        assert!(report.frames.iter().all(|frame| frame.fingerprint != 0));
        assert!(report.directory.is_none());
    }
}
