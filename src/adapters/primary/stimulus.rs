//! The ECS half of stimulus perception (ADR 0047).
//!
//! Mark an emitter with [`StimulusSource`] and an observer with [`Perception`]. Each tick the
//! emitters publish into one bounded [`StimulusBuffer`] resource, expired records are swept, and
//! every observer that is due a perception pass writes its nearest, strongest stimuli into
//! [`PerceivedStimuli`] — which is what a behaviour-tree leaf or a utility scorer reads.
//!
//! ```
//! use bevy_app::App;
//! use bevy_ecs::prelude::Commands;
//! use bevy_flock::prelude::*;
//! use bevy_math::Vec3;
//!
//! fn shout(commands: &mut Commands) {
//!     commands.spawn((
//!         StimulusSource::sound(12.0, 0.9),
//!         Transform::from_translation(Vec3::new(4.0, 0.0, 0.0)),
//!     ));
//! }
//!
//! let mut app = App::new();
//! app.add_plugins(StimulusPlugin::default());
//! ```
//!
//! The pass is budgeted by [`PerceptionSettings::interval`]: perception is mid-band work, and an
//! agent that re-reads the buffer every tick is paying for an answer that changes once a second.
//! The tick counter is the simulation's, so a replay perceives on exactly the same ticks.

use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::*;
use bevy_math::Vec3;
use bevy_transform::prelude::Transform;
use smallvec::SmallVec;

use crate::adapters::primary::components::{Npc, NpcVelocity};
use crate::adapters::primary::plugin::FlockSet;
use crate::application::SimClock;
use crate::domain::stimulus::{
    Occluder, OpenAir, Perceived, PerceptionQuery, Stimulus, StimulusBuffer, StimulusError,
    StimulusKind, perceive,
};

/// Something that publishes itself into the world's stimulus buffer every tick it lives.
///
/// Kept separate from the buffer's own [`Stimulus`] because this is the recipe (kind, radius,
/// intensity, lifetime) and that is one record of it at one tick.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct StimulusSource {
    /// The channel.
    pub kind: StimulusKind,
    /// How far it carries.
    pub radius: f32,
    /// How strong it is at the source.
    pub intensity: f32,
    /// How long each pulse stays interesting, in ticks.
    pub ttl: u32,
}

impl StimulusSource {
    /// A source on any channel.
    #[must_use]
    pub const fn new(kind: StimulusKind, radius: f32, intensity: f32, ttl: u32) -> Self {
        Self {
            kind,
            radius,
            intensity,
            ttl,
        }
    }

    /// A sighting that carries 30 m and lasts half a second.
    #[must_use]
    pub const fn sight(radius: f32, intensity: f32) -> Self {
        Self::new(StimulusKind::Sight, radius, intensity, 30)
    }

    /// A sound that carries 45 m and lingers a second and a half.
    #[must_use]
    pub const fn sound(radius: f32, intensity: f32) -> Self {
        Self::new(StimulusKind::Sound, radius, intensity, 90)
    }

    /// One record of this source at `tick`.
    #[must_use]
    pub fn pulse(self, position: Vec3, tick: crate::domain::tick::Tick) -> Stimulus {
        Stimulus::new(self.kind, position, self.radius, self.intensity, tick).with_ttl(self.ttl)
    }
}

/// Marks an NPC as an observer.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Perception;

/// What an observer perceived on its last pass, strongest first.
#[derive(Component, Debug, Clone, Default)]
pub struct PerceivedStimuli(pub SmallVec<[Perceived; 8]>);
impl PerceivedStimuli {
    /// The stimuli, strongest first.
    #[must_use]
    pub fn as_slice(&self) -> &[Perceived] {
        &self.0
    }

    /// The strongest stimulus, if any.
    #[must_use]
    pub fn strongest(&self) -> Option<Perceived> {
        self.0.first().copied()
    }
}

/// The shared buffer and the numbers every observer uses.
#[derive(Resource, Debug)]
pub struct StimulusBufferResource {
    /// The published stimuli.
    pub buffer: StimulusBuffer,
    /// How observers perceive.
    pub query: PerceptionQuery,
    /// How many ticks between perception passes. One means every tick.
    pub interval: u32,
    /// How many records the last sweep retired.
    pub expired: u32,
}

/// How perception is configured.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct PerceptionSettings {
    /// How many stimuli the world buffer holds.
    pub capacity: usize,
    /// Sight, hearing and the observer's memory of them.
    pub query: PerceptionQuery,
    /// Ticks between perception passes. 6 is ten times a second at 60 Hz.
    pub interval: u32,
}

impl PerceptionSettings {
    /// Settings for a buffer of `capacity` records, perceiving every 6 ticks.
    ///
    /// # Errors
    ///
    /// Returns [`StimulusError::Empty`] for a capacity of zero.
    pub fn try_new(capacity: usize) -> Result<Self, StimulusError> {
        // The buffer is the only fallible part of these settings, and it is built by the plugin;
        // validating here keeps the failure at configuration time rather than at first add.
        if capacity == 0 {
            return Err(StimulusError::Empty);
        }
        Ok(Self {
            capacity,
            query: PerceptionQuery::new(),
            interval: 6,
        })
    }
}

impl Default for PerceptionSettings {
    fn default() -> Self {
        Self {
            // 4,096 live stimuli is a village firefight, not a crowd's worth of noise; the buffer
            // is a fixed allocation, so the default is deliberately modest.
            capacity: 4_096,
            query: PerceptionQuery::new(),
            interval: 6,
        }
    }
}

/// Adds stimulus perception.
#[derive(Debug, Clone, Copy, Default)]
pub struct StimulusPlugin {
    /// Buffer size, ranges and cadence.
    pub settings: PerceptionSettings,
}

impl StimulusPlugin {
    /// A plugin with explicit settings.
    #[must_use]
    pub const fn new(settings: PerceptionSettings) -> Self {
        Self { settings }
    }
}

impl Plugin for StimulusPlugin {
    fn build(&self, app: &mut App) {
        let Ok(buffer) = StimulusBuffer::new(self.settings.capacity) else {
            // A zero-capacity buffer is a programming error, and a plugin cannot return an error.
            // Doing nothing leaves the resource absent, and the systems below are gated on it.
            return;
        };
        // A plugin that is added without the flock plugin still needs a clock to stamp and age
        // stimuli. `init_resource` leaves the flock plugin's advancing clock alone.
        app.init_resource::<SimClock>();
        app.insert_resource(self.settings)
            .insert_resource(StimulusBufferResource {
                buffer,
                query: self.settings.query,
                interval: self.settings.interval.max(1),
                expired: 0,
            });
        app.add_systems(
            Update,
            (emit_stimuli, perceive_stimuli)
                .chain()
                .in_set(FlockSet::Perception),
        );
    }
}

/// Publish one pulse per live emitter, then retire the expired records.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are taken by value by construction"
)]
pub fn emit_stimuli(
    clock: Res<SimClock>,
    mut stimuli: ResMut<StimulusBufferResource>,
    emitters: Query<(&Transform, &StimulusSource)>,
) {
    let buffer = stimuli.bypass_change_detection().buffer_mut();
    buffer.retain_live(clock.tick);
    let mut emitters = emitters.iter();
    // Emitters are visited in entity order so a replay publishes in the same order; the buffer is
    // a ring, and the ring's order is the order an observer sees ties resolved in.
    for (transform, source) in emitters.by_ref() {
        buffer.push(source.pulse(transform.translation, clock.tick));
    }
}

impl StimulusBufferResource {
    /// The buffer, mutably, for the emit system. Kept private so a caller cannot forget to sweep.
    fn buffer_mut(&mut self) -> &mut StimulusBuffer {
        &mut self.buffer
    }
}

/// Give every due observer its list of what is nearby.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are taken by value by construction"
)]
pub fn perceive_stimuli(
    clock: Res<SimClock>,
    stimuli: Res<StimulusBufferResource>,
    observers: Query<
        (&Transform, &NpcVelocity, &mut PerceivedStimuli),
        (With<Perception>, With<Npc>),
    >,
) {
    let interval = u64::from(stimuli.interval.max(1));
    if !clock.tick.get().is_multiple_of(interval) {
        return;
    }
    let query = stimuli.query;
    for (transform, velocity, mut perceived) in observers {
        let forward = if velocity.0.length_squared() > f32::EPSILON {
            velocity.0
        } else {
            Vec3::Z
        };
        perceived.0 = perceive(
            &stimuli.buffer,
            transform.translation,
            forward,
            clock.tick,
            &query,
            &OpenAir as &dyn Occluder,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::primary::components::NpcBundle;
    use crate::domain::ids::NpcId;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(StimulusPlugin::default());
        app
    }

    /// One frame, then one tick: `FlockPlugin` owns the advance in a real app, and driving it by
    /// hand here keeps the tests from depending on system order inside `Update`.
    fn tick(app: &mut App) {
        app.update();
        app.world_mut()
            .resource_mut::<crate::application::SimClock>()
            .advance();
    }

    fn spawn_observer(app: &mut App, position: Vec3) -> Entity {
        app.world_mut()
            .spawn(NpcBundle::default_for(NpcId::unique(0)).with_position(position))
            .insert(Perception)
            .insert(PerceivedStimuli::default())
            .id()
    }

    fn spawn_emitter(app: &mut App, position: Vec3, source: StimulusSource) -> Entity {
        app.world_mut()
            .spawn((source, Transform::from_translation(position)))
            .id()
    }

    #[test]
    fn an_emitter_is_perceived_by_a_nearby_observer() {
        let mut app = app();
        let observer = spawn_observer(&mut app, Vec3::ZERO);
        spawn_emitter(
            &mut app,
            Vec3::new(0.0, 0.0, 5.0),
            StimulusSource::sight(30.0, 1.0),
        );
        tick(&mut app);
        let perceived = app
            .world()
            .get::<PerceivedStimuli>(observer)
            .expect("component");
        assert_eq!(perceived.0.len(), 1);
        assert_eq!(perceived.0[0].kind, StimulusKind::Sight);
        assert!(perceived.0[0].strength > 0.0);
    }

    #[test]
    fn an_observer_out_of_range_sees_nothing() {
        let mut app = app();
        let observer = spawn_observer(&mut app, Vec3::ZERO);
        spawn_emitter(
            &mut app,
            Vec3::new(0.0, 0.0, 500.0),
            StimulusSource::sight(30.0, 1.0),
        );
        tick(&mut app);
        let perceived = app
            .world()
            .get::<PerceivedStimuli>(observer)
            .expect("component");
        assert!(perceived.0.is_empty());
    }

    #[test]
    fn a_despawned_emitter_stops_being_perceived_once_its_ttl_expires() {
        let mut app = app();
        let observer = spawn_observer(&mut app, Vec3::ZERO);
        let emitter = spawn_emitter(
            &mut app,
            Vec3::new(0.0, 0.0, 5.0),
            StimulusSource::new(StimulusKind::Sound, 30.0, 1.0, 2),
        );
        tick(&mut app);
        assert_eq!(
            app.world()
                .get::<PerceivedStimuli>(observer)
                .expect("component")
                .0
                .len(),
            1
        );
        app.world_mut().despawn(emitter);
        // The observer only re-reads the buffer every sixth tick, so the window has to cover more
        // than the two-tick life of the sound, not just more than two frames.
        for _ in 0..12 {
            tick(&mut app);
        }
        assert!(
            app.world()
                .get::<PerceivedStimuli>(observer)
                .expect("component")
                .0
                .is_empty(),
            "a sound with a two-tick life cannot outlast five"
        );
    }

    #[test]
    fn perception_runs_on_the_configured_interval() {
        let mut app = app();
        let observer = spawn_observer(&mut app, Vec3::ZERO);
        tick(&mut app);
        let seen = app
            .world()
            .get::<PerceivedStimuli>(observer)
            .expect("component")
            .0
            .len();
        assert_eq!(
            seen, 0,
            "tick 0 with an interval of 6 has nothing to see yet"
        );
    }
}
