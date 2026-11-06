//! The demo's own guarantees, tested without a window.
//!
//! Three claims are worth a test rather than a paragraph: the city is deterministic, the artefacts
//! are a pure function of the simulation, and the shipped assets are the ones the scene names. The
//! first two are checked by building the same thing twice and comparing bytes; the third by
//! reading every file the manifest promises.

use bevy_flock::domain::ids::SimSeed;
use bevy_flock::domain::temperament::Archetype;
use bevy_flock::testkit::OrDie;
use bevy_flock_demo::app::Playground;
use bevy_flock_demo::assets;
use bevy_flock_demo::city::CityLayout;
use bevy_flock_demo::plan;
use bevy_flock_demo::scene::{self, Body, Scene};
use bevy_math::Vec3;

const EXTENT: f32 = 700.0;
const POPULATION: u32 = 400;
const TICKS: u64 = 30;
/// The walk test needs seconds, not ticks: at 1/60 s per tick, 30 ticks is half a second and a
/// person has barely leaned forward.
const WALK_TICKS: u64 = 300;

/// A run at test scale, through exactly the code the binary runs.
fn played(seed: u64) -> Playground {
    let mut playground = Playground::new(seed, POPULATION, EXTENT).or_die();
    playground.drain_spawn();
    playground.place_crowd();
    playground.tick(TICKS);
    playground
}

#[test]
fn two_runs_of_the_same_seed_produce_the_same_digest() {
    let first = played(7).diagnostics().digest();
    let second = played(7).diagnostics().digest();
    assert_eq!(
        first, second,
        "the simulation is not reproducible at {POPULATION} NPCs"
    );
}

#[test]
fn the_same_seed_places_the_same_crowd() {
    let mut first = played(7);
    let one = scene::crowd_fingerprint(&first.bodies());
    let mut second = played(7);
    let two = scene::crowd_fingerprint(&second.bodies());
    assert_eq!(one, two, "the same seed produced a different crowd");
}

#[test]
fn a_different_seed_places_a_different_crowd() {
    // The library's `digest` will happily match across seeds: it covers population, tiers, mood and
    // the tick, none of which depend on where anyone stands, and that coarseness is deliberate (it
    // is what stops a one-ulp transcendental difference reading as non-determinism). Proving two
    // cities differ therefore needs the positional fingerprint, not the digest.
    let mut first = played(7);
    let one = scene::crowd_fingerprint(&first.bodies());
    let mut second = played(9);
    let two = scene::crowd_fingerprint(&second.bodies());
    assert_ne!(one, two, "two seeds produced the same crowd positions");
}

#[test]
fn the_crowd_walks() {
    // Without a target the steering solver's fixed point is "everyone standing still", which is how
    // this test found a real bug: the crowd was placed, and then never moved again.
    let mut playground = Playground::new(7, POPULATION, EXTENT).or_die();
    playground.drain_spawn();
    playground.place_crowd();
    let started = playground.bodies();
    playground.tick(WALK_TICKS);
    let ended = playground.bodies();
    let moved = started
        .iter()
        .zip(&ended)
        .filter(|(before, after)| before.position.distance(after.position) > 0.5)
        .count();
    assert!(
        moved as f32 / started.len() as f32 > 0.5,
        "only {moved} of {} NPCs moved in {WALK_TICKS} ticks",
        started.len()
    );
}

#[test]
fn the_crowd_lands_in_the_settlements() {
    let mut playground = played(7);
    let bodies = playground.bodies();
    let city = CityLayout::generate(SimSeed::new(7), EXTENT);
    assert_eq!(bodies.len(), POPULATION as usize);
    let inside = bodies
        .iter()
        .filter(|body| {
            city.districts
                .iter()
                .any(|district| body.position.distance(district.centre) <= district.radius * 1.2)
        })
        .count();
    // The crowd walks, so some agents will have left their district by the end of the run; what
    // must hold is that the city, not the empty ruins, is where nearly all of them are.
    assert!(
        inside as f32 / bodies.len() as f32 > 0.85,
        "only {inside} of {} NPCs are near a settlement",
        bodies.len()
    );
}

#[test]
fn the_artefacts_are_a_function_of_the_run() {
    let render = |seed: u64| {
        let mut playground = played(seed);
        let city = playground.city.clone();
        let digest = playground.diagnostics().digest();
        let bodies = playground.bodies();
        let scene = Scene::build(&city, &bodies);
        (
            scene.to_json(&city, digest),
            plan::render(&city, &bodies, digest, TICKS),
        )
    };
    let (first_scene, first_plan) = render(7);
    let (second_scene, second_plan) = render(7);
    assert!(
        first_scene == second_scene,
        "the scene changed between runs"
    );
    assert!(first_plan == second_plan, "the plan changed between runs");
}

#[test]
fn every_archetype_maps_onto_a_shipped_body() {
    for archetype in Archetype::ALL {
        let slug = assets::body_for(archetype);
        let model = assets::model(slug).or_die();
        assert_eq!(model.kind, assets::ModelKind::Body, "{slug} is not a body");
    }
}

#[test]
fn the_scene_names_only_assets_that_exist() {
    let city = CityLayout::generate(SimSeed::new(1), EXTENT);
    let bodies = [
        Body {
            archetype: Archetype::Guard,
            position: Vec3::ZERO,
            yaw: 0.0,
            appearance_seed: 1,
        },
        Body {
            archetype: Archetype::Merchant,
            position: Vec3::X,
            yaw: 0.0,
            appearance_seed: 2,
        },
        Body {
            archetype: Archetype::Urchin,
            position: Vec3::Z,
            yaw: 0.0,
            appearance_seed: 3,
        },
    ];
    let scene = Scene::build(&city, &bodies);
    let document = scene.to_json(&city, 0);
    for (slug, instances) in scene.groups() {
        if instances.is_empty() {
            continue;
        }
        let model = assets::model(slug).or_die();
        assert!(
            document.contains(model.file),
            "{} is drawn but unnamed",
            model.slug
        );
        let path = assets::root().join(model.file);
        let summary = assets::inspect(&path).or_die();
        assert!(summary.has_mesh, "{} has no geometry", model.slug);
    }
}
