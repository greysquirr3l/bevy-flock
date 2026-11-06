//! Handing velocities to a physics engine, without depending on one.
//!
//! A crowd simulation and a physics engine want the same thing from an NPC: a
//! position and a velocity. They disagree about who owns them. This crate's answer
//! is a port: [`PhysicsPort`] takes a velocity per NPC per frame, and the game
//! decides what to do with it — push it into Avian, into Rapier, into a custom
//! kinematic solver, or into nothing at all.
//!
//! The library does not ship an Avian adapter, and that is a decision rather than an
//! omission: Avian's Bevy version gate moves faster than this crate's, and a hard
//! dependency would pin every user to whichever pair happened to be compatible the
//! week it was published. [`PhysicsBridge`] with no port attached is a no-op, so the
//! default build pays nothing; a game that wants collisions implements four lines.
//!
//! ## Who integrates?
//!
//! Two arrangements work:
//!
//! * **The crate integrates, physics resolves.** Entities carry [`PhysicsBacked`];
//!   the physics engine moves them and the crate reads positions back. Velocity
//!   export still happens so the engine knows what the crowd *wants*.
//! * **Physics integrates.** The port receives desired velocities and the game's own
//!   system does the integration. The crate's integration system is skipped for
//!   marked entities by the game's own run conditions.

use crate::adapters::primary::components::{Npc, NpcVelocity};
use crate::domain::ids::NpcId;
use bevy_ecs::prelude::*;
use bevy_math::Vec3;

/// Marks an NPC whose transform is owned by an external physics world.
///
/// Purely advisory: nothing in this crate reads it, because whether an entity is
/// integrated here or elsewhere is the game's decision. It exists so a game can hang
/// a run condition off `With<PhysicsBacked>` and so a debug overlay can report which
/// NPCs the crate is not moving.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PhysicsBacked;

/// Something that can take a velocity.
///
/// Object-safe and synchronous: pushing a velocity must be cheap and must not block.
/// An implementation that talks to another thread owns its own channel.
pub trait PhysicsPort: Send + Sync {
    /// Offer this tick's velocity for one NPC.
    fn push(&mut self, npc: NpcId, velocity: Vec3);

    /// Called once at the end of the export pass, so an implementation can flush a
    /// batch or mark a command buffer for submission.
    fn flush(&mut self) {}

    /// A short label for diagnostics.
    fn label(&self) -> &'static str;
}

/// The bridge between the crowd and whichever physics engine is in use.
#[derive(Resource, Default)]
pub struct PhysicsBridge {
    port: Option<Box<dyn PhysicsPort>>,
    pushed: u64,
    flushes: u64,
}

impl core::fmt::Debug for PhysicsBridge {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PhysicsBridge")
            .field("port", &self.label())
            .field("pushed", &self.pushed)
            .field("flushes", &self.flushes)
            .finish_non_exhaustive()
    }
}

impl PhysicsBridge {
    /// A bridge that hands velocities nowhere.
    #[must_use]
    pub fn detached() -> Self {
        Self::default()
    }

    /// A bridge with a port attached.
    #[must_use]
    pub fn with_port(port: Box<dyn PhysicsPort>) -> Self {
        Self {
            port: Some(port),
            pushed: 0,
            flushes: 0,
        }
    }

    /// Whether a port is attached.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.port.is_some()
    }

    /// The port's label, or `"detached"`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        self.port.as_ref().map_or("detached", |port| port.label())
    }

    /// How many velocities have been pushed.
    #[must_use]
    pub const fn pushed(&self) -> u64 {
        self.pushed
    }

    /// How many flushes have happened.
    #[must_use]
    pub const fn flushes(&self) -> u64 {
        self.flushes
    }

    /// Take the port out, for replacement or shutdown.
    pub fn take_port(&mut self) -> Option<Box<dyn PhysicsPort>> {
        self.port.take()
    }
}

/// Push every NPC's velocity to the port, if there is one.
///
/// Runs in `FlockSet::Integration`, after steering and before the crate's own
/// integration: a port that is authoritative wants the desired velocity, not the
/// velocity the crate already integrated.
pub fn export_velocities(mut bridge: ResMut<PhysicsBridge>, npcs: Query<(&Npc, &NpcVelocity)>) {
    let Some(port) = bridge.port.as_mut() else {
        return;
    };
    let mut pushed = 0_u64;
    for (npc, velocity) in &npcs {
        port.push(npc.0, velocity.0);
        pushed = pushed.saturating_add(1);
    }
    port.flush();
    bridge.pushed = bridge.pushed.saturating_add(pushed);
    bridge.flushes = bridge.flushes.saturating_add(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::OrDie;
    use bevy_app::{App, Update};
    use bevy_transform::prelude::Transform;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Recorder {
        pushes: Arc<Mutex<Vec<(NpcId, Vec3)>>>,
        flushes: Arc<Mutex<u32>>,
    }

    impl PhysicsPort for Recorder {
        fn push(&mut self, npc: NpcId, velocity: Vec3) {
            if let Ok(mut pushes) = self.pushes.lock() {
                pushes.push((npc, velocity));
            }
        }

        fn flush(&mut self) {
            if let Ok(mut flushes) = self.flushes.lock() {
                *flushes += 1;
            }
        }

        fn label(&self) -> &'static str {
            "recorder"
        }
    }

    fn app(bridge: PhysicsBridge) -> App {
        let mut app = App::new();
        app.insert_resource(bridge)
            .add_systems(Update, export_velocities);
        app
    }

    #[test]
    fn a_detached_bridge_costs_nothing() {
        let mut app = app(PhysicsBridge::detached());
        app.world_mut().spawn((
            Npc(NpcId::unique(1)),
            NpcVelocity(Vec3::X),
            Transform::default(),
        ));
        app.update();
        let bridge = app.world().resource::<PhysicsBridge>();
        assert!(!bridge.is_attached());
        assert_eq!(bridge.pushed(), 0);
        assert_eq!(bridge.label(), "detached");
    }

    #[test]
    fn an_attached_port_receives_every_velocity_and_one_flush() {
        let recorder = Recorder::default();
        let pushes = Arc::clone(&recorder.pushes);
        let flushes = Arc::clone(&recorder.flushes);
        let mut app = app(PhysicsBridge::with_port(Box::new(recorder)));
        for index in 0..3_u32 {
            app.world_mut().spawn((
                Npc(NpcId::unique(index)),
                NpcVelocity(Vec3::new(index as f32, 0.0, 0.0)),
                Transform::default(),
            ));
        }
        app.update();
        assert_eq!(*flushes.lock().or_die(), 1);
        assert_eq!(pushes.lock().or_die().len(), 3);
        assert_eq!(app.world().resource::<PhysicsBridge>().pushed(), 3);
    }

    #[test]
    fn the_port_can_be_replaced_and_removed() {
        let mut bridge = PhysicsBridge::with_port(Box::new(Recorder::default()));
        assert!(bridge.is_attached());
        let taken = bridge.take_port();
        assert!(taken.is_some());
        assert!(!bridge.is_attached());
    }

    #[test]
    fn the_bridge_reports_what_it_is_attached_to() {
        let bridge = PhysicsBridge::with_port(Box::new(Recorder::default()));
        assert_eq!(bridge.label(), "recorder");
        assert_eq!(bridge.flushes(), 0);
    }
}
