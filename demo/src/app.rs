//! The playground itself: one object that builds the simulation, places the crowd and steps it.
//!
//! This is a module rather than code inside `main` because the binary and the tests must run *the
//! same* configuration. They did not, once: the binary disabled flock cohesion and the tests used
//! the default, so the tests watched a crowd walk to the origin while the binary's crowd stayed in
//! its districts. Sharing the setup is cheaper than remembering the difference.

use crate::DemoError;
use crate::city::CityLayout;
use crate::scene::Body;
use bevy_app::{App, Update};
use bevy_ecs::prelude::{Entity, Query, Res, ResMut, Resource};
use bevy_ecs::query::QueryState;
use bevy_flock::adapters::primary::cognition::PlayerFocus;
use bevy_flock::adapters::primary::components::{Npc, NpcIdentity, NpcTarget, NpcVelocity};
use bevy_flock::adapters::primary::observability::Diagnostics;
use bevy_flock::adapters::primary::perception::PlayerCharacter;
use bevy_flock::application::AvoidanceParams;
use bevy_flock::domain::ids::SimSeed;
use bevy_flock::domain::population::PopulationSize;
use bevy_flock::domain::spatial::SpatialGridConfig;
use bevy_flock::prelude::{FlockCommands, FlockConfig, FlockPlugin};
use bevy_math::Vec3;
use bevy_transform::prelude::Transform;

/// Where an NPC may decide to walk next: the settlements, the shelters, the wayfinding.
///
/// A crowd with somewhere to go is the whole demonstration. With no target the steering solver has
/// a stable fixed point at "everybody standing still", which is an accurate simulation of a city at
/// four in the morning and a useless demo.
#[derive(Resource, Debug)]
struct Waypoints(Vec<Vec3>);

/// How many retargetings have happened, mixed into the destination draw so a person does not walk
/// the same circuit forever.
#[derive(Resource, Debug, Default)]
struct Retargets(u64);

/// Send anyone who has arrived somewhere else.
fn retarget(
    npcs: Query<(&Npc, &mut NpcTarget, &Transform)>,
    waypoints: Res<Waypoints>,
    mut retargets: ResMut<Retargets>,
) {
    for (npc, mut target, transform) in npcs {
        if transform.translation.distance_squared(target.0) > 16.0 {
            continue;
        }
        retargets.0 = retargets.0.wrapping_add(1);
        let mixed = u64::from(npc.0.to_bits()).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ retargets.0.wrapping_mul(0xD1B5_4A32_D192_ED03);
        if waypoints.0.is_empty() {
            return;
        }
        let index = usize::try_from(mixed % waypoints.0.len() as u64).unwrap_or(0);
        if let Some(destination) = waypoints.0.get(index) {
            target.0 = *destination;
        }
    }
}

/// A city, a crowd in it, and the means to step both.
pub struct Playground {
    /// The Bevy app: the library's plugin plus the demo's retargeting system.
    pub app: App,
    /// The layout the crowd is placed in.
    pub city: CityLayout,
    /// How many NPCs the city was asked for.
    pub population: u32,
    /// Where the visitor stands: the anchor the cognition ladder measures from.
    pub anchor: Vec3,
}

impl Playground {
    /// Build a city and a crowd in it.
    ///
    /// # Errors
    ///
    /// Returns [`DemoError::Config`] when the library refuses the population or the grid.
    pub fn new(seed: u64, population: u32, extent: f32) -> Result<Self, DemoError> {
        let city = CityLayout::generate(SimSeed::new(seed), extent);
        let size = PopulationSize::try_new(population as usize)
            .map_err(|error| DemoError::Config(error.to_string()))?;
        let grid = SpatialGridConfig::try_new(
            Vec3::new(-extent, -8.0, -extent),
            Vec3::new(extent, 24.0, extent),
            // The grid coarsens itself if this is too fine for the volume. Twelve metres is roughly
            // a city-block bucket at this extent, which is the density the steering kernel wants.
            12.0,
        )
        .map_err(|error| DemoError::Config(error.to_string()))?;

        // A crowd with cohesion enabled is a flock, and a flock has a centre: run one in a city and
        // everybody walks to the same intersection. A street needs separation and a shared pace
        // instead, so the crowd holds its districts. This is the demo's steering opinion, not a
        // library default.
        let mut params = FlockConfig::default().boids.params();
        // The crowd preset weights separation and alignment and leaves seek at zero, which is right
        // for a flock and wrong for a city: a street full of people is going somewhere. Cohesion
        // off (or everybody walks to one intersection), seek on (or nobody walks at all).
        params.weights.cohesion = 0.0;
        params.weights.seek = 1.0;
        params.weights.arrive = 0.5;
        let boids = FlockConfig::default()
            .boids
            .with_grid(grid)
            .with_params(params)
            .map_err(|error| DemoError::Config(error.to_string()))?
            .with_avoidance(Some(AvoidanceParams::default()));

        let mut config = FlockConfig::default()
            .with_population(size)
            .with_seed(SimSeed::new(seed))
            .with_spawn_budget(2_000);
        config.boids = boids;

        let mut app = App::new();
        // `App::new()` has no clock. `integrate_flock` advances positions by the frame delta, so a
        // headless app without `TimePlugin` integrates at zero and the crowd never moves — which is
        // exactly what this demo did until a test caught it.
        app.add_plugins(bevy_time::TimePlugin);
        app.add_plugins(FlockPlugin::new(config));

        // The ladder promotes whoever is near something worth caring about, and a city with nobody
        // in it promotes nobody. One visitor standing in the largest settlement is enough: it is
        // the anchor the brief's traveller would be.
        let anchor = city
            .district("gate-row")
            .map_or(Vec3::ZERO, |district| district.centre);
        app.world_mut().spawn((
            PlayerFocus,
            PlayerCharacter,
            Transform::from_translation(anchor),
        ));

        let mut waypoints: Vec<Vec3> = city.districts.iter().map(|d| d.centre).collect();
        waypoints.extend(city.shelters.iter().map(|shelter| shelter.position));
        waypoints.extend(city.pylons.iter().map(|pylon| pylon.position));
        app.insert_resource(Waypoints(waypoints));
        app.init_resource::<Retargets>();
        app.add_systems(Update, retarget);

        Ok(Self {
            app,
            city,
            population,
            anchor,
        })
    }

    /// Step until the spawn queue drains, returning how many ticks it took.
    pub fn drain_spawn(&mut self) -> u64 {
        let mut ticks = 0_u64;
        // A ceiling rather than a `loop`: a spawn that never drains is a bug the demo should
        // report, not hang on.
        while ticks < 10_000 {
            self.app.update();
            ticks += 1;
            if self.app.flock_stats().spawn_complete() {
                break;
            }
        }
        ticks
    }

    /// Move every NPC into the district that owns it, and give it somewhere to walk.
    ///
    /// A game places its crowd with its own spawn system; `Placement` is the library's convenience
    /// and knows nothing about districts. This is the demo doing the world-building the library
    /// deliberately does not do.
    pub fn place_crowd(&mut self) {
        let city = self.city.clone();
        let population = self.population;
        let mut query: QueryState<(Entity, &Npc, &mut Transform)> = self.app.world_mut().query();
        let world = self.app.world_mut();
        let mut assignments: Vec<(Entity, Vec3)> = Vec::new();
        for (slot, (entity, npc, mut transform)) in query.iter_mut(world).enumerate() {
            let slot = u32::try_from(slot).unwrap_or(0);
            let district = city.district_for(slot, population);
            let position = city.npc_position(district, npc.0);
            transform.translation = position;
            // Somewhere to be: another settlement. Walking across the city is what makes the
            // avoidance pass and the steering road do anything at all.
            let next = (slot as usize).wrapping_add(3) % city.districts.len().max(1);
            let destination = city
                .districts
                .get(next)
                .map_or(position, |area| area.centre);
            assignments.push((entity, destination));
        }
        for (entity, destination) in assignments {
            world.entity_mut(entity).insert(NpcTarget(destination));
        }
    }

    /// Step the simulation.
    pub fn tick(&mut self, ticks: u64) {
        for _ in 0..ticks {
            self.app.update();
        }
    }

    /// The library's own report.
    #[must_use]
    pub fn diagnostics(&self) -> Diagnostics {
        self.app.world().resource::<Diagnostics>().clone()
    }

    /// NPCs alive.
    #[must_use]
    pub fn live(&self) -> u32 {
        self.app.flock_stats().live
    }

    /// Read the crowd back out as bodies a renderer could draw.
    ///
    /// Sorted by identity, so the artefacts do not depend on the order the ECS happened to iterate:
    /// that is the difference between a diffable scene and a noisy one.
    #[must_use]
    pub fn bodies(&mut self) -> Vec<Body> {
        let mut query: QueryState<(&Npc, &NpcIdentity, &NpcVelocity, &Transform)> =
            self.app.world_mut().query();
        let world = self.app.world_mut();
        let mut collected: Vec<(u32, Body)> = query
            .iter(world)
            .map(|(npc, identity, velocity, transform)| {
                let yaw = if velocity.0.length_squared() > 1e-6 {
                    velocity.0.x.atan2(velocity.0.z)
                } else {
                    0.0
                };
                (
                    npc.0.to_bits(),
                    Body {
                        archetype: identity.archetype,
                        position: transform.translation,
                        yaw,
                        appearance_seed: identity.appearance_seed,
                    },
                )
            })
            .collect();
        collected.sort_by_key(|(id, _)| *id);
        collected.into_iter().map(|(_, body)| body).collect()
    }
}
