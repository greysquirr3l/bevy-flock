//! Fixtures the unit tests share.
//!
//! Two modules need the same small city and the same handful of bodies: `render3d` frames it, and
//! `playback` flies through it. Building it twice would let the two drift, and the drift would show
//! up as a test that passes on a scene nothing else uses.

use bevy_flock::domain::ids::SimSeed;
use bevy_flock::domain::temperament::Archetype;
use bevy_math::Vec3;

use crate::city::CityLayout;
use crate::render3d::{self, Shot};
use crate::scene::{Body, Scene};

/// A small settlement and the bodies standing in it.
///
/// Not the demo's own city: a test that depends on the shipped layout breaks when somebody moves a
/// district, and this only needs something with rubble and a crowd in it.
#[must_use]
pub fn sample_scene() -> (CityLayout, Vec<Body>) {
    let city = CityLayout::generate(SimSeed::new(7), 900.0);
    let centre = city
        .districts
        .iter()
        .map(|district| district.centre)
        .next()
        .unwrap_or(Vec3::ZERO);
    let bodies: Vec<Body> = (0..24_u64)
        .map(|index| Body {
            archetype: Archetype::Urchin,
            position: centre + Vec3::new((index % 6) as f32 * 3.0 - 9.0, 0.0, -8.0),
            yaw: 0.0,
            appearance_seed: index,
        })
        .collect();
    (city, bodies)
}

/// The five views, framed on [`sample_scene`].
#[must_use]
pub fn sample_shots(city: &CityLayout, bodies: &[Body]) -> Vec<Shot> {
    render3d::shots(city, bodies)
}

/// A built scene for the sample bodies, for the tests that need to draw rather than to frame.
#[must_use]
pub fn sample_ready() -> (CityLayout, Scene, Vec<Body>) {
    let (city, bodies) = sample_scene();
    let scene = Scene::build(&city, &bodies);
    (city, scene, bodies)
}
