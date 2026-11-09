//! What the LOD band saves, measured rather than asserted.
//!
//! Builds two worlds with the same crowd in the same places — one whose profile culls, one whose
//! profile never does — and times the binding system over both. The camera sits at the edge of the
//! plaza, where a player watching a crowd would be, so the far half is stepped or paused.
//!
//! Run it with `cargo run -p bevy-flock-anim --release --example lod_cost`.
//!
//! The number printed is the binding's own cost, not the frame: Bevy's `animate_targets` runs
//! outside this system and is not part of the measurement. What a paused body saves in bones is
//! real, but invisible here, so this is a floor on the saving rather than a ceiling.
use std::time::Instant;

use bevy_animation::AnimationPlayer;
use bevy_animation::graph::AnimationNodeIndex;
use bevy_asset::Assets;
use bevy_ecs::prelude::World;
use bevy_ecs::schedule::Schedule;
use bevy_flock::adapters::primary::cognition::NpcTier;
use bevy_flock::application::SimClock;
use bevy_flock::domain::cognition::CognitionTier;
use bevy_flock::domain::num;
use bevy_flock::prelude::Locomotion;
use bevy_math::{Vec2, Vec3};
use bevy_transform::prelude::Transform;

use bevy_flock_anim::{
    AnimationFocus, AnimationLod, BlendLod, ClipBindings, LocomotionProfile, LodLevel,
    apply_locomotion_blend,
};

/// The plaza the crowd stands in, in metres.
const PLAZA_RADIUS: f32 = 60.0;

/// How many passes each world is timed over, after a warm-up.
const PASSES: u32 = 240;

/// How many passes run before the clock starts, so the players' maps and the LOD components exist.
const WARM_UP: u32 = 32;

/// The populations the task asks about.
const SIZES: [usize; 2] = [2_000, 10_000];

/// A world with `count` bodies in a plaza, the binding scheduled, and a profile with `lod`.
fn crowd(count: usize, lod: BlendLod, focus: AnimationFocus) -> (World, Schedule) {
    let mut world = World::new();
    let mut profiles = Assets::<LocomotionProfile>::default();
    // The city-crowd numbers this crate's documentation uses: a 1.4 m/s walk, a 5 m/s run, a 1.5 m
    // stride.
    let Ok(profile) = LocomotionProfile::try_new(1.4, 5.0, 1.5, 0.5, 2.0) else {
        unreachable!("the example's profile is valid")
    };
    let handle = profiles.add(profile.with_lod(lod));
    world.insert_resource(profiles);
    world.insert_resource(focus);
    world.insert_resource(SimClock::default());

    let bindings = ClipBindings::new(
        handle,
        AnimationNodeIndex::new(0),
        AnimationNodeIndex::new(1),
        AnimationNodeIndex::new(2),
    );
    for index in 0..count {
        let (position, velocity) = place(index, count);
        let mut motion = Locomotion::default();
        // A body that has already been walking for half a second has a non-zero phase, so the
        // measurement includes the steady state rather than a crowd that just started moving.
        for _ in 0..30 {
            motion.advance(velocity, velocity, 1.0 / 60.0);
        }
        world.spawn((
            motion,
            Transform::from_translation(position),
            bindings.clone(),
            AnimationPlayer::default(),
            // Every body is cheap-tier: the crowd nobody is talking to, which is what a plaza is.
            NpcTier(CognitionTier::Reflex),
        ));
    }

    let mut schedule = Schedule::default();
    schedule.add_systems(apply_locomotion_blend);
    if schedule.initialize(&mut world).is_err() {
        unreachable!("a single-system schedule always builds");
    }
    (world, schedule)
}

/// A deterministic position on a golden-angle spiral, and the velocity of somebody walking across it.
fn place(index: usize, count: usize) -> (Vec3, Vec2) {
    let fraction = if count == 0 {
        0.0
    } else {
        num::usize_to_f32(index) / num::usize_to_f32(count)
    };
    let radius = PLAZA_RADIUS * fraction.sqrt();
    let angle = fraction * 2.0 * std::f32::consts::TAU * 7.0;
    let (sin, cos) = angle.sin_cos();
    (
        Vec3::new(radius * cos, 0.0, radius * sin),
        Vec2::new(-sin, cos) * 1.2,
    )
}

/// Time `PASSES` runs of the binding, in milliseconds per pass.
fn time(world: &mut World, schedule: &mut Schedule) -> f64 {
    for _ in 0..WARM_UP {
        advance(world);
        schedule.run(world);
    }
    let start = Instant::now();
    for _ in 0..PASSES {
        advance(world);
        schedule.run(world);
    }
    let elapsed = start.elapsed();
    elapsed.as_secs_f64() * 1_000.0 / f64::from(PASSES)
}

fn advance(world: &mut World) {
    if let Some(mut clock) = world.get_resource_mut::<SimClock>() {
        clock.advance();
    }
}

fn main() {
    // Two camera positions: the edge of the plaza, which is where a player watching a crowd stands,
    // and outside it, where the near band empties and the paused band fills.
    for (label, eye) in [
        ("camera at the plaza edge", PLAZA_RADIUS),
        ("camera 140 m out", 140.0),
    ] {
        println!("\n{label}");
        println!(
            "{:>8}  {:>12}  {:>12}  {:>8}",
            "bodies", "culled ms", "control ms", "saved"
        );
        for count in SIZES {
            let focus = AnimationFocus::at(Vec3::new(eye, 0.0, 0.0));
            let (mut culled_world, mut culled) = crowd(count, BlendLod::default(), focus);
            let (mut control_world, mut control) = crowd(count, BlendLod::disabled(), focus);
            let culled_ms = time(&mut culled_world, &mut culled);
            let control_ms = time(&mut control_world, &mut control);
            let saved = if control_ms > 0.0 {
                100.0 * (control_ms - culled_ms) / control_ms
            } else {
                0.0
            };
            println!("{count:>8}  {culled_ms:>12.3}  {control_ms:>12.3}  {saved:>7.1}%");
            report_levels(&mut culled_world);
        }
    }
}

/// Print how the crowd split across the three levels, so the saving can be read against the split.
fn report_levels(world: &mut World) {
    let (mut full, mut stepped, mut paused) = (0_usize, 0_usize, 0_usize);
    let mut query = world.query::<&AnimationLod>();
    for lod in query.iter(world) {
        match lod.level() {
            LodLevel::Full => full += 1,
            LodLevel::Stepped => stepped += 1,
            LodLevel::Paused => paused += 1,
        }
    }
    println!("          levels: {full} full, {stepped} stepped, {paused} paused");
}
